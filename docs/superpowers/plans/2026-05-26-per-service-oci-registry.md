# Per-Service OCI Registry Configuration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let each service reference a project-scoped, named OCI Registry record so private/authenticated image pulls work per workload.

**Architecture:** A new `Registry` domain entity (project-scoped, stored as JSON in a SQLite `registries` table) carries an `endpoint` + `auth_kind` + optional SOPS `credential_ref`. At deploy time the coordinator resolves the service's registry, decrypts the credential, maps it to an `oci_client::RegistryAuth` via `resolve_registry_auth`, and passes the auth explicitly into `OciImagePuller::pull(image, auth)`. `ExternalImageSource` keeps its legacy `image` field for backwards compatibility while adding optional `registry_id` + `image_ref`.

**Tech Stack:** Rust 2024, axum, rusqlite, serde, uuid v7, oci-client, SOPS (via `SopsSecretStore`), thiserror.

**Spec:** `docs/superpowers/specs/2026-05-25-per-service-oci-registry-design.md`

---

## Notes for the implementer

- **TDD:** every task writes a failing test first, runs it to confirm failure, implements minimal code, re-runs to confirm pass, commits.
- **`OciImagePuller::pull` signature changes** from `pull(&self, image)` to `pull(&self, image, auth)`. This breaks `RegistryImagePuller` (src/oci/registry.rs:31), the two acquirer call sites (src/artifacts/acquirer.rs:187,283), and the `FakePuller` in tests (tests/backend_contract.rs:174). All must be updated in Task 3 so the workspace keeps compiling.
- **Existing patterns to mirror:**
  - Domain struct + validation: `ServiceConfig` / `Registry`-like in src/domain.rs.
  - Store method style (JSON-in-column): jobs (src/state.rs:981-1020), migration block (src/state.rs:262-314).
  - API CRUD handler style: project members (src/app.rs:536-573), `put_credential` (src/app.rs:575-587).
  - RBAC: `ensure_role(&state, &principal, project_id, Role::Admin)` (src/app.rs:247).
  - `ApiError` enum + mapping (src/app.rs:1108-1206).
- **Security:** never log decrypted SOPS payloads; API responses echo only the `SecretRef` name; `resolve_registry_auth` errors must be generic (no payload contents).
- Run the full suite with `cargo test` and lints with `cargo clippy --all-targets --all-features` at the end.

---

## Task 1: `RegistryAuthKind` + `Registry` domain type

**Files:**
- Modify: `src/domain.rs`
- Test: `src/domain.rs` (inline `#[cfg(test)]` module — follow existing in-file test style)

- [ ] **Step 1: Write failing tests**

Add to the domain tests module:

```rust
#[test]
fn registry_requires_credential_unless_anonymous() {
    let err = Registry::new(Uuid::now_v7(), "ghcr", "ghcr.io", RegistryAuthKind::Basic, None)
        .unwrap_err();
    assert_eq!(err, DomainError::RegistryMissingCredential);

    let ok = Registry::new(
        Uuid::now_v7(), "public", "docker.io", RegistryAuthKind::Anonymous, None,
    );
    assert!(ok.is_ok());
}

#[test]
fn registry_rejects_empty_name_or_endpoint() {
    let p = Uuid::now_v7();
    let r = SecretRef::parse("ghcr-cred").unwrap();
    assert_eq!(
        Registry::new(p, "  ", "ghcr.io", RegistryAuthKind::Basic, Some(r.clone())).unwrap_err(),
        DomainError::EmptyName
    );
    assert_eq!(
        Registry::new(p, "ghcr", "", RegistryAuthKind::Basic, Some(r)).unwrap_err(),
        DomainError::RegistryMissingEndpoint
    );
}
```

- [ ] **Step 2: Run, confirm fail**

Run: `cargo test --lib domain::tests::registry`
Expected: FAIL (type `Registry` / variants not found).

- [ ] **Step 3: Implement**

