# Registry Inline Credentials Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Accept raw registry credentials (`username`/`password` or `token`) on `POST /v1/projects/{pid}/registries` and `PATCH …/{rid}`. The backend encrypts the payload with SOPS+age into `secrets/<project_id>/<ref>.sops.yaml` and persists a server-generated `credential_ref` on the registry row. The legacy `secret_ref` API field is removed.

**Architecture:**
- Add a new `SopsSecretStore::encrypt` method that writes the SOPS-encrypted YAML using `DENIA_AGE_RECIPIENT` and the existing `sops` binary, via the `CommandRunner` trait.
- Replace the registry `RegistryInput` JSON shape: a tagged enum keyed on `auth_kind`, carrying inline fields per variant. Handler derives a `SecretRef` (UUIDv7-based), encrypts the payload, then stores the `Registry` with that ref.
- Frontend form replaces the "Secret reference" string field with auth-kind-specific inputs (username+password for basic; token for token/ECR/GAR; nothing for anonymous).
- New ADR-021 captures the secret-encryption ownership shift (operator-owned → control-plane-owned).

**Tech Stack:** Rust 2024, axum, rusqlite, SOPS (binary), age recipient, React (web/src), TypeScript.

---

## Pre-flight

### Worktree
This plan should be executed in an isolated worktree (see superpowers:using-git-worktrees). Branch suggestion: `registry-inline-credentials`.

### Files Map

**Create:**
- `docs/adr/021-control-plane-secret-encryption.md`

**Modify:**
- `docs/adr/README.md` — register ADR-021.
- `src/config.rs` — add `age_recipient: Option<String>` field, `DENIA_AGE_RECIPIENT` env, `for_test` default.
- `src/secrets.rs` — add `SecretError::MissingAgeRecipient`, `SopsSecretStore::encrypt`, `SecretRef::generate` (UUIDv7-derived).
- `src/api/registries.rs` — replace `RegistryInput` with tagged enum; encrypt + persist in create/update.
- `src/api/error.rs` — map `SecretError` → 500/400 mapping.
- `tests/backend_contract.rs` — rewrite the registry CRUD test to send inline payload + `FakeCommandRunner` + tempdir.
- `web/src/routes/.../registries-form.tsx` (or equivalent) — replace `secret_ref` field with per-auth-kind fields.
- `web/src/lib/api/registries.ts` (or equivalent) — update request type.

**Test:**
- `src/secrets.rs` — unit test for `encrypt` writing expected sops command + file.
- `tests/backend_contract.rs` — covers POST with inline payload, PATCH update, rejection of legacy `secret_ref`, rejection of mismatched payload (e.g. basic without password).

---

## Task 1: ADR-021 Control-Plane Secret Encryption

**Files:**
- Create: `docs/adr/021-control-plane-secret-encryption.md`
- Modify: `docs/adr/README.md` (append index row)

- [ ] **Step 1: Write ADR-021**

