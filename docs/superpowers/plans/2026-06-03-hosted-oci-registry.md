# Hosted OCI Registry Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Denia-hosted OCI registry support under same-origin `/v2`, backed by local `data_dir`, with metadata, conservative garbage collection, and web console visibility.

**Architecture:** Implement hosted registry as a separate backend subsystem from external pull registries. `/v2` handles Distribution-shaped upload, blob, and manifest routes with existing bearer auth and project RBAC. SQLite stores metadata; blob bytes live under `data_dir/registry`; web UI reads `/v1/registry` status/list/GC endpoints.

**Tech Stack:** Rust 2024, axum, rusqlite, serde, sha2, uuid v7, tokio fs/blocking boundaries, existing auth guards, React/TanStack Query/Effect web stack.

**Spec:** `docs/superpowers/specs/2026-06-03-client-cli-and-hosted-registry-design.md`

**ADR:** ADR-031

---

## File Structure

Create:

- `src/registry/mod.rs`: hosted registry module root.
- `src/registry/domain.rs`: repository, manifest, tag, blob, upload, and GC types.
- `src/registry/storage.rs`: filesystem paths and atomic blob writes.
- `src/registry/repo.rs`: SQLite metadata queries.
- `src/registry/api_v2.rs`: `/v2` Distribution-shaped routes.
- `src/api/hosted_registry.rs`: `/v1/registry` management/status/GC routes.
- `tests/hosted_registry_contract.rs`: backend API contract tests.
- `web/src/routes/settings/hosted-registry.tsx`: hosted registry storage/GC UI.

Modify:

- `src/app.rs`: mount public/authenticated `/v2` registry router and `/v1/registry` management router.
- `src/repo/sqlite/pool.rs`: add registry metadata migrations.
- `src/config.rs`: add registry dir and GC settings.
- `src/daemon.rs`: start periodic hosted registry GC task.
- `src/api/mod.rs`: expose `hosted_registry`.
- `src/lib.rs`: expose `registry`.
- `web/src/effect/schema.ts`, `web/src/effect/api-client.ts`, `web/src/components/Sidebar.tsx`, `web/src/routeTree.gen.ts`: add hosted registry UI/API wiring.

## Task 1: Domain And Storage Paths

**Files:**
- Create: `src/registry/domain.rs`
- Create: `src/registry/storage.rs`
- Create: `src/registry/mod.rs`
- Test: `tests/hosted_registry_storage.rs`

- [ ] **Step 1: Write storage tests**

```rust
use denia::registry::storage::RegistryStorage;

#[test]
fn blob_path_is_content_addressed() {
    let dir = tempfile::tempdir().unwrap();
    let storage = RegistryStorage::new(dir.path().to_path_buf());
    let path = storage.blob_path("sha256:abcdef").unwrap();
    assert!(path.ends_with("registry/blobs/sha256/abcdef"));
}

#[test]
fn reject_non_sha256_digest() {
    let dir = tempfile::tempdir().unwrap();
    let storage = RegistryStorage::new(dir.path().to_path_buf());
    assert!(storage.blob_path("md5:abcdef").is_err());
}
```

- [ ] **Step 2: Run failing test**

Run: `cargo test --test hosted_registry_storage`

Expected: compile failure because `registry` module does not exist.