Add near the `Credential` types in src/domain.rs:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistryAuthKind {
    Anonymous,
    Basic,
    Token,
    EcrToken,
    GarToken,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registry {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub endpoint: String,
    pub auth_kind: RegistryAuthKind,
    pub credential_ref: Option<SecretRef>,
}

impl Registry {
    pub fn new(
        project_id: Uuid,
        name: impl Into<String>,
        endpoint: impl Into<String>,
        auth_kind: RegistryAuthKind,
        credential_ref: Option<SecretRef>,
    ) -> Result<Self, DomainError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(DomainError::EmptyName);
        }
        let endpoint = endpoint.into();
        if endpoint.trim().is_empty() {
            return Err(DomainError::RegistryMissingEndpoint);
        }
        if auth_kind != RegistryAuthKind::Anonymous && credential_ref.is_none() {
            return Err(DomainError::RegistryMissingCredential);
        }
        Ok(Self {
            id: Uuid::now_v7(),
            project_id,
            name,
            endpoint,
            auth_kind,
            credential_ref,
        })
    }
}
```

Add to `DomainError` enum: `RegistryMissingEndpoint`, `RegistryMissingCredential` (with `#[error(..)]` messages matching the existing style).

- [ ] **Step 4: Run, confirm pass**

Run: `cargo test --lib domain::tests::registry`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/domain.rs
git commit -m "feat(domain): add Registry entity and RegistryAuthKind"
```

---

## Task 2: `ExternalImageSource` dual fields + resolution

**Files:**
- Modify: `src/domain.rs` (`ExternalImageSource` at src/domain.rs:82-86)
- Test: `src/domain.rs` inline tests

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn external_image_source_resolution_matrix() {
    // legacy: full image only
    let legacy = ExternalImageSource {
        image: "ghcr.io/acme/web:1".into(),
        credential: None,
        registry_id: None,
        image_ref: None,
    };
    let (full, used_registry) = legacy.resolve_ref("docker.io").unwrap();
    assert_eq!(full, "ghcr.io/acme/web:1");
    assert!(!used_registry);

    // new: registry + image_ref
    let new = ExternalImageSource {
        image: String::new(),
        credential: None,
        registry_id: Some(Uuid::now_v7()),
        image_ref: Some("library/redis:7".into()),
    };
    let (full, used_registry) = new.resolve_ref("docker.io").unwrap();
    assert_eq!(full, "docker.io/library/redis:7");
    assert!(used_registry);

    // ambiguous: both
    let both = ExternalImageSource {
        image: "x".into(),
        credential: None,
        registry_id: Some(Uuid::now_v7()),
        image_ref: Some("y".into()),
    };
    assert_eq!(both.validate().unwrap_err(), DomainError::RegistrySourceAmbiguous);

    // missing: neither
    let neither = ExternalImageSource {
        image: String::new(),
        credential: None,
        registry_id: None,
        image_ref: None,
    };
    assert_eq!(neither.validate().unwrap_err(), DomainError::RegistrySourceMissing);
}
```

- [ ] **Step 2: Run, confirm fail**

Run: `cargo test --lib domain::tests::external_image_source_resolution_matrix`
Expected: FAIL (fields/methods not found).

- [ ] **Step 3: Implement**

Replace `ExternalImageSource` (src/domain.rs:82-86) with:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalImageSource {
    pub image: String,
    pub credential: Option<SecretRef>,
    #[serde(default)]
    pub registry_id: Option<Uuid>,
    #[serde(default)]
    pub image_ref: Option<String>,
}

impl ExternalImageSource {
    fn uses_registry(&self) -> bool {
        self.registry_id.is_some() || self.image_ref.is_some()
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        let registry = self.registry_id.is_some() && self.image_ref.is_some();
        let partial_registry = self.uses_registry() && !registry;
        let legacy = !self.image.trim().is_empty();
        match (registry, legacy) {
            (true, true) => Err(DomainError::RegistrySourceAmbiguous),
            (false, false) => Err(DomainError::RegistrySourceMissing),
            _ if partial_registry => Err(DomainError::RegistrySourceMissing),
            _ => Ok(()),
        }
    }

