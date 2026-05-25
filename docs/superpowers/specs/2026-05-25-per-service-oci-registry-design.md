# Per-Service OCI Registry Configuration — Design

- **Date**: 2026-05-25
- **Status**: Approved (brainstorm)
- **Scope**: Backend only. Web console UI and inline-Dockerfile authoring are explicitly out of scope (the latter is a separate follow-up spec).

## Problem

External-image services pull from OCI registries, but Denia has no per-service
way to configure which registry to authenticate against or how. Today
`ArtifactAcquirer::new` builds a single empty `StaticCredentialProvider`, so the
`ExternalImageSource.credential` field is never plumbed through to the pull, and
private registries cannot be used per workload. ECR/GAR support exists only as
feature-gated providers that read process-wide environment variables — unusable
for multi-registry, multi-service setups.

TODO.md item 16: "OCI registry should be able to be configured on each service"
— each workload must be able to override the default credential provider.

## Goals

- A reusable, project-scoped **Registry** entity that services reference by id.
- Four auth kinds: `Basic`, `Token` (bearer), `EcrToken`, `GarToken`. ECR/GAR
  use pre-minted tokens stored in SOPS (operator rotates externally); no AWS/GCP
  SDK dependency is added.
- Backwards compatibility: existing services keep their full `image` reference
  and continue to deploy without a registry record.
- Registry management restricted to project `Admin`.

## Non-Goals