- [ ] **Step 3: Implement domain types**

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostedRepository {
    pub id: Uuid,
    pub project_id: Uuid,
    pub service_id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostedManifest {
    pub repository_id: Uuid,
    pub digest: String,
    pub media_type: String,
    pub size: u64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostedTag {
    pub repository_id: Uuid,
    pub tag: String,
    pub manifest_digest: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostedUpload {
    pub id: Uuid,
    pub repository_id: Uuid,
    pub path: std::path::PathBuf,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

- [ ] **Step 4: Implement storage paths**

```rust
use std::path::PathBuf;
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum RegistryStorageError {
    #[error("digest must be sha256:<hex>")]
    InvalidDigest,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct RegistryStorage {
    root: PathBuf,
}

impl RegistryStorage {
    pub fn new(data_dir: PathBuf) -> Self {
        Self { root: data_dir.join("registry") }
    }

    pub fn blob_path(&self, digest: &str) -> Result<PathBuf, RegistryStorageError> {
        let hex = digest.strip_prefix("sha256:").ok_or(RegistryStorageError::InvalidDigest)?;
        if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(RegistryStorageError::InvalidDigest);
        }
        Ok(self.root.join("blobs").join("sha256").join(hex))
    }

    pub fn upload_dir(&self, upload_id: Uuid) -> PathBuf {
        self.root.join("uploads").join(upload_id.to_string())
    }
}
```

- [ ] **Step 5: Wire and run**

`src/registry/mod.rs`:

```rust
pub mod domain;
pub mod storage;
```

`src/lib.rs`:

```rust
pub mod registry;
```

Run: `cargo test --test hosted_registry_storage`

Expected: storage tests pass.

- [ ] **Step 6: Commit**

```bash
git add src/registry src/lib.rs tests/hosted_registry_storage.rs
git commit -m "feat(registry): add hosted storage paths"
```

## Task 2: SQLite Metadata

**Files:**
- Create: `src/registry/repo.rs`
- Modify: `src/repo/sqlite/pool.rs`
- Test: `tests/hosted_registry_repo.rs`

- [ ] **Step 1: Write repository tests**

```rust
use denia::registry::repo::HostedRegistryRepo;
use denia::repo::sqlite::SqlitePool;
use uuid::Uuid;

#[test]
fn repository_and_tag_roundtrip() {
    let pool = SqlitePool::open_in_memory().unwrap();
    let repo = HostedRegistryRepo::new(pool);
    let project_id = Uuid::now_v7();
    let service_id = Uuid::now_v7();
    let repository = repo.ensure_repository(project_id, service_id, "default/api").unwrap();
    repo.put_manifest(repository.id, "sha256:abc", "application/vnd.oci.image.manifest.v1+json", 100).unwrap();
    repo.put_tag(repository.id, "latest", "sha256:abc").unwrap();
    let tags = repo.tags(repository.id).unwrap();
    assert_eq!(tags[0].tag, "latest");
    assert_eq!(tags[0].manifest_digest, "sha256:abc");
}
```

- [ ] **Step 2: Run failing test**

Run: `cargo test --test hosted_registry_repo`

Expected: failure because migrations/repo do not exist.

- [ ] **Step 3: Add migration**

Add migration vNext in `src/repo/sqlite/pool.rs` creating:

```sql
CREATE TABLE IF NOT EXISTS hosted_repositories (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL,
  service_id TEXT NOT NULL,
  name TEXT NOT NULL,
  created_at TEXT NOT NULL,
  UNIQUE(project_id, service_id)
);
CREATE TABLE IF NOT EXISTS hosted_manifests (
  repository_id TEXT NOT NULL,
  digest TEXT NOT NULL,
  media_type TEXT NOT NULL,
  size INTEGER NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY(repository_id, digest)
);
CREATE TABLE IF NOT EXISTS hosted_tags (
  repository_id TEXT NOT NULL,
  tag TEXT NOT NULL,
  manifest_digest TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  PRIMARY KEY(repository_id, tag)
);
CREATE TABLE IF NOT EXISTS hosted_blobs (
  repository_id TEXT NOT NULL,
  digest TEXT NOT NULL,
  size INTEGER NOT NULL,
  created_at TEXT NOT NULL,
  PRIMARY KEY(repository_id, digest)
);
CREATE TABLE IF NOT EXISTS hosted_uploads (
  id TEXT PRIMARY KEY,
  repository_id TEXT NOT NULL,
  path TEXT NOT NULL,
  started_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS hosted_registry_gc_runs (
  id TEXT PRIMARY KEY,
  status TEXT NOT NULL,
  scanned_blobs INTEGER NOT NULL,
  deleted_blobs INTEGER NOT NULL,
  deleted_bytes INTEGER NOT NULL,
  started_at TEXT NOT NULL,
  finished_at TEXT
);
```

- [ ] **Step 4: Implement repo methods**

Implement:

```rust
pub fn ensure_repository(&self, project_id: Uuid, service_id: Uuid, name: &str) -> Result<HostedRepository, RepoError>;
pub fn put_manifest(&self, repository_id: Uuid, digest: &str, media_type: &str, size: u64) -> Result<(), RepoError>;
pub fn put_tag(&self, repository_id: Uuid, tag: &str, manifest_digest: &str) -> Result<(), RepoError>;
pub fn tags(&self, repository_id: Uuid) -> Result<Vec<HostedTag>, RepoError>;
```

Use `Uuid::now_v7()` for new repository ids and ISO/RFC3339 timestamps via
`chrono::Utc::now()`.

- [ ] **Step 5: Run and commit**

Run: `cargo test --test hosted_registry_repo`

Expected: repository metadata test passes.

Commit:

```bash
git add src/registry/repo.rs src/repo/sqlite/pool.rs tests/hosted_registry_repo.rs
git commit -m "feat(registry): add hosted metadata repo"
```

## Task 3: `/v2` Auth And Repository Resolution

**Files:**
- Create: `src/registry/api_v2.rs`
- Modify: `src/app.rs`
- Test: `tests/hosted_registry_contract.rs`

- [ ] **Step 1: Write auth tests**

```rust
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

#[tokio::test]
async fn v2_requires_bearer_auth() {
    let app = test_app_with_project_service().await;
    let resp = app.oneshot(Request::builder()
        .uri("/v2/default/api/manifests/latest")
        .body(axum::body::Body::empty())
        .unwrap()).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}
```

- [ ] **Step 2: Run failing test**

Run: `cargo test --test hosted_registry_contract v2_requires_bearer_auth`

Expected: 404 because `/v2` is not mounted.

- [ ] **Step 3: Implement `/v2` router skeleton**

Create a router with:

```rust
pub fn router() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/", axum::routing::get(v2_ping))
        .route("/{project}/{service}/manifests/{reference}", axum::routing::get(get_manifest).put(put_manifest))
        .route("/{project}/{service}/blobs/{digest}", axum::routing::get(get_blob).head(head_blob))
        .route("/{project}/{service}/blobs/uploads/", axum::routing::post(start_upload))
        .route("/{project}/{service}/blobs/uploads/{upload_id}", axum::routing::patch(patch_upload).put(commit_upload))
}
```

All handlers should require `Principal` and call a helper that resolves
project/service by names and enforces role. Return 401 for missing auth and 404
for unknown project/service.

- [ ] **Step 4: Mount router**

In `src/app.rs`, mount:

```rust
.nest("/v2", registry::api_v2::router().route_layer(middleware::from_fn_with_state(state.clone(), require_auth)))
```

Keep `/v1` routes unchanged.

- [ ] **Step 5: Run and commit**

Run: `cargo test --test hosted_registry_contract v2_requires_bearer_auth`

Expected: unauthorized test passes.

Commit:

```bash
git add src/registry/api_v2.rs src/app.rs tests/hosted_registry_contract.rs
git commit -m "feat(registry): mount hosted registry api"
```

## Task 4: Blob Upload Lifecycle

**Files:**
- Modify: `src/registry/api_v2.rs`
- Modify: `src/registry/storage.rs`
- Modify: `src/registry/repo.rs`
- Test: `tests/hosted_registry_contract.rs`

- [ ] **Step 1: Write upload lifecycle test**

Test sequence:

1. `POST /v2/default/api/blobs/uploads/` returns `202` and `Location`.
2. `PATCH <Location>` with bytes appends data.
3. `PUT <Location>?digest=sha256:<actual>` commits.
4. `GET /v2/default/api/blobs/sha256:<actual>` returns original bytes.

- [ ] **Step 2: Run failing test**

Run: `cargo test --test hosted_registry_contract upload_lifecycle`

Expected: upload lifecycle routes return placeholder 501/404.

- [ ] **Step 3: Implement upload session writes**

Use `Uuid::now_v7()` for upload id. Write bytes to
`data_dir/registry/uploads/<id>/data`. On commit, stream/hash file with SHA-256,
compare to query digest, hard-link or rename to blob path, record blob metadata,
and remove upload session metadata.

- [ ] **Step 4: Run and commit**

Run: `cargo test --test hosted_registry_contract upload_lifecycle`

Expected: upload lifecycle passes.

Commit:

```bash
git add src/registry/api_v2.rs src/registry/storage.rs src/registry/repo.rs tests/hosted_registry_contract.rs
git commit -m "feat(registry): support blob uploads"
```

## Task 5: Manifest And Tag Persistence

**Files:**
- Modify: `src/registry/api_v2.rs`
- Modify: `src/registry/repo.rs`
- Test: `tests/hosted_registry_contract.rs`

- [ ] **Step 1: Write manifest tests**

Test:

- `PUT /v2/default/api/manifests/latest` stores JSON body and returns digest.
- `GET /v2/default/api/manifests/latest` returns the same bytes and media type.
- `GET /v2/default/api/manifests/<digest>` returns the same bytes.

- [ ] **Step 2: Run failing test**

Run: `cargo test --test hosted_registry_contract manifest_roundtrip`

Expected: manifest route is not implemented.

- [ ] **Step 3: Implement manifest storage**

Hash manifest body with SHA-256. Store manifest bytes under
`data_dir/registry/manifests/sha256/<hex>` or as a content-addressed blob using
the same storage helper. Record manifest metadata and tag mapping when reference
is not a digest.

- [ ] **Step 4: Run and commit**

Run: `cargo test --test hosted_registry_contract manifest_roundtrip`

Expected: manifest tests pass.

Commit:

```bash
git add src/registry/api_v2.rs src/registry/repo.rs tests/hosted_registry_contract.rs
git commit -m "feat(registry): store hosted manifests"
```

## Task 6: Garbage Collection

**Files:**
- Create: `src/registry/gc.rs`
- Modify: `src/registry/mod.rs`
- Create: `src/api/hosted_registry.rs`
- Modify: `src/api/mod.rs`
- Modify: `src/app.rs`
- Test: `tests/hosted_registry_gc.rs`

- [ ] **Step 1: Write GC tests**

Test:

- a blob referenced by a manifest is kept;
- an unreferenced blob older than grace period is deleted;
- an active upload file is kept;
- manual GC endpoint returns scanned/deleted/reclaimed counters.

- [ ] **Step 2: Run failing test**

Run: `cargo test --test hosted_registry_gc`

Expected: compile failure because GC module does not exist.

- [ ] **Step 3: Implement conservative GC**

Define:

```rust
pub struct RegistryGcReport {
    pub scanned_blobs: u64,
    pub deleted_blobs: u64,
    pub deleted_bytes: u64,
    pub kept_referenced: u64,
    pub kept_recent: u64,
    pub kept_uploads: u64,
}
```

Compute referenced digests from manifest/blob-link/upload metadata, scan blob
files, delete only unreferenced blobs older than grace period, and persist a GC
run row.

- [ ] **Step 4: Add management endpoints**

Add:

```text
GET  /v1/registry/status
POST /v1/registry/gc
GET  /v1/registry/repositories
```

`status` and `gc` are super-admin only. Repository list is project-filtered for
non-super-admin users.

- [ ] **Step 5: Run and commit**

Run: `cargo test --test hosted_registry_gc`

Expected: GC tests pass.

Commit:

```bash
git add src/registry/gc.rs src/registry/mod.rs src/api/hosted_registry.rs src/api/mod.rs src/app.rs tests/hosted_registry_gc.rs
git commit -m "feat(registry): add hosted registry gc"
```

## Task 7: Web Console

**Files:**
- Modify: `web/src/effect/schema.ts`
- Modify: `web/src/effect/api-client.ts`
- Create: `web/src/routes/settings/hosted-registry.tsx`
- Modify: `web/src/components/Sidebar.tsx`
- Test: `web/src/routes/settings/-hosted-registry.test.tsx`

- [ ] **Step 1: Write web tests**

Test that the page renders:

- empty state when no repositories exist;
- repository row with project/service/tag/size;
- GC run button disabled while mutation is pending.

- [ ] **Step 2: Add schemas**

Add Effect schemas:

```ts
export class HostedRegistryStatus extends Schema.Class<HostedRegistryStatus>('HostedRegistryStatus')({
  repositories: Schema.Number,
  blobs: Schema.Number,
  total_bytes: Schema.Number,
  last_gc_at: Schema.NullOr(Schema.String),
  last_gc_deleted_bytes: Schema.Number,
}) {}

export class HostedRepository extends Schema.Class<HostedRepository>('HostedRepository')({
  project_id: Schema.String,
  project_name: Schema.String,
  service_id: Schema.String,
  service_name: Schema.String,
  repository: Schema.String,
  tags: Schema.Array(Schema.Struct({
    tag: Schema.String,
    digest: Schema.String,
    size: Schema.Number,
    updated_at: Schema.String,
  })),
}) {}
```

- [ ] **Step 3: Add API client methods**

Add:

```ts
readonly getHostedRegistryStatus: Effect.Effect<HostedRegistryStatus, ApiError | DecodeError>
readonly listHostedRepositories: Effect.Effect<ReadonlyArray<HostedRepository>, ApiError | DecodeError>
readonly runHostedRegistryGc: Effect.Effect<HostedRegistryStatus, ApiError | DecodeError>
```

- [ ] **Step 4: Add route and sidebar link**

Create `/settings/hosted-registry` following the existing
`settings/oci-cache.tsx` layout: compact status stats, repository table, and a
manual GC confirm button.

- [ ] **Step 5: Run and commit**

Run:

```bash
cd web && pnpm test -- --run hosted-registry
cd web && pnpm build
```

Expected: hosted registry tests and web build pass.

Commit:

```bash
git add web/src/effect/schema.ts web/src/effect/api-client.ts web/src/routes/settings/hosted-registry.tsx web/src/components/Sidebar.tsx web/src/routes/settings/-hosted-registry.test.tsx
git commit -m "feat(web): show hosted registry status"
```

## Task 8: Full Verification

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Document hosted registry**

Add README sections for:

- same-origin `/v2`;
- bearer API token auth;
- `<project>/<service>` naming;
- local `data_dir/registry` storage;
- manual and periodic GC.

- [ ] **Step 2: Run verification**

Run:

```bash
cargo fmt --all
cargo test
cargo clippy --all-targets --all-features
cd web && pnpm build
```

Expected: all commands pass.

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs(registry): document hosted registry"
```

## Self-Review

- Spec coverage: `/v2`, same-origin routing, bearer auth, project/service
  naming, local storage, metadata, GC, and UI are covered.
- Type consistency: hosted repository, manifest, upload, storage, repo, GC, and
  API names stay consistent across tasks.
- Placeholder scan: every task names exact files, commands, expected outcomes,
  and concrete route/type shapes.