    /// Returns (full_image_ref, used_registry).
    pub fn resolve_ref(&self, endpoint: &str) -> Result<(String, bool), DomainError> {
        self.validate()?;
        if let (Some(_), Some(image_ref)) = (self.registry_id, &self.image_ref) {
            Ok((format!("{}/{}", endpoint.trim_end_matches('/'), image_ref), true))
        } else {
            Ok((self.image.clone(), false))
        }
    }
}
```

Add to `DomainError`: `RegistrySourceAmbiguous`, `RegistrySourceMissing`.

Note: `resolve_ref` takes the endpoint because the registry record (and thus its endpoint) is loaded at the deploy boundary, not inside the domain type. For the legacy path the `endpoint` arg is ignored.

- [ ] **Step 4: Run, confirm pass**

Run: `cargo test --lib domain::tests::external_image_source_resolution_matrix`
Expected: PASS.

- [ ] **Step 5: Fix existing constructors**

`ServiceConfig`/`ServiceSource` construction sites and tests that build `ExternalImageSource { image, credential }` now need the two new fields. Run `cargo build` and add `registry_id: None, image_ref: None` to each literal the compiler flags.

Run: `cargo build`
Expected: compiles after literal updates.

- [ ] **Step 6: Commit**

```bash
git add src/domain.rs
git commit -m "feat(domain): dual-field ExternalImageSource with registry resolution"
```

---

## Task 3: `resolve_registry_auth` + `OciImagePuller::pull(image, auth)` + cleanup

**Files:**
- Modify: `src/oci/credentials.rs` (replace provider with resolver)
- Modify: `src/oci/mod.rs` (trait signature; drop `ecr`/`gar` module decls)
- Modify: `src/oci/registry.rs` (drop provider field, use passed auth)
- Delete: `src/oci/ecr.rs`, `src/oci/gar.rs`
- Modify: `src/artifacts/acquirer.rs` (thread `auth` through, drop `StaticCredentialProvider`)
- Modify: `Cargo.toml` (remove `ecr`/`gar` features)
- Modify: `tests/backend_contract.rs` (`FakePuller::pull` signature)
- Test: `src/oci/credentials.rs` inline tests

- [ ] **Step 1: Write failing tests for `resolve_registry_auth`**

In src/oci/credentials.rs:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::RegistryAuthKind;
    use crate::secrets::SecretPayload;

    #[test]
    fn anonymous_needs_no_payload() {
        let auth = resolve_registry_auth(RegistryAuthKind::Anonymous, None).unwrap();
        assert!(matches!(auth, RegistryAuth::Anonymous));
    }

    #[test]
    fn basic_splits_user_password() {
        let p = SecretPayload::new("alice:s3cret");
        match resolve_registry_auth(RegistryAuthKind::Basic, Some(&p)).unwrap() {
            RegistryAuth::Basic(u, pw) => {
                assert_eq!(u, "alice");
                assert_eq!(pw, "s3cret");
            }
            _ => panic!("expected basic"),
        }
    }

    #[test]
    fn basic_rejects_malformed_payload() {
        let p = SecretPayload::new("no-colon");
        assert!(resolve_registry_auth(RegistryAuthKind::Basic, Some(&p)).is_err());
    }

    #[test]
    fn basic_requires_payload() {
        assert!(resolve_registry_auth(RegistryAuthKind::Basic, None).is_err());
    }

    #[test]
    fn ecr_and_gar_map_to_fixed_users() {
        let p = SecretPayload::new("tok");
        match resolve_registry_auth(RegistryAuthKind::EcrToken, Some(&p)).unwrap() {
            RegistryAuth::Basic(u, pw) => { assert_eq!(u, "AWS"); assert_eq!(pw, "tok"); }
            _ => panic!(),
        }
        match resolve_registry_auth(RegistryAuthKind::GarToken, Some(&p)).unwrap() {
            RegistryAuth::Basic(u, pw) => { assert_eq!(u, "oauth2accesstoken"); assert_eq!(pw, "tok"); }
            _ => panic!(),
        }
    }
}
```

- [ ] **Step 2: Run, confirm fail**

Run: `cargo test --lib oci::credentials`
Expected: FAIL (function not defined).

- [ ] **Step 3: Replace `credentials.rs` body**

Confirm whether `oci_client` exposes `RegistryAuth::Bearer` (check the version in Cargo.lock; `grep -rn "Bearer" ~/.cargo/registry/src/*/oci-client*/`). If present, map `Token` → `RegistryAuth::Bearer(token)`. If absent, map `Token` → `RegistryAuth::Basic("".into(), token)` and leave a `// oci-client lacks Bearer; token-as-password` comment.