```markdown
# ADR-021: Control-Plane SOPS Secret Encryption

- Status: Accepted
- Date: 2026-05-28
- Supersedes (in part): operator-managed `.sops.yaml` workflow for registry credentials.

## Context

Until now, Denia only *decrypts* SOPS-encrypted files at deploy time; operators
must place `secrets/<project_id>/<ref>.sops.yaml` on disk out-of-band and POST
only an opaque `secret_ref` string to the API. This breaks UX: the web console
asks operators to type a "secret reference" with no way to enter the actual
credential. Frontend users have repeatedly sent the credential payload itself
in the `secret_ref` field, which then fails validation.

## Decision

The control plane owns SOPS encryption for registry credentials:

1. Add `DENIA_AGE_RECIPIENT` env (age public key). `denia` refuses to start
   when registry creation is attempted without it.
2. `POST /v1/projects/{pid}/registries` and `PATCH …/{rid}` accept the raw
   payload (`username`/`password` or `token`) instead of `secret_ref`.
3. The handler generates a `SecretRef` deterministically from a UUIDv7,
   encrypts the payload with `sops --encrypt --age $RECIPIENT --input-type
   json --output-type yaml`, and writes
   `<data_dir>/secrets/<project_id>/<ref>.sops.yaml` with mode `0600`.
4. The previously documented operator-managed `.sops.yaml` flow is retired
   for registry credentials. Existing service-secret refs (SSH deploy keys,
   etc.) remain operator-managed for now; their migration is out of scope.

## Consequences

- Easier: end-to-end UX from web console; no out-of-band file shuffling.
- Easier: per-project namespacing remains by construction.
- Harder: control plane now needs filesystem write access to `secrets/`
  (already true for data_dir). Plaintext briefly transits the `sops` binary;
  payload is written to a `0600` temp file in the same secrets dir before
  `sops --encrypt` is invoked, then deleted.
- Harder: bootstrap docs must instruct operators to set both
  `DENIA_AGE_RECIPIENT` (encryption) and `SOPS_AGE_KEY_FILE` (decryption).

## Alternatives Considered

- Derive recipient from `SOPS_AGE_KEY_FILE` at boot — rejected: adds an `age`
  crate dependency just for public-key derivation.
- `.sops.yaml` creation rules — rejected: operator still has to manage a
  separate config file; not simpler than one env var.
- Frontend pre-creates credential then references it — rejected: doubles
  the API surface, still requires backend encryption.

## References

- [ADR-001 Initial Backend Architecture](001-initial-backend-architecture.md)
- [`src/secrets.rs`](../../src/secrets.rs)
```

- [ ] **Step 2: Append index row to `docs/adr/README.md`**

```markdown
| [021](021-control-plane-secret-encryption.md) | Control-Plane SOPS Secret Encryption | Accepted | 2026-05-28 |
```

- [ ] **Step 3: Commit**

```bash
git add docs/adr/021-control-plane-secret-encryption.md docs/adr/README.md
git commit -m "docs(adr): ADR-021 control-plane SOPS secret encryption"
```

---

## Task 2: Config — `DENIA_AGE_RECIPIENT`

**Files:**
- Modify: `src/config.rs:37-69` (AppConfig struct), `src/config.rs:86-194` (`from_env`), `src/config.rs:196-230` (`for_test`)

- [ ] **Step 1: Write failing config test**

Append to the `#[cfg(test)] mod tests` block at the bottom of `src/config.rs`:

```rust
#[test]
fn age_recipient_parsed_from_env() {
    let guard = EnvGuard::set("DENIA_AGE_RECIPIENT", "age1qy0testrecipient");
    let admin = EnvGuard::set("DENIA_ADMIN_TOKEN", "x".repeat(64));
    let cfg = AppConfig::from_env().expect("config from env");
    assert_eq!(cfg.age_recipient.as_deref(), Some("age1qy0testrecipient"));
    drop(guard);
    drop(admin);
}

#[test]
fn age_recipient_absent_when_unset() {
    let admin = EnvGuard::set("DENIA_ADMIN_TOKEN", "x".repeat(64));
    // Ensure unset:
    unsafe { std::env::remove_var("DENIA_AGE_RECIPIENT"); }
    let cfg = AppConfig::from_env().expect("config from env");
    assert!(cfg.age_recipient.is_none());
    drop(admin);
}

struct EnvGuard {
    key: &'static str,
    prior: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, val: impl AsRef<str>) -> Self {
        let prior = std::env::var(key).ok();
        unsafe { std::env::set_var(key, val.as_ref()); }
        Self { key, prior }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.prior {
            Some(v) => unsafe { std::env::set_var(self.key, v); },
            None => unsafe { std::env::remove_var(self.key); },
        }
    }
}
```

> Note: env tests may race with other tests touching the same vars; if so, mark `#[serial_test::serial]` (skip if crate not in deps — alternative is the existing dedicated `#[ignore]` integration test pattern).

- [ ] **Step 2: Run failing tests**

```
cargo test --lib config:: -- --nocapture
```

Expected: `error[E0609]: no field 'age_recipient' on type 'AppConfig'`.

- [ ] **Step 3: Add field + env parse + for_test default**

Add to `AppConfig` (after `tls_dir`):

