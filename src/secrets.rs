use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::command::{CommandError, CommandRunner};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SecretRef(String);

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SecretRefError {
    #[error("secret reference cannot be empty")]
    Empty,
    #[error("secret reference must contain only ASCII letters, digits, dot, underscore, or dash")]
    InvalidCharacters,
}

impl SecretRef {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn parse(value: impl Into<String>) -> Result<Self, SecretRefError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(SecretRefError::Empty);
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(SecretRefError::InvalidCharacters);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn file_stem(&self) -> String {
        self.0
            .bytes()
            .map(|byte| {
                if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-') {
                    byte as char
                } else {
                    '_'
                }
            })
            .collect()
    }

    /// Generate a fresh SOPS-friendly ref name. Used by the API when the
    /// caller supplies an inline payload instead of a ref name.
    pub fn generate(prefix: &str) -> Self {
        let id = uuid::Uuid::now_v7();
        Self(format!("{}-{}", prefix, id.simple()))
    }
}

impl<'de> Deserialize<'de> for SecretRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretPayload {
    pub value: String,
}

impl SecretPayload {
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
        }
    }
}

#[derive(Debug, Error)]
pub enum SecretError {
    #[error(transparent)]
    Command(#[from] CommandError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("secret path traversal detected")]
    PathTraversal,
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SopsSecretStore {
    data_dir: std::path::PathBuf,
}

impl SopsSecretStore {
    pub fn new(data_dir: impl Into<std::path::PathBuf>) -> Self {
        Self {
            data_dir: data_dir.into(),
        }
    }

    /// Resolve the on-disk SOPS file for a secret reference *within a project's
    /// namespace*. Secrets live under `secrets/<project_id>/<ref>.sops.yaml`, so
    /// a deployment can only ever decrypt secrets belonging to its own project —
    /// references are authorized by construction, not by global name (F-2).
    pub fn secret_path(
        &self,
        project_id: uuid::Uuid,
        secret_ref: &SecretRef,
    ) -> std::path::PathBuf {
        self.data_dir
            .join("secrets")
            .join(project_id.to_string())
            .join(format!("{}.sops.yaml", secret_ref.file_stem()))
    }

    /// Reject any resolved secret path that escapes `<data_dir>/secrets`.
    ///
    /// This guard is meaningful on the *creation* path, before the file exists:
    /// it walks the lexical path components and refuses `..` / root-absolute
    /// segments, then confirms the result is still contained under the secrets
    /// dir by string prefix. `SecretRef::parse` + `file_stem` already strip any
    /// `/`, `..`, or NUL, so this is defense-in-depth — but unlike the previous
    /// canonicalize-or-`Ok` check, it actually enforces containment when the
    /// target does not yet resolve on disk. When both paths *do* canonicalize
    /// (a re-encrypt over an existing file, or decrypt), the stronger
    /// symlink-resolving check is applied as well.
    fn validate_secret_path(&self, path: &std::path::Path) -> Result<(), SecretError> {
        use std::path::Component;

        let secrets_dir = self.data_dir.join("secrets");

        // Lexical containment: no parent-dir or root escapes, and the path must
        // start with the secrets dir prefix. Holds before the file exists.
        let mut normalized = std::path::PathBuf::new();
        for component in path.components() {
            match component {
                Component::ParentDir => return Err(SecretError::PathTraversal),
                Component::Prefix(_) | Component::RootDir if !normalized.as_os_str().is_empty() => {
                    return Err(SecretError::PathTraversal);
                }
                other => normalized.push(other.as_os_str()),
            }
        }
        if !normalized.starts_with(&secrets_dir) {
            return Err(SecretError::PathTraversal);
        }

        // Strongest check when both ends resolve (existing file / symlinks).
        if let (Ok(canonical_dir), Ok(canonical_path)) =
            (secrets_dir.canonicalize(), path.canonicalize())
            && !canonical_path.starts_with(&canonical_dir)
        {
            return Err(SecretError::PathTraversal);
        }

        Ok(())
    }

    pub async fn decrypt(
        &self,
        runner: &dyn CommandRunner,
        sops_binary: &std::path::Path,
        age_key_file: &std::path::Path,
        project_id: uuid::Uuid,
        secret_ref: &SecretRef,
    ) -> Result<SecretPayload, SecretError> {
        let secret_path = self.secret_path(project_id, secret_ref);
        self.validate_secret_path(&secret_path)?;
        let secret_path = secret_path.to_string_lossy();
        let sops_binary = sops_binary.to_string_lossy();
        let age_key_file = age_key_file.to_string_lossy();
        let output = runner
            .run_env(
                &sops_binary,
                &["--decrypt", "--output-type", "json", secret_path.as_ref()],
                &[("SOPS_AGE_KEY_FILE", age_key_file.as_ref())],
            )
            .await?;

        Ok(serde_json::from_str(&output.stdout)?)
    }

    /// Encrypt `payload` and write the SOPS YAML to
    /// `<data_dir>/secrets/<project_id>/<ref>.sops.yaml` with mode `0600`.
    ///
    /// Plaintext lives only:
    /// - in memory inside this function,
    /// - briefly in a `0600` temp file in the same directory (deleted before
    ///   return, including on encrypt failure).
    pub async fn encrypt(
        &self,
        runner: &dyn CommandRunner,
        sops_binary: &std::path::Path,
        age_recipient: &str,
        project_id: uuid::Uuid,
        secret_ref: &SecretRef,
        payload: &SecretPayload,
    ) -> Result<(), SecretError> {
        let target = self.secret_path(project_id, secret_ref);
        self.validate_secret_path(&target)?;
        let parent = target.parent().expect("secret_path always has parent");
        tokio::fs::create_dir_all(parent).await?;
        set_dir_permissions_700(parent)?;

        let plaintext = serde_json::to_vec(payload)?;
        let plain_name = format!(".{}.{}.json", secret_ref.file_stem(), std::process::id());
        let plain_path = parent.join(plain_name);

        write_file_mode(&plain_path, &plaintext, 0o600).await?;

        let result = async {
            let sops_s = sops_binary.to_string_lossy();
            let plain_s = plain_path.to_string_lossy();
            let out = runner
                .run(
                    &sops_s,
                    &[
                        "--encrypt",
                        "--age",
                        age_recipient,
                        "--input-type",
                        "json",
                        "--output-type",
                        "yaml",
                        plain_s.as_ref(),
                    ],
                )
                .await?;
            write_file_mode(&target, out.stdout.as_bytes(), 0o600).await?;
            Ok::<_, SecretError>(())
        }
        .await;

        // Always remove plaintext, even on failure.
        let _ = tokio::fs::remove_file(&plain_path).await;
        result
    }

    /// Delete the on-disk SOPS file for a secret reference, if present.
    ///
    /// Used when a registry's credentials are cleared (auth → Anonymous) so the
    /// previously-encrypted material does not linger on disk. A missing file is
    /// not an error (idempotent). The path is validated against the secrets
    /// root to refuse traversal, mirroring `decrypt`.
    pub async fn delete(
        &self,
        project_id: uuid::Uuid,
        secret_ref: &SecretRef,
    ) -> Result<(), SecretError> {
        let target = self.secret_path(project_id, secret_ref);
        // `validate_secret_path` canonicalizes; only enforce when the file
        // actually exists (canonicalize fails on missing paths).
        if target.exists() {
            self.validate_secret_path(&target)?;
            match tokio::fs::remove_file(&target).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(SecretError::Io(e)),
            }
        }
        Ok(())
    }
}

#[cfg(unix)]
async fn write_file_mode(
    path: &std::path::Path,
    bytes: &[u8],
    mode: u32,
) -> Result<(), SecretError> {
    use tokio::io::AsyncWriteExt;
    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(mode)
        .open(path)
        .await?;
    f.write_all(bytes).await?;
    f.flush().await?;
    Ok(())
}

#[cfg(unix)]
fn set_dir_permissions_700(path: &std::path::Path) -> Result<(), SecretError> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o700);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