```rust
use oci_client::secrets::RegistryAuth;

use crate::domain::RegistryAuthKind;
use crate::secrets::SecretPayload;
use super::OciError;

fn payload_value(payload: Option<&SecretPayload>) -> Result<String, OciError> {
    payload
        .map(|p| p.value.clone())
        .ok_or_else(|| OciError::Pull("registry credential is required".into()))
}

pub fn resolve_registry_auth(
    kind: RegistryAuthKind,
    payload: Option<&SecretPayload>,
) -> Result<RegistryAuth, OciError> {
    match kind {
        RegistryAuthKind::Anonymous => Ok(RegistryAuth::Anonymous),
        RegistryAuthKind::Basic => {
            let raw = payload.ok_or_else(|| OciError::Pull("registry credential is required".into()))?;
            let (user, pass) = raw
                .value
                .split_once(':')
                .ok_or_else(|| OciError::Pull("basic credential must be 'user:password'".into()))?;
            Ok(RegistryAuth::Basic(user.to_string(), pass.to_string()))
        }
        RegistryAuthKind::Token => Ok(RegistryAuth::Bearer(payload_value(payload)?)),
        RegistryAuthKind::EcrToken => Ok(RegistryAuth::Basic("AWS".into(), payload_value(payload)?)),
        RegistryAuthKind::GarToken => {
            Ok(RegistryAuth::Basic("oauth2accesstoken".into(), payload_value(payload)?))
        }
    }
}
```

Delete `RegistryCredentialProvider` trait and `StaticCredentialProvider` struct entirely.

- [ ] **Step 4: Update trait + RegistryImagePuller**

In src/oci/mod.rs: change trait method to `async fn pull(&self, image: &str, auth: oci_client::secrets::RegistryAuth) -> Result<PulledImage, OciError>;`. Remove `pub mod ecr;` / `pub mod gar;` and their `#[cfg(feature = ...)]` lines.

In src/oci/registry.rs: drop the `credential_provider` field, the `Arc<dyn RegistryCredentialProvider>` import, and the `new(credential_provider)` arg → `new() -> Self`. In `pull`, delete the `auth_for` lookup and use the `auth` parameter directly in `self.client.pull(&reference, &auth, accepted)`.

- [ ] **Step 5: Update acquirer**

In src/artifacts/acquirer.rs:
- Remove the `StaticCredentialProvider` import and its use in `new` → `puller: Arc::new(RegistryImagePuller::new())`.
- Add `auth: RegistryAuth` params to `acquire_rootfs_bundle_from_image_config`, `pull_and_unpack_external`, `acquire_external_image`; thread into `self.puller.pull(image, auth)` calls (src/artifacts/acquirer.rs:187,283).
- Git path (`acquire_rootfs_bundle_from_image_config` git arm and `materialize_rootfs_bundle_inprocess`) does `read_layout`, not `pull` — no auth needed there; only pass auth on the external-image arm. For the git arm of `acquire_rootfs_bundle_from_image_config`, no auth param is consumed.

Decision: give `acquire_rootfs_bundle_from_image_config(runner, request, auth: RegistryAuth)` the auth and only use it on the `ExternalImage` arm.

- [ ] **Step 6: Update Cargo.toml + fake puller**

Cargo.toml: delete `ecr = []` and `gar = []` (lines 35-36), keep `default = []`.

tests/backend_contract.rs:174 — update `FakePuller::pull` to `async fn pull(&self, _image: &str, _auth: denia::oci::RegistryAuth...)`. Use the actual `oci_client::secrets::RegistryAuth` type (re-export it from `denia::oci` if not already; otherwise `oci_client::secrets::RegistryAuth`). Update the call at backend_contract.rs:208-216 to pass `RegistryAuth::Anonymous`.

- [ ] **Step 7: Build + test whole workspace**

Run: `cargo build && cargo test --lib oci::credentials && cargo test --test backend_contract artifact_acquirer_pulls_external_image`
Expected: compiles; both tests PASS.

- [ ] **Step 8: Commit**

```bash
git add src/oci/ src/artifacts/acquirer.rs Cargo.toml tests/backend_contract.rs
git rm src/oci/ecr.rs src/oci/gar.rs
git commit -m "refactor(oci): resolve_registry_auth, explicit pull auth, drop env providers"
```