```rust
    /// Age public key used to encrypt control-plane-managed secrets (registry
    /// credentials, etc.). Required at the point of first encryption; absence
    /// is reported as a 400/500 at API time, not at boot. See ADR-021.
    pub age_recipient: Option<String>,
```

In `AppConfig::from_env`, parse and include the field:

```rust
        let age_recipient = env::var("DENIA_AGE_RECIPIENT")
            .ok()
            .filter(|v| !v.trim().is_empty());
```

Add it to the `Ok(Self { … })` literal alongside `tls_dir`.

In `AppConfig::for_test`, add the field with `age_recipient: Some("age1test".into()),` so tests that exercise encryption have a recipient available.

- [ ] **Step 4: Run tests**

```
cargo test --lib config:: -- --nocapture
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat(config): add DENIA_AGE_RECIPIENT for control-plane encryption"
```

---

## Task 3: SOPS encryption in `SecretStore`

**Files:**
- Modify: `src/secrets.rs` (add encrypt path, error variants, `SecretRef::generate`).

- [ ] **Step 1: Write failing unit test**

Append to `src/secrets.rs` `#[cfg(test)] mod tests`:

```rust
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

        // Plaintext temp file was cleaned up.
        let plain_glob: Vec<_> = std::fs::read_dir(target.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("json"))
            .collect();
        assert!(plain_glob.is_empty(), "plaintext temp not cleaned");

        let cmd = runner.commands();
        assert_eq!(cmd.len(), 1);
        assert!(cmd[0].starts_with("sops --encrypt --age age1qy0testrecipient --input-type json --output-type yaml "));
        assert!(cmd[0].contains(&pid.to_string()));
    }

    #[tokio::test]
    async fn encrypt_propagates_command_failure() {
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

        // Failed encrypt MUST NOT leave plaintext on disk.
        let target = store.secret_path(pid, &secret_ref);
        let parent = target.parent().unwrap();
        if parent.exists() {
            let leftovers: Vec<_> = std::fs::read_dir(parent).unwrap().collect();
            assert!(leftovers.is_empty(), "encrypt failed but left files behind");
        }
    }
}
```

> If `tempfile` is not yet in `[dev-dependencies]`, add it (cheap, used by other tests already — verify with `grep tempfile Cargo.toml` first; if absent, append `tempfile = "3"` under `[dev-dependencies]`).

- [ ] **Step 2: Run failing tests**

```
cargo test --lib secrets::encrypt_tests -- --nocapture
```

Expected: compile error — `SopsSecretStore::encrypt` not defined.

- [ ] **Step 3: Implement `encrypt`**

Add to `impl SopsSecretStore` in `src/secrets.rs`:

```rust
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

        let result = (async {
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
        })
        .await;

        // Always remove plaintext, even on failure.
        let _ = tokio::fs::remove_file(&plain_path).await;
        result
    }
```

And the small helpers (at module scope, below the impl):

```rust
#[cfg(unix)]
async fn write_file_mode(
    path: &std::path::Path,
    bytes: &[u8],
    mode: u32,
) -> Result<(), SecretError> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = tokio::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(mode)
        .open(path)
        .await?;
    use tokio::io::AsyncWriteExt;
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
```

Extend `SecretError`:

```rust
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
```

And `SecretRef::generate`:

```rust
    /// Generate a fresh SOPS-friendly ref name. Used by the API when the
    /// caller supplies an inline payload instead of a ref name.
    pub fn generate(prefix: &str) -> Self {
        let id = uuid::Uuid::now_v7();
        Self(format!("{}-{}", prefix, id.simple()))
    }
```

- [ ] **Step 4: Run tests**

```
cargo test --lib secrets:: -- --nocapture
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/secrets.rs Cargo.toml
git commit -m "feat(secrets): control-plane SOPS encryption via SopsSecretStore::encrypt"
```

---

## Task 4: Registry API — inline-payload input

**Files:**
- Modify: `src/api/registries.rs` (handlers + input struct)
- Modify: `src/app.rs` if a new method on `AppState` is needed (probably not — `command_runner` and `config` are already accessible).

- [ ] **Step 1: Write failing API test (in `tests/backend_contract.rs`)**

