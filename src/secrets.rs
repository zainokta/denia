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

    fn validate_secret_path(&self, path: &std::path::Path) -> Result<(), SecretError> {
        let secrets_dir = self.data_dir.join("secrets");
        match (secrets_dir.canonicalize(), path.canonicalize()) {
            (Ok(canonical_dir), Ok(canonical_path)) => {
                if !canonical_path.starts_with(&canonical_dir) {
                    return Err(SecretError::PathTraversal);
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    pub async fn decrypt(
        &self,
        runner: &dyn CommandRunner,
        sops_binary: &std::path::Path,
        project_id: uuid::Uuid,
        secret_ref: &SecretRef,
    ) -> Result<SecretPayload, SecretError> {
        let secret_path = self.secret_path(project_id, secret_ref);
        self.validate_secret_path(&secret_path)?;
        let secret_path = secret_path.to_string_lossy();
        let sops_binary = sops_binary.to_string_lossy();
        let output = runner
            .run(
                &sops_binary,
                &["--decrypt", "--output-type", "json", secret_path.as_ref()],
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
    async fn secret_ref_generate_produces_parseable_ref() {
        let r = SecretRef::generate("registry");
        assert!(r.as_str().starts_with("registry-"));
        let reparsed = SecretRef::parse(r.as_str().to_string()).expect("parse generated ref");
        assert_eq!(reparsed.as_str(), r.as_str());
    }
}