---

## Task 4: SQLite migration v6 + registry store methods

**Files:**
- Modify: `src/state.rs` (migration block after v5 at src/state.rs:314; add methods + `StateError` variants)
- Test: `src/state.rs` inline tests (mirror `migrate_advances_to_version_5` at src/state.rs:1348)

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn registry_crud_roundtrip_and_unique_name() {
    let store = test_store(); // follow existing helper used by other state tests
    let project = store.put_project(Project::new("p", None).unwrap()).unwrap();
    let cred = SecretRef::parse("ghcr-cred").unwrap();
    let reg = Registry::new(project.id, "ghcr", "ghcr.io", RegistryAuthKind::Basic, Some(cred)).unwrap();

    store.create_registry(&reg).unwrap();
    assert_eq!(store.registry(reg.id).unwrap().unwrap().endpoint, "ghcr.io");
    assert_eq!(store.registries_for_project(project.id).unwrap().len(), 1);

    // duplicate (project_id, name) rejected
    let dup = Registry::new(project.id, "ghcr", "other.io", RegistryAuthKind::Anonymous, None).unwrap();
    assert!(store.create_registry(&dup).is_err());
}

#[test]
fn delete_registry_blocked_when_referenced() {
    let store = test_store();
    let project = store.put_project(Project::new("p", None).unwrap()).unwrap();
    let reg = Registry::new(project.id, "ghcr", "ghcr.io", RegistryAuthKind::Anonymous, None).unwrap();
    store.create_registry(&reg).unwrap();

    // a service in the project referencing reg.id
    let svc = service_with_registry(project.id, reg.id); // build ServiceConfig w/ ExternalImage{registry_id:Some(reg.id), image_ref:Some("x:1")}
    store.put_service(svc).unwrap();

    assert_eq!(store.delete_registry(reg.id).unwrap_err(), StateError::RegistryInUse);
}
```

(Use the helpers the existing state tests use to build a store/service; check the test module at the bottom of src/state.rs for the exact pattern before writing.)

- [ ] **Step 2: Run, confirm fail**

Run: `cargo test --lib state::tests::registry`
Expected: FAIL (methods/variants not found).

- [ ] **Step 3: Add migration v6**

After the `if current < 5 { .. }` block (src/state.rs:314), before `Ok(())`:

```rust
if current < 6 {
    connection.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS registries (
            id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            name TEXT NOT NULL,
            config_json TEXT NOT NULL,
            UNIQUE(project_id, name)
        );
        CREATE INDEX IF NOT EXISTS idx_registries_project ON registries(project_id);
        "#,
    )?;
    connection.execute("DELETE FROM schema_version", [])?;
    connection.execute("INSERT INTO schema_version (version) VALUES (6)", [])?;
}
```

- [ ] **Step 4: Add store methods + StateError variants**

Add `RegistryInUse`, `RegistryNotFound` to `StateError` (with `#[error(..)]`). Add methods on `SqliteStore` (mirror jobs at src/state.rs:981-1020):