- In-process SigV4 / GCP metadata-server token exchange (deferred; would be its
  own ADR per ADR-011's deferral note).
- Inline Dockerfile authoring per service (separate spec).
- System-scoped / shared registries across projects (project scope only).
- Web console UI for registry management.

## Decisions (from brainstorm)

| Question | Decision |
|----------|----------|
| Model | Named `Registry` entity, not inline or credential-only |
| Ownership | Project-scoped |
| Auth kinds | Basic, Token, EcrToken, GarToken |
| ECR/GAR creds | Pre-minted token in SOPS (no SDK) |
| Host model | Registry has `endpoint`; service stores host-less `image_ref` |
| Migration | Backwards-compatible dual fields (legacy `image` retained) |
| RBAC | Project `Admin` only |

## Design

### 1. Domain types (`src/domain.rs`)

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RegistryAuthKind {
    Anonymous,
    Basic,     // payload: "username:password"
    Token,     // payload: raw bearer/identity token
    EcrToken,  // payload: pre-minted ECR login password (user "AWS")
    GarToken,  // payload: pre-minted GAR access token (user "oauth2accesstoken")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registry {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,        // unique per project
    pub endpoint: String,    // "docker.io", "ghcr.io", "<acct>.dkr.ecr.<region>.amazonaws.com"
    pub auth_kind: RegistryAuthKind,
    pub credential_ref: Option<SecretRef>, // required unless Anonymous
}
```

`ExternalImageSource` gains two optional fields, keeps `image` and `credential`:

```rust
pub struct ExternalImageSource {
    pub image: String,                  // legacy/full-ref fallback (may be "")
    pub credential: Option<SecretRef>,  // legacy, kept
    #[serde(default)]
    pub registry_id: Option<Uuid>,
    #[serde(default)]
    pub image_ref: Option<String>,      // host-less, e.g. "library/redis:7"
}
```

**Validation** (`DomainError::RegistrySourceAmbiguous` / `RegistrySourceMissing`):

- Valid: `registry_id + image_ref` both set (new path), OR `image` non-empty
  (legacy path).
- Invalid: both paths set, or neither.

**Resolution helper** returns `(full_ref: String, auth_kind, credential_ref)`:

- New path: compose `{endpoint}/{image_ref}`, carry registry's auth.
- Legacy path: return `image`; auth is `Anonymous`, or `Basic` from legacy
  `credential` if present.

`Registry` validation: non-empty `name`, non-empty `endpoint`,
`credential_ref` present unless `auth_kind == Anonymous`.

### 2. OCI auth resolution (`src/oci/`)

`OciImagePuller::pull` takes auth explicitly — no embedded provider:

```rust
#[async_trait]
pub trait OciImagePuller: Send + Sync {
    async fn pull(&self, image: &str, auth: RegistryAuth) -> Result<PulledImage, OciError>;
    async fn read_layout(&self, layout_dir: &Path) -> Result<PulledImage, OciError>;
}
```

`RegistryImagePuller` drops its `credential_provider` field and constructor
argument; it uses the passed `auth` directly.

New resolver in `src/oci/credentials.rs`:

```rust
pub fn resolve_auth(
    kind: RegistryAuthKind,
    payload: Option<&SecretPayload>,
) -> Result<RegistryAuth, OciError> {
    match kind {
        Anonymous => Ok(RegistryAuth::Anonymous),
        Basic => {
            let raw = payload.ok_or(OciError::Pull("basic auth needs credential".into()))?;
            let (user, pass) = raw.value.split_once(':')
                .ok_or(OciError::Pull("basic credential must be 'user:password'".into()))?;
            Ok(RegistryAuth::Basic(user.into(), pass.into()))
        }
        Token   => Ok(RegistryAuth::Bearer(payload_value(payload)?)),
        EcrToken => Ok(RegistryAuth::Basic("AWS".into(), payload_value(payload)?)),
        GarToken => Ok(RegistryAuth::Basic("oauth2accesstoken".into(), payload_value(payload)?)),
    }
}
```

**Cleanup:**

- `RegistryCredentialProvider` trait + `StaticCredentialProvider` deleted (unused).
- `src/oci/ecr.rs`, `src/oci/gar.rs` env-var providers deleted. ECR/GAR become
  plain auth-kinds resolved via `resolve_auth` from a SOPS-stored pre-minted
  token.
- Cargo features `ecr` / `gar` removed (no gated provider code remains).

**Open item to confirm in plan phase:** whether `oci-client` exposes
`RegistryAuth::Bearer`. If not, `Token` falls back to `Basic` with the token as
the password (registry-dependent); resolve before implementation.

### 3. Persistence (`src/state.rs`)

Migration **v6**:

```sql
CREATE TABLE IF NOT EXISTS registries (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    name TEXT NOT NULL,
    config_json TEXT NOT NULL,
    UNIQUE(project_id, name)
);
CREATE INDEX IF NOT EXISTS idx_registries_project ON registries(project_id);
```

`config_json` holds the serialized `Registry` (consistent with services/jobs:
whole struct in a JSON column). No data backfill — legacy services keep `image`,
dual-field fallback covers them.

Store methods (mirror project_members / jobs style):

- `create_registry(&Registry)` — errors on dup `(project_id, name)`.
- `update_registry(&Registry)`.
- `delete_registry(id)` — refuses if any service in the project references the
  `registry_id` (`StateError::RegistryInUse`); prevents orphaned references.
- `registry(id) -> Option<Registry>`.
- `registries_for_project(project_id) -> Vec<Registry>`.

`StateError` gains `RegistryInUse`, `RegistryNotFound`.

### 4. API, RBAC, deploy wiring

**API** (`src/app.rs`), all guarded `ensure_role(.., project_id, Role::Admin)`:

```
GET    /v1/projects/{project_id}/registries
POST   /v1/projects/{project_id}/registries
GET    /v1/projects/{project_id}/registries/{id}
PATCH  /v1/projects/{project_id}/registries/{id}
DELETE /v1/projects/{project_id}/registries/{id}
```

- Request validation: `name` non-empty, `endpoint` non-empty, `credential_ref`
  present unless `auth_kind == Anonymous`.
- Responses **never** include decrypted credentials — only the `SecretRef` name.
- Service create/update accepts `registry_id` + `image_ref`; validates the
  `registry_id` belongs to the same project (404 `RegistryNotFound`) and runs
  the Section-1 ambiguity check.

**Deploy wiring** (`src/deploy.rs`):

`deploy_external_image_source` resolves auth before acquire:

1. If `registry_id` set: load `Registry`, decrypt `credential_ref` via
   `SopsSecretStore`, `resolve_auth(kind, payload)`, compose
   `{endpoint}/{image_ref}`.
2. Legacy path (`image` set): `RegistryAuth::Anonymous` (or `Basic` from legacy
   `credential` if present).
3. Pass `(full_ref, auth)` into the acquirer.

Acquirer methods gain an `auth: RegistryAuth` param threaded to
`puller.pull(image, auth)`:

- `acquire_rootfs_bundle_from_image_config(runner, request, auth)`
- `pull_and_unpack_external(source, auth)`
- `acquire_external_image(source, auth)`
- Git/BuildKit path passes `RegistryAuth::Anonymous` (layout read, no pull auth).

`DeployError` gains `RegistryNotFound`, `SecretDecrypt(#[from] SecretError)`.

### 5. Testing

Write tests before implementation (TDD):

- **`domain.rs`**: `Registry` validation; `ExternalImageSource` ambiguity matrix
  (both / neither / new / legacy); ref composition `{endpoint}/{image_ref}`.
- **`oci/credentials.rs`**: `resolve_auth` per kind — Basic split, malformed
  `user:password`, missing payload errors, ECR/GAR user mapping.
- **`state.rs`**: registry CRUD; dup `(project_id, name)` rejected;
  `delete_registry` blocked when referenced (`RegistryInUse`); migration v5→v6
  idempotent.
- **deploy**: external-image deploy resolves registry → auth → pull (fake
  `OciImagePuller` asserts received `RegistryAuth`); legacy `image` path →
  Anonymous; unknown `registry_id` → `RegistryNotFound`.
- **API**: registry CRUD RBAC (Viewer / Operator denied, Admin ok); credential
  never echoed in response.
- Update existing fake puller(s) in `tests/` for the new `pull(image, auth)`
  signature.

## Security invariants

- Decrypted SOPS payload is never logged; `Registry` and API responses carry
  only the `SecretRef` name.
- `resolve_auth` errors are generic ("credential invalid" / "needs credential")
  — no payload contents in error text.
- Decrypt happens at deploy time; the payload is dropped after the pull.

## ADR

New `docs/adr/012-per-service-registry.md`, amending ADR-011. Records:
project-scoped Registry entity; four auth kinds with pre-minted ECR/GAR tokens
in SOPS; removal of env-var ECR/GAR providers and `ecr`/`gar` cargo features;
`OciImagePuller::pull` signature change; dual-field backwards-compatible
migration. Update `docs/adr/README.md` and mark ADR-011 amended.

## Verification

- `cargo fmt --all`
- `cargo build`
- `cargo test`
- `cargo clippy --all-targets --all-features`