Locate the existing `registry_api_admin_can_crud_no_credential_leak` test (~`tests/backend_contract.rs:923`). Replace it with the version below — same name, new shape:

```rust
#[tokio::test]
async fn registry_api_admin_can_crud_no_credential_leak() {
    use tempfile::tempdir;
    use denia::command::{CommandOutput, FakeCommandRunner};

    let tmp = tempdir().expect("tempdir");
    let mut cfg = AppConfig::for_test("test-token");
    cfg.data_dir = tmp.path().to_path_buf();
    cfg.age_recipient = Some("age1test".to_string());
    let store = SqliteStore::open_in_memory().expect("open sqlite");
    store.migrate().expect("migrate");

    let fake_yaml_create = "data: ENC[AES256_GCM,...create]\n".to_string();
    let fake_yaml_patch = "data: ENC[AES256_GCM,...patch]\n".to_string();
    let runner = FakeCommandRunner::new(vec![
        CommandOutput { status: 0, stdout: fake_yaml_create.clone(), stderr: String::new() },
        CommandOutput { status: 0, stdout: fake_yaml_patch.clone(), stderr: String::new() },
    ]);
    let ingress = std::sync::Arc::new(denia::ingress::state::IngressState::default());
    let state = AppState::new_with_deploy_dependencies(
        cfg.clone(),
        &store,
        denia::runtime::FakeRuntime::default(),
        denia::health::FakeHealthChecker::healthy(),
        runner.clone(),
        ingress,
    );
    let app = build_router(state);
    let project = create_project_for_test(&store, "p1");

    // POST
    let body = serde_json::json!({
        "name": "ghcr",
        "endpoint": "ghcr.io",
        "auth_kind": "basic",
        "username": "zainokta",
        "password": "example-redacted-token"
    });
    let response = app
        .clone()
        .oneshot(/* same builder as before, POST /v1/projects/{pid}/registries */)
        .await
        .unwrap();
    assert_eq!(response.status(), http::StatusCode::CREATED);
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let registry_id_str = value["id"].as_str().unwrap();
    let registry_id = Uuid::parse_str(registry_id_str).unwrap();
    let credential_ref = value["credential_ref"].as_str().expect("ref returned");
    assert!(
        credential_ref.starts_with("registry-"),
        "ref should be generated: {credential_ref}"
    );
    // Body must not echo plaintext.
    let body_text = String::from_utf8(bytes.to_vec()).unwrap();
    for needle in ["password", "username", "example-redacted-token-prefix", "zainokta"] {
        assert!(
            !body_text.contains(needle),
            "response leaks credential field {needle}: {body_text}"
        );
    }

    // Encrypted file exists and contains fake YAML.
    let secret_path = tmp.path()
        .join("secrets")
        .join(project.id.to_string())
        .join(format!("{credential_ref}.sops.yaml"));
    let written = std::fs::read_to_string(&secret_path).expect("encrypted file");
    assert_eq!(written, fake_yaml_create);

    // PATCH (rotate password)
    let patch_body = serde_json::json!({
        "name": "ghcr-renamed",
        "endpoint": "ghcr.io",
        "auth_kind": "basic",
        "username": "zainokta",
        "password": "example-redacted-token"
    });
    let response = app
        .clone()
        .oneshot(/* PATCH /v1/projects/{pid}/registries/{rid} with patch_body */)
        .await
        .unwrap();
    assert_eq!(response.status(), http::StatusCode::OK);
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["id"].as_str(), Some(registry_id_str));
    assert_eq!(value["name"].as_str(), Some("ghcr-renamed"));
    // PATCH should write the new YAML to disk (same ref name).
    let written = std::fs::read_to_string(&secret_path).expect("rotated encrypted file");
    assert_eq!(written, fake_yaml_patch);

    // Confirms sops was called twice with expected args.
    let cmds = runner.commands();
    assert_eq!(cmds.len(), 2);
    for c in &cmds {
        assert!(c.contains("--encrypt"));
        assert!(c.contains("--age age1test"));
    }
}

#[tokio::test]
async fn registry_api_rejects_legacy_secret_ref_field() {
    let (app, store) = registry_api_test_app();
    let project = create_project_for_test(&store, "p1");
    let body = serde_json::json!({
        "name": "ghcr",
        "endpoint": "ghcr.io",
        "auth_kind": "basic",
        "secret_ref": "ghcr-token"
    });
    let resp = app
        .oneshot(/* POST */)
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn registry_api_rejects_basic_without_password() {
    let (app, store) = registry_api_test_app();
    let project = create_project_for_test(&store, "p1");
    let body = serde_json::json!({
        "name": "ghcr",
        "endpoint": "ghcr.io",
        "auth_kind": "basic",
        "username": "zainokta"
    });
    let resp = app
        .oneshot(/* POST */)
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn registry_api_anonymous_needs_no_payload() {
    let (app, store) = registry_api_test_app();
    let project = create_project_for_test(&store, "p1");
    let body = serde_json::json!({
        "name": "pub",
        "endpoint": "docker.io",
        "auth_kind": "anonymous"
    });
    let resp = app
        .oneshot(/* POST */)
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::CREATED);
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(value["credential_ref"].is_null());
}
```