```rust
pub fn create_registry(&self, registry: &Registry) -> Result<(), StateError> {
    let conn = self.connection()?;
    conn.execute(
        "INSERT INTO registries (id, project_id, name, config_json) VALUES (?1, ?2, ?3, ?4)",
        params![
            registry.id.to_string(),
            registry.project_id.to_string(),
            registry.name,
            serde_json::to_string(registry)?,
        ],
    )?;
    Ok(())
}

pub fn update_registry(&self, registry: &Registry) -> Result<(), StateError> {
    let conn = self.connection()?;
    let n = conn.execute(
        "UPDATE registries SET name = ?2, config_json = ?3 WHERE id = ?1",
        params![registry.id.to_string(), registry.name, serde_json::to_string(registry)?],
    )?;
    if n == 0 { return Err(StateError::RegistryNotFound); }
    Ok(())
}

pub fn registry(&self, id: Uuid) -> Result<Option<Registry>, StateError> {
    let conn = self.connection()?;
    let json: Option<String> = conn
        .query_row("SELECT config_json FROM registries WHERE id = ?1", params![id.to_string()], |r| r.get(0))
        .optional()?;
    Ok(json.map(|j| serde_json::from_str(&j)).transpose()?)
}

pub fn registries_for_project(&self, project_id: Uuid) -> Result<Vec<Registry>, StateError> {
    let conn = self.connection()?;
    let mut stmt = conn.prepare(
        "SELECT config_json FROM registries WHERE project_id = ?1 ORDER BY name")?;
    let rows = stmt.query_map(params![project_id.to_string()], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for row in rows { out.push(serde_json::from_str(&row?)?); }
    Ok(out)
}

pub fn delete_registry(&self, id: Uuid) -> Result<(), StateError> {
    let conn = self.connection()?;
    let registry = self.registry(id)?.ok_or(StateError::RegistryNotFound)?;
    // refuse if any service in the project references this registry_id
    let mut stmt = conn.prepare("SELECT config_json FROM services WHERE project_id = ?1")?;
    let rows = stmt.query_map(params![registry.project_id.to_string()], |r| r.get::<_, String>(0))?;
    for row in rows {
        if let Ok(svc) = serde_json::from_str::<ServiceConfig>(&row?) {
            if let crate::domain::ServiceSource::ExternalImage(src) = &svc.source {
                if src.registry_id == Some(id) {
                    return Err(StateError::RegistryInUse);
                }
            }
        }
    }
    conn.execute("DELETE FROM registries WHERE id = ?1", params![id.to_string()])?;
    Ok(())
}
```

Ensure `Registry`, `RegistryAuthKind` are imported in src/state.rs (extend the existing `use crate::domain::{..}` at src/state.rs:12). `optional()` comes from `rusqlite::OptionalExtension` — confirm it's already imported (jobs use it); if not, add the import.

- [ ] **Step 5: Run, confirm pass**