#[cfg(test)]
mod encrypt_tests {
    use super::*;
    use crate::command::{CommandOutput, FakeCommandRunner};
    use tempfile::tempdir;
    use uuid::Uuid;

    #[tokio::test]
    async fn encrypt_writes_sops_yaml_with_age_recipient() {
        let dir = tempdir().unwrap();
        let store = SopsSecretStore::new(dir.path());
        let pid = Uuid::now_v7();
        let secret_ref = SecretRef::parse("test-ref").unwrap();

        let fake_yaml = "data: ENC[AES256_GCM,...]\nsops:\n  age: []\n";
        let runner = FakeCommandRunner::new(vec![CommandOutput {
            status: 0,
            stdout: fake_yaml.to_string(),
            stderr: String::new(),
        }]);

        store
            .encrypt(
                &runner,
                std::path::Path::new("sops"),
                "age1qy0testrecipient",
                pid,
                &secret_ref,
                &SecretPayload::new("alice:s3cret"),
            )
            .await
            .expect("encrypt ok");

        let target = store.secret_path(pid, &secret_ref);
        let written = std::fs::read_to_string(&target).expect("encrypted file written");
        assert_eq!(written, fake_yaml);

        let leftovers: Vec<_> = std::fs::read_dir(target.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("json"))
            .collect();
        assert!(leftovers.is_empty(), "plaintext temp not cleaned");

        let cmd = runner.commands();
        assert_eq!(cmd.len(), 1);
        assert!(
            cmd[0].starts_with(
                "sops --encrypt --age age1qy0testrecipient --input-type json --output-type yaml "
            ),
            "got: {}",
            cmd[0]
        );
    }