(Replace the `/* … */` placeholders with the same request-builder pattern used elsewhere in the file — see the original `registry_api_admin_can_crud_no_credential_leak` body for the verbatim form.)

- [ ] **Step 2: Run failing tests**

```
cargo test --test backend_contract registry_api -- --nocapture
```

Expected: fails to compile (`username`/`password` not in `RegistryInput`), or fails at runtime with the old plain-text error.

- [ ] **Step 3: Rewrite `RegistryInput` and handlers**

Replace the body of `src/api/registries.rs` (preserving `router()` shape and routes) with:

```rust
use axum::{Json, Router, extract::State, http::StatusCode, routing::get};
use serde::Deserialize;

use crate::api::ApiError;
use crate::app::AppState;
use crate::auth::{Principal, ensure_role};
use crate::domain::{Registry, RegistryAuthKind, Role};
use crate::secrets::{SecretPayload, SecretRef};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/projects/{project_id}/registries",
            get(list_registries).post(create_registry),
        )
        .route(
            "/projects/{project_id}/registries/{registry_id}",
            get(get_registry)
                .patch(update_registry_handler)
                .delete(delete_registry_handler),
        )
}

#[derive(Debug, Deserialize)]
#[serde(tag = "auth_kind", rename_all = "snake_case", deny_unknown_fields)]
enum RegistryInputAuth {
    Anonymous,
    Basic { username: String, password: String },
    Token { token: String },
    EcrToken { token: String },
    GarToken { token: String },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryInput {
    name: String,
    endpoint: String,
    #[serde(flatten)]
    auth: RegistryInputAuth,
}

impl RegistryInputAuth {
    fn kind(&self) -> RegistryAuthKind {
        match self {
            Self::Anonymous => RegistryAuthKind::Anonymous,
            Self::Basic { .. } => RegistryAuthKind::Basic,
            Self::Token { .. } => RegistryAuthKind::Token,
            Self::EcrToken { .. } => RegistryAuthKind::EcrToken,
            Self::GarToken { .. } => RegistryAuthKind::GarToken,
        }
    }

    fn payload(&self) -> Option<SecretPayload> {
        match self {
            Self::Anonymous => None,
            Self::Basic { username, password } => {
                Some(SecretPayload::new(format!("{username}:{password}")))
            }
            Self::Token { token }
            | Self::EcrToken { token }
            | Self::GarToken { token } => Some(SecretPayload::new(token.clone())),
        }
    }
}

async fn list_registries(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(project_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<Vec<Registry>>, ApiError> {
    ensure_role(&state, &principal, project_id, Role::Admin)?;
    Ok(Json(state.registries.registries_for_project(project_id)?))
}

async fn encrypt_and_persist(
    state: &AppState,
    project_id: uuid::Uuid,
    payload: &SecretPayload,
) -> Result<SecretRef, ApiError> {
    let recipient = state
        .config
        .age_recipient
        .as_deref()
        .ok_or_else(|| ApiError::BadRequest(
            "control plane has no DENIA_AGE_RECIPIENT configured".into(),
        ))?;
    let secret_ref = SecretRef::generate("registry");
    let store = crate::secrets::SopsSecretStore::new(state.config.data_dir.clone());
    store
        .encrypt(
            state.command_runner.as_ref(),
            state.config.sops_binary.as_path(),
            recipient,
            project_id,
            &secret_ref,
            payload,
        )
        .await
        .map_err(|e| ApiError::BadRequest(format!("secret encryption failed: {e}")))?;
    Ok(secret_ref)
}

async fn create_registry(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(project_id): axum::extract::Path<uuid::Uuid>,
    Json(input): Json<RegistryInput>,
) -> Result<(StatusCode, Json<Registry>), ApiError> {
    ensure_role(&state, &principal, project_id, Role::Admin)?;
    let auth_kind = input.auth.kind();
    let credential_ref = match input.auth.payload() {
        Some(payload) => Some(encrypt_and_persist(&state, project_id, &payload).await?),
        None => None,
    };
    let registry = Registry::new(
        project_id,
        input.name,
        input.endpoint,
        auth_kind,
        credential_ref,
    )
    .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    state.registries.create_registry(&registry)?;
    Ok((StatusCode::CREATED, Json(registry)))
}

async fn get_registry(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path((project_id, registry_id)): axum::extract::Path<(uuid::Uuid, uuid::Uuid)>,
) -> Result<Json<Registry>, ApiError> {
    ensure_role(&state, &principal, project_id, Role::Admin)?;
    let registry = state
        .registries
        .registry(registry_id)?
        .ok_or_else(|| ApiError::NotFound("registry not found".into()))?;
    if registry.project_id != project_id {
        return Err(ApiError::NotFound("registry not found".into()));
    }
    Ok(Json(registry))
}

async fn update_registry_handler(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path((project_id, registry_id)): axum::extract::Path<(uuid::Uuid, uuid::Uuid)>,
    Json(input): Json<RegistryInput>,
) -> Result<Json<Registry>, ApiError> {
    ensure_role(&state, &principal, project_id, Role::Admin)?;
    let existing = state
        .registries
        .registry(registry_id)?
        .ok_or_else(|| ApiError::NotFound("registry not found".into()))?;
    if existing.project_id != project_id {
        return Err(ApiError::NotFound("registry not found".into()));
    }
    let auth_kind = input.auth.kind();
    let credential_ref = match (input.auth.payload(), existing.credential_ref.clone()) {
        (Some(payload), Some(prev_ref)) => {
            // Reuse the same ref so the encrypted file is overwritten in-place
            // and ServiceConfig rows referencing it stay valid.
            let store = crate::secrets::SopsSecretStore::new(state.config.data_dir.clone());
            let recipient = state
                .config
                .age_recipient
                .as_deref()
                .ok_or_else(|| ApiError::BadRequest(
                    "control plane has no DENIA_AGE_RECIPIENT configured".into(),
                ))?;
            store
                .encrypt(
                    state.command_runner.as_ref(),
                    state.config.sops_binary.as_path(),
                    recipient,
                    project_id,
                    &prev_ref,
                    &payload,
                )
                .await
                .map_err(|e| ApiError::BadRequest(format!("secret encryption failed: {e}")))?;
            Some(prev_ref)
        }
        (Some(payload), None) => {
            Some(encrypt_and_persist(&state, project_id, &payload).await?)
        }
        (None, _) => None,
    };
    let mut updated = Registry::new(
        project_id,
        input.name,
        input.endpoint,
        auth_kind,
        credential_ref,
    )
    .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    updated.id = registry_id;
    state.registries.update_registry(&updated)?;
    Ok(Json(updated))
}

async fn delete_registry_handler(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path((project_id, registry_id)): axum::extract::Path<(uuid::Uuid, uuid::Uuid)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    ensure_role(&state, &principal, project_id, Role::Admin)?;
    let registry = state
        .registries
        .registry(registry_id)?
        .ok_or_else(|| ApiError::NotFound("registry not found".into()))?;
    if registry.project_id != project_id {
        return Err(ApiError::NotFound("registry not found".into()));
    }
    state.registries.delete_registry(registry_id)?;
    Ok(Json(serde_json::json!({"deleted": true})))
}
```