Run: `cargo test --lib state::tests::registry && cargo test --lib state::tests::migrate`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/state.rs
git commit -m "feat(state): registries table v6 migration and CRUD with in-use guard"
```

---

## Task 5: Deploy-path auth resolution

**Files:**
- Modify: `src/deploy.rs` (`deploy_external_image_source` at src/deploy.rs:172-195; `DeployError` at src/deploy.rs:29-51)
- Modify: `src/app.rs` (`create_deployment` external arm at src/app.rs:311-332 — pass SOPS deps)
- Test: `tests/backend_contract.rs` (extend the fake-puller deploy test to assert received auth)

- [ ] **Step 1: Write failing test**

Extend backend_contract.rs: a `FakePuller` that records the `RegistryAuth` it received. Build a project + registry (Basic, credential_ref pointing at a SOPS file the `FakeCommandRunner` decrypts to `{"value":"alice:pw"}`) + a service with `ExternalImage { registry_id: Some(reg), image_ref: Some("acme/web:1") }`. Deploy via the external-image path and assert the puller received `RegistryAuth::Basic("alice","pw")` and image `"ghcr.io/acme/web:1"`. Add a second case: legacy `image`-only service → puller receives `RegistryAuth::Anonymous`. Add a third: unknown `registry_id` → `DeployError::RegistryNotFound`.

(Mirror how existing deploy tests stand up a coordinator + acquirer with `with_traits`; check the existing external-image deploy test for the harness.)

- [ ] **Step 2: Run, confirm fail**

Run: `cargo test --test backend_contract deploy_external_image`
Expected: FAIL.

- [ ] **Step 3: Implement deploy wiring**

Add to `DeployError`: `RegistryNotFound`, `SecretDecrypt(#[from] crate::secrets::SecretError)`.

Change `deploy_external_image_source` to accept the SOPS pieces and resolve auth. New signature:

```rust
pub async fn deploy_external_image_source(
    &self,
    service: &ServiceConfig,
    acquirer: &ArtifactAcquirer,
    runner: &dyn CommandRunner,
    secret_store: &crate::secrets::SopsSecretStore,
    sops_binary: &std::path::Path,
) -> Result<Deployment, DeployError> {
    let ServiceSource::ExternalImage(source) = &service.source else {
        return Err(DeployError::UnsupportedServiceSource);
    };

    let (full_ref, auth) = if let Some(registry_id) = source.registry_id {
        let registry = self
            .store
            .registry(registry_id)?
            .ok_or(DeployError::RegistryNotFound)?;
        let payload = match &registry.credential_ref {
            Some(secret_ref) => Some(secret_store.decrypt(runner, sops_binary, secret_ref).await?),
            None => None,
        };
        let auth = crate::oci::credentials::resolve_registry_auth(registry.auth_kind, payload.as_ref())?;
        let (full_ref, _) = source.resolve_ref(&registry.endpoint)
            .map_err(|_| DeployError::UnsupportedServiceSource)?;
        (full_ref, auth)
    } else {
        // legacy full-image path: decrypt legacy `credential` as Basic if present, else Anonymous
        let (full_ref, _) = source.resolve_ref("")
            .map_err(|_| DeployError::UnsupportedServiceSource)?;
        let auth = match &source.credential {
            Some(secret_ref) => {
                let payload = secret_store.decrypt(runner, sops_binary, secret_ref).await?;
                crate::oci::credentials::resolve_registry_auth(
                    crate::domain::RegistryAuthKind::Basic,
                    Some(&payload),
                )?
            }
            None => oci_client::secrets::RegistryAuth::Anonymous,
        };
        (full_ref, auth)
    };

    let artifact = acquirer
        .acquire_rootfs_bundle_from_image_config(
            runner,
            ArtifactAcquireRequest::ExternalImage { image: full_ref },
            auth,
        )
        .await?;

    self.deploy(DeploymentPlan { service: service.clone(), artifact }).await
}
```

Add a `OciError` → `DeployError` mapping if needed (`resolve_registry_auth` returns `OciError`); add `#[error("oci error: {0}")] Oci(#[from] crate::oci::OciError)` to `DeployError` and map it.

`deploy_git_source` (src/deploy.rs:197) keeps its current behavior but its acquirer call now needs the new `auth` arg — pass `RegistryAuth::Anonymous`.

- [ ] **Step 4: Update `create_deployment` caller (src/app.rs:325)**

Build a `SopsSecretStore` from `state.config` data dir and pass `state.config.sops_binary` (confirm field names: `grep -n "sops_binary\|data_dir\|SopsSecretStore::new" src/config.rs src/secrets.rs src/app.rs`). Pass both into `deploy_external_image_source`.

- [ ] **Step 5: Build + test**

Run: `cargo build && cargo test --test backend_contract deploy_external_image`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/deploy.rs src/app.rs
git commit -m "feat(deploy): resolve per-service registry auth before pull"
```

---

## Task 6: Registry CRUD API + service validation

**Files:**
- Modify: `src/app.rs` (handlers + routes at src/app.rs:175-220; `ApiError` mapping at src/app.rs:1108-1206)
- Test: `tests/backend_contract.rs` (HTTP-level RBAC + no-credential-leak)

- [ ] **Step 1: Write failing tests**

Using the existing HTTP test harness (router + token), assert:
- `POST /v1/projects/{id}/registries` as Admin → 201; response body has no decrypted secret, only `credential_ref` name.
- Same as Viewer/Operator → 403.
- `GET /v1/projects/{id}/registries` lists created registries.
- `DELETE` a referenced registry → 409 (`RegistryInUse`).
- `POST /v1/services` with `registry_id` not in the project → 404.

(Find the existing project-members or domains HTTP tests in backend_contract.rs and mirror their request-building.)

- [ ] **Step 2: Run, confirm fail**

Run: `cargo test --test backend_contract registry_api`
Expected: FAIL.

- [ ] **Step 3: Implement handlers**

Add request body type + five handlers (mirror members at src/app.rs:536-573). All call `ensure_role(&state, &principal, project_id, Role::Admin)`:

```rust
#[derive(Debug, Deserialize)]
struct RegistryInput {
    name: String,
    endpoint: String,
    auth_kind: crate::domain::RegistryAuthKind,
    secret_ref: Option<String>,
}

async fn list_registries(/* State, Principal, Path(project_id) */) -> Result<Json<Vec<Registry>>, ApiError> {
    ensure_role(..Admin)?;
    Ok(Json(state.store.registries_for_project(project_id)?))
}

async fn create_registry(/* .., Json(input) */) -> Result<(StatusCode, Json<Registry>), ApiError> {
    ensure_role(..Admin)?;
    let cred = input.secret_ref.map(SecretRef::parse).transpose().map_err(ApiError::InvalidSecretRef)?;
    let registry = Registry::new(project_id, input.name, input.endpoint, input.auth_kind, cred)
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    state.store.create_registry(&registry)?;
    Ok((StatusCode::CREATED, Json(registry)))
}

async fn get_registry(/* Path((project_id, id)) */) -> Result<Json<Registry>, ApiError> {
    ensure_role(..Admin)?;
    let registry = state.store.registry(id)?.ok_or_else(|| ApiError::NotFound("registry not found".into()))?;
    if registry.project_id != project_id { return Err(ApiError::NotFound("registry not found".into())); }
    Ok(Json(registry))
}

async fn update_registry_handler(/* Path((project_id, id)), Json(input) */) -> Result<Json<Registry>, ApiError> {
    ensure_role(..Admin)?;
    // load existing for project check, rebuild with same id, call store.update_registry
}

async fn delete_registry_handler(/* Path((project_id, id)) */) -> Result<Json<serde_json::Value>, ApiError> {
    ensure_role(..Admin)?;
    state.store.delete_registry(id)?;
    Ok(Json(serde_json::json!({"deleted": true})))
}
```

`Registry` serializes its `credential_ref` as a `SecretRef` (the name only) — no decryption happens in any handler, so no leak. Confirm `SecretRef` serializes to its string form (it does — src/secrets.rs custom (De)serialize).

In `put_service` (src/app.rs:293-299): after the role check, if `ServiceSource::ExternalImage`, call `source.validate().map_err(|e| ApiError::BadRequest(e.to_string()))?`, and if `registry_id` is set, verify `state.store.registry(id)` exists and belongs to `service.project_id`, else `ApiError::NotFound`.

- [ ] **Step 4: Wire routes**

Add to the `protected` router (src/app.rs:199, near projects):

```rust
.route(
    "/projects/{project_id}/registries",
    get(list_registries).post(create_registry),
)
.route(
    "/projects/{project_id}/registries/{registry_id}",
    get(get_registry).patch(update_registry_handler).delete(delete_registry_handler),
)
```

Add `patch` to the `axum::routing` import (src/app.rs:7).

Add `StateError::RegistryInUse` → 409 and `StateError::RegistryNotFound` → 404 arms to the `ApiError::State` match (src/app.rs:1166-1180).

- [ ] **Step 5: Run, confirm pass**

Run: `cargo test --test backend_contract registry_api`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/app.rs
git commit -m "feat(api): project-scoped registry CRUD with admin RBAC"
```

---

## Task 7: ADR-012 + index + full verification

**Files:**
- Create: `docs/adr/012-per-service-registry.md`
- Modify: `docs/adr/README.md`
- Modify: `docs/adr/011-inprocess-oci-acquisition.md` (mark amended)
- Modify: `TODO.md` (check off item 16)

- [ ] **Step 1: Write ADR-012**

Follow the format of `docs/adr/011-inprocess-oci-acquisition.md`. Status: Proposed. Context: per-service registry need. Decision: project-scoped `Registry` entity; five auth kinds (Anonymous/Basic/Token/EcrToken/GarToken) with pre-minted ECR/GAR tokens in SOPS; removal of env-var ECR/GAR providers and `ecr`/`gar` cargo features; `OciImagePuller::pull(image, auth)` signature change; dual-field backwards-compatible `ExternalImageSource`; migration v6. Consequences + Alternatives (in-process SigV4/metadata exchange deferred). References: this plan + the spec. Note "Amends ADR-011".

- [ ] **Step 2: Update index + ADR-011**

Add row `| 012 | Per-Service OCI Registry | Proposed | 2026-05-26 |` to docs/adr/README.md. Add an "Amended by ADR-012" note to ADR-011.

- [ ] **Step 3: Check off TODO item 16** in TODO.md.

- [ ] **Step 4: Full verification**

Run:
```bash
cargo fmt --all
cargo build
cargo test
cargo clippy --all-targets --all-features
```
Expected: fmt clean, build ok, all tests PASS, clippy no warnings.

- [ ] **Step 5: Commit**

```bash
git add docs/adr/012-per-service-registry.md docs/adr/README.md docs/adr/011-inprocess-oci-acquisition.md TODO.md
git commit -m "docs(adr): accept 012 per-service registry and update index"
```