    #[tokio::test]
    async fn encrypt_propagates_command_failure_and_cleans_plaintext() {
        let dir = tempdir().unwrap();
        let store = SopsSecretStore::new(dir.path());
        let pid = Uuid::now_v7();
        let secret_ref = SecretRef::parse("fail-ref").unwrap();

        let runner = FakeCommandRunner::new(vec![]); // no outputs -> NoFakeOutput

        let err = store
            .encrypt(
                &runner,
                std::path::Path::new("sops"),
                "age1qy0recipient",
                pid,
                &secret_ref,
                &SecretPayload::new("x"),
            )
            .await
            .expect_err("expected encrypt failure");
        assert!(matches!(err, SecretError::Command(_)));

        let target = store.secret_path(pid, &secret_ref);
        let parent = target.parent().unwrap();
        assert!(parent.exists(), "parent dir was created");
        let leftovers: Vec<_> = std::fs::read_dir(parent).unwrap().collect();
        assert!(
            leftovers.iter().all(|e| {
                let p = e.as_ref().unwrap().path();
                p.extension().and_then(|s| s.to_str()) != Some("json")
            }),
            "plaintext .json left behind on encrypt failure"
        );
    }

    #[tokio::test]
    async fn delete_removes_existing_secret_and_is_idempotent() {
        let dir = tempdir().unwrap();
        let store = SopsSecretStore::new(dir.path());
        let pid = Uuid::now_v7();
        let secret_ref = SecretRef::parse("registry-cred").unwrap();

        let fake_yaml = "data: ENC[AES256_GCM,...]\n";
        let runner = FakeCommandRunner::new(vec![CommandOutput {
            status: 0,
            stdout: fake_yaml.to_string(),
            stderr: String::new(),
        }]);
        store
            .encrypt(
                &runner,
                std::path::Path::new("sops"),
                "age1qy0testrecipient",
                pid,
                &secret_ref,
                &SecretPayload::new("u:p"),
            )
            .await
            .unwrap();
        let target = store.secret_path(pid, &secret_ref);
        assert!(target.exists());

        store.delete(pid, &secret_ref).await.expect("delete ok");
        assert!(!target.exists(), "secret file should be removed");

        // Idempotent: deleting again is not an error.
        store
            .delete(pid, &secret_ref)
            .await
            .expect("second delete is a no-op");
    }

    #[tokio::test]
    async fn secret_ref_generate_produces_parseable_ref() {
        let r = SecretRef::generate("registry");
        assert!(r.as_str().starts_with("registry-"));
        let reparsed = SecretRef::parse(r.as_str().to_string()).expect("parse generated ref");
        assert_eq!(reparsed.as_str(), r.as_str());
    }

    #[test]
    fn validate_secret_path_accepts_in_namespace_path_before_file_exists() {
        let dir = tempdir().unwrap();
        let store = SopsSecretStore::new(dir.path());
        let pid = Uuid::now_v7();
        let secret_ref = SecretRef::parse("ok-ref").unwrap();
        // The target does not exist yet (creation path); the lexical check must
        // still accept a properly namespaced path.
        let target = store.secret_path(pid, &secret_ref);
        store
            .validate_secret_path(&target)
            .expect("in-namespace path accepted before the file exists");
    }

    #[test]
    fn validate_secret_path_rejects_parent_dir_escape() {
        let dir = tempdir().unwrap();
        let store = SopsSecretStore::new(dir.path());
        // A hand-built path that climbs out of the secrets dir must be rejected
        // even though the file does not exist (the canonicalize fallback would
        // have silently allowed it before).
        let escape = dir
            .path()
            .join("secrets")
            .join("..")
            .join("..")
            .join("etc")
            .join("passwd.sops.yaml");
        let err = store
            .validate_secret_path(&escape)
            .expect_err("parent-dir traversal must be rejected");
        assert!(matches!(err, SecretError::PathTraversal));
    }

    #[test]
    fn validate_secret_path_rejects_outside_secrets_dir() {
        let dir = tempdir().unwrap();
        let store = SopsSecretStore::new(dir.path());
        // Absolute path that is not under <data_dir>/secrets at all.
        let outside = std::path::Path::new("/tmp/elsewhere/leak.sops.yaml");
        let err = store
            .validate_secret_path(outside)
            .expect_err("path outside secrets dir must be rejected");
        assert!(matches!(err, SecretError::PathTraversal));
    }
}