> `deny_unknown_fields` on the outer struct rejects legacy `secret_ref` automatically as 400. The 400 error body is JSON per the existing `ApiError::IntoResponse`.

- [ ] **Step 4: Wire `command_runner` access (sanity check)**

`state.command_runner` is `Arc<dyn CommandRunner>` (visible inside the crate). If `pub(crate)` visibility blocks the API module (it shouldn't — both are crate-internal), confirm with `cargo check`. If it does, widen visibility to `pub(crate)` on the field is already enough.

- [ ] **Step 5: Run tests**

```
cargo test --test backend_contract registry_api -- --nocapture
cargo test --lib
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/api/registries.rs tests/backend_contract.rs
git commit -m "feat(api): registries accept inline credentials, encrypt server-side"
```

---

## Task 5: Frontend — replace `secret_ref` field

**Files:**
- Modify: `web/src/.../registries-form.tsx` (locate via `rg -n "secret_ref" web/src`)
- Modify: TypeScript request typing for registry POST/PATCH

- [ ] **Step 1: Locate frontend pieces**

```
rg -n "secret_ref" web/src
rg -n "registries" web/src/routes
```

- [ ] **Step 2: Update form schema and request payload**

Replace the single `secret_ref` input with a switch on `auth_kind`:

- `anonymous` → no extra fields
- `basic` → two fields: `username` (text), `password` (password-masked)
- `token` / `ecr_token` / `gar_token` → one field: `token` (password-masked)

Update the TypeScript request type so the body shape mirrors `RegistryInputAuth` (flat fields, no `secret_ref`).

> Per project AGENTS.md, frontend changes are user-explicit only. This is one such case (the user reported the UX issue directly).

- [ ] **Step 3: Manual verify in browser**

```bash
cd web && pnpm build
cd .. && cargo run
# Visit http://localhost:7180, create a registry, confirm 201 with credential_ref echoed,
# confirm <data_dir>/secrets/<pid>/<ref>.sops.yaml exists (mode 0600).
```

> Skip the manual verify step if running headless. Note that explicitly in the handoff.

- [ ] **Step 4: Commit**

```bash
git add web/src
git commit -m "feat(web): registry form accepts inline username/password/token"
```

---

## Task 6: Documentation + bootstrap

**Files:**
- Modify: `AGENTS.md` (drop the "operator-managed registry .sops.yaml" wording in favor of ADR-021)
- Modify: `README.md` if it mentions `secrets/*.sops.yaml` for registry creds (`README.md:221`)

- [ ] **Step 1: Edit AGENTS.md**

Replace the line:

```
- SOPS-encrypted files hold SSH deploy keys, registry credentials, and service secrets.
```

with:

```
- SOPS-encrypted files hold SSH deploy keys and service secrets. Registry credentials are encrypted by the control plane on the registry CRUD path; see ADR-021. Operators must set `DENIA_AGE_RECIPIENT` in addition to the existing decryption key.
```

- [ ] **Step 2: Edit README.md**

Update the registry-credentials sentence (`README.md:221`) to point at ADR-021 and mention the new env var.

- [ ] **Step 3: Commit**

```bash
git add AGENTS.md README.md
git commit -m "docs: control-plane registry secret encryption (ADR-021)"
```

---

## Task 7: Final verification

- [ ] **Step 1: Full test suite**

```
cargo fmt --all
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

Expected: all green. Report counts (passed / ignored).

- [ ] **Step 2: GitNexus self-check**

```
gitnexus_detect_changes({scope: "all"})
```

Expected: only the files listed in this plan show as changed.

- [ ] **Step 3: Summarize verification commands and results** to the user before declaring complete.

---

## Out of Scope

- Migrating service-secret refs (SSH deploy keys, generic service secrets) to inline payload. They keep the operator-managed flow until a follow-up ADR.
- Rotation policy beyond "PATCH overwrites the file at the existing ref". Periodic rotation is its own plan.
- An `age` Rust crate dependency for in-process encryption (rejected in ADR-021 alternatives; revisit if `sops` binary dependency becomes painful).
