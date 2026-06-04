# `denia auth` + `denia push` (Working-Tree Upload Deploy) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a Vercel-style local deploy loop — `denia auth` mints a long-lived API token; `denia push` packs the working tree, uploads it, and the server builds the uploaded Dockerfile context and deploys it.

**Architecture:** Client-side: two new subcommands on the existing unified `denia` binary reusing the `src/cli/client/` scaffolding. Server-side: a hardened upload endpoint stages a `tar.zst` build context, a new `UploadedContext` artifact source builds it via the existing BuildKit invocation (reusing `confine_under`), and the existing async-deploy + SSE-log pipeline carries it to `Healthy`.

**Tech Stack:** Rust 2024, axum 0.8, tokio, `tar` + `zstd` (present), `reqwest` (present, rustls), `crossterm` (present — password no-echo), new dep `ignore` (gitignore/dockerignore matching), `httpmock`/`assert_cmd` (dev-deps, present).

**Spec:** `docs/superpowers/specs/2026-06-03-denia-push-working-tree-deploy-design.md`
**ADR:** `docs/adr/034-client-driven-deploy-upload.md`

---

## Build & Test Environment (read first)

The repo `target/` may be root-owned; cargo then fails permission-denied. Build/test with a shared writable target dir:

```bash
export CARGO_TARGET_DIR=/tmp/denia-verify
```

A debug build needs the gitignored SPA bundle to exist (rust-embed `#[folder = "web/dist/client"]`). It has already been copied into this worktree at `web/dist/client`. If a clean checkout lacks it: `cp -r ~/Project/denia/web/dist/client web/dist/client` (or `cd web && pnpm build`).

Standard per-task verification (unless a task says otherwise):

```bash
CARGO_TARGET_DIR=/tmp/denia-verify cargo test <test-name> -- --nocolor
CARGO_TARGET_DIR=/tmp/denia-verify cargo build
```

Privileged runtime tests are root-gated and run only in the final task:

```bash
DENIA_RUN_PRIVILEGED_TESTS=1 cargo test --test linux_runtime_privileged -- --ignored
```

Commit format: `<type>(<scope>): message` (`feat`/`fix`/`docs`/`test`/`refactor`). All persisted IDs are UUIDv7 (`Uuid::now_v7()`). Never log passwords, tokens, or decrypted payloads.

---

## File Structure

**Server (modify):**
- `Cargo.toml` — add `ignore = "0.4"`.
- `src/config.rs` — upload caps + uploads dir config fields.
- `src/artifacts/mod.rs` — `ArtifactSource::UploadedContext` variant.
- `src/artifacts/acquirer.rs` — `ArtifactAcquireRequest::Upload`, `acquire_staged()`, dispatch arms.
- `src/domain/deployment.rs` — `DeploymentRequest::Upload` variant + `service_id()`.
- `src/deploy/coordinator.rs` — `Upload` arm in `run_inner_with_deps`.
- `src/api/deployments.rs` — accept `Upload`; staged-dir cleanup after the deploy task.
- `src/app.rs` — merge `api::uploads::router()` into `authed`.

**Server (create):**
- `src/api/uploads.rs` — `POST /v1/services/{id}/uploads` + hardened extraction.

**Client (modify):**
- `src/cli/client/profile.rs` — config writer.
- `src/cli/client/manifest.rs` — build-config fields.
- `src/cli/client/http.rs` — `login`, `create_api_token`, `me`, `create_project`, `create_service`, `upload_context`, `create_deployment`, `stream_deployment_log`.
- `src/cli/client/mod.rs` — `pub mod auth; pub mod push; pub mod pack;`.
- `src/cli/mod.rs` — `Commands::Auth`, `Commands::Push` + dispatch.

**Client (create):**
- `src/cli/client/auth.rs`, `src/cli/client/push.rs`, `src/cli/client/pack.rs`.

**Docs (modify):** `README.md` (Features, CLI table, API highlights, "Deploy from your machine" section).

---

## Phase 0 — Dependencies & Config

### Task 0.1: Add the `ignore` crate

**Files:** Modify `Cargo.toml`

- [ ] **Step 1: Add the dependency** under `[dependencies]` (keep alphabetical-ish grouping near other utility crates):

```toml
# gitignore/.dockerignore-aware working-tree walking for `denia push` (ADR-034).
ignore = "0.4"
```

- [ ] **Step 2: Verify it resolves**

Run: `CARGO_TARGET_DIR=/tmp/denia-verify cargo build`
Expected: builds; `ignore` appears in `Cargo.lock`.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "build: add ignore crate for working-tree packing (ADR-034)"
```

### Task 0.2: Upload config fields

**Files:** Modify `src/config.rs`

Follow the existing config pattern exactly: a field on the `FileConfig` TOML struct, the same field on the resolved `AppConfig`, a `DENIA_*` env override, a default, and inclusion in the default template the daemon writes on first boot. Add:

| AppConfig field | TOML key | Env override | Default |
|---|---|---|---|
| `uploads_dir: PathBuf` | `uploads_dir` | `DENIA_UPLOADS_DIR` | `<data_dir>/uploads` |
| `upload_max_bytes: u64` | `upload_max_bytes` | `DENIA_UPLOAD_MAX_BYTES` | `536870912` (512 MiB, compressed body cap) |
| `upload_max_uncompressed_bytes: u64` | `upload_max_uncompressed_bytes` | `DENIA_UPLOAD_MAX_UNCOMPRESSED_BYTES` | `2147483648` (2 GiB) |
| `upload_max_entries: u64` | `upload_max_entries` | `DENIA_UPLOAD_MAX_ENTRIES` | `200000` |
| `upload_ttl_secs: u64` | `upload_ttl_secs` | `DENIA_UPLOAD_TTL_SECS` | `3600` |

- [ ] **Step 1: Write a failing test** in the `src/config.rs` test module (mirror an existing default-value test):

```rust
#[test]
fn upload_defaults_are_populated() {
    let cfg = AppConfig::from_file_config(FileConfig::default(), &test_data_dir());
    assert_eq!(cfg.uploads_dir, cfg.data_dir.join("uploads"));
    assert_eq!(cfg.upload_max_bytes, 536_870_912);
    assert_eq!(cfg.upload_max_entries, 200_000);
}
```

(Adapt `from_file_config`/`test_data_dir` to the actual constructor names in `src/config.rs`.)

- [ ] **Step 2: Run it, confirm it fails to compile** (fields don't exist).

Run: `CARGO_TARGET_DIR=/tmp/denia-verify cargo test upload_defaults_are_populated`
Expected: compile error / FAIL.

- [ ] **Step 3: Add the fields** to `FileConfig` (with `#[serde(default = "...")]` defaults like siblings) and `AppConfig`, the env overrides in the env-merge function, and the default-template writer.

- [ ] **Step 4: Run the test**

Run: `CARGO_TARGET_DIR=/tmp/denia-verify cargo test upload_defaults_are_populated`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat(config): add upload staging dir + size/entry/ttl caps (ADR-034)"
```

---

## Phase 1 — Artifact source: build from a staged context

### Task 1.1: `ArtifactSource::UploadedContext`

**Files:** Modify `src/artifacts/mod.rs:14-26`

- [ ] **Step 1:** Add the variant to the `#[serde(tag = "type", rename_all = "snake_case")]` enum:

```rust
    UploadedContext {
        upload_id: String,
        dockerfile_path: String,
        context_path: String,
    },
```

- [ ] **Step 2: Build** — `CARGO_TARGET_DIR=/tmp/denia-verify cargo build`. Expected: compiles (no match yet consumes it).

- [ ] **Step 3: Commit**

```bash
git add src/artifacts/mod.rs
git commit -m "feat(artifacts): add UploadedContext source variant (ADR-034)"
```

### Task 1.2: `acquire_staged` + dispatch arms

**Files:** Modify `src/artifacts/acquirer.rs` (`ArtifactAcquireRequest` L42, `acquire()` L138, `acquire_rootfs_bundle_from_image_config()` L198, add `acquire_staged` near `build_from_git_checkout` L333; Test: same file test module)

The Git path runs `buildctl build --frontend dockerfile.v0 --local context=<dir> --local dockerfile=<dir> --output type=oci,dest=<artifact_dir>` over a git checkout (`build_from_git_checkout`, L344-384). `acquire_staged` does the same over an already-extracted upload dir resolved from `config.uploads_dir`.

- [ ] **Step 1: Write a failing test** (uses a `FakeCommandRunner` already present in the acquirer tests; assert the `buildctl` args reference the staged context dir and that paths are confined):

```rust
#[tokio::test]
async fn acquire_staged_runs_buildctl_over_staged_context() {
    // Arrange: a config whose uploads_dir/<id>/context exists with a Dockerfile.
    // Use the same FakeCommandRunner pattern as the git acquire tests.
    let (acquirer, runner, upload_id, _tmp) = staged_fixture("Dockerfile", ".");
    let source = ArtifactSource::UploadedContext {
        upload_id: upload_id.clone(),
        dockerfile_path: "Dockerfile".into(),
        context_path: ".".into(),
    };
    let digest = acquirer.acquire_staged(&runner, &source).await.unwrap();
    assert!(!digest.is_empty());
    let buildctl = runner.last_invocation_for("buildctl");
    assert!(buildctl.args.iter().any(|a| a.contains("context=")
        && a.contains(&format!("uploads/{upload_id}/context"))));
}
```

(Write `staged_fixture` in the test module: builds an `AppConfig` with a temp `uploads_dir`, creates `<uploads_dir>/<id>/context/Dockerfile`, returns the acquirer built via `ArtifactAcquirer::with_traits` with fakes.)

- [ ] **Step 2: Run it, confirm it fails** (method missing).

Run: `CARGO_TARGET_DIR=/tmp/denia-verify cargo test acquire_staged_runs_buildctl_over_staged_context`
Expected: FAIL (no method `acquire_staged`).

- [ ] **Step 3: Add `ArtifactAcquireRequest::Upload`** to the enum (L42):

```rust
    Upload {
        upload_id: String,
        dockerfile_path: String,
        context_path: String,
    },
```

- [ ] **Step 4: Implement `acquire_staged`** (model on `build_from_git_checkout` L333-385, dropping the clone/checkout):

```rust
async fn acquire_staged(
    &self,
    runner: &dyn CommandRunner,
    source: &ArtifactSource,
) -> Result<String, ArtifactAcquireError> {
    let ArtifactSource::UploadedContext { upload_id, dockerfile_path, context_path } = source
    else {
        unreachable!("staged acquisition requires an uploaded-context source");
    };
    // Resolve the server-owned staged dir; never trust a client path.
    let staged = self.config.uploads_dir.join(upload_id).join("context");
    let context_dir = confine_under(&staged, context_path)?;
    let dockerfile_dir = confine_under(&staged, dockerfile_path)?;

    let context = format!("context={}", context_dir.to_string_lossy());
    let dockerfile = format!("dockerfile={}", dockerfile_dir.to_string_lossy());
    let output = format!("type=oci,dest={}", self.config.artifact_dir.to_string_lossy());
    let program = self.config.buildkit_binary.to_string_lossy();
    let args = [
        "build", "--frontend", "dockerfile.v0",
        "--local", context.as_str(),
        "--local", dockerfile.as_str(),
        "--output", output.as_str(),
    ];
    let out = runner.run(program.as_ref(), &args).await?;
    Ok(out.stdout.trim().to_string())
}
```

- [ ] **Step 5: Add the `acquire()` arm** (L138 match), mirroring the Git arm:

```rust
    ArtifactAcquireRequest::Upload { upload_id, dockerfile_path, context_path } => {
        let source = ArtifactSource::UploadedContext { upload_id, dockerfile_path, context_path };
        let digest = self.acquire_staged(runner, &source).await?;
        Ok(ArtifactRecord::new(digest, ArtifactKind::OciImage, source)?)
    }
```

- [ ] **Step 6: Extend `acquire_rootfs_bundle_from_image_config`** (L198 match): collapse the Git arm to cover Upload too, since both build locally then materialize:

```rust
    ArtifactAcquireRequest::Git { .. } | ArtifactAcquireRequest::Upload { .. } => {
        let _ = auth;
        let image_artifact = self.acquire(runner, request).await?;
        let _bundle_dir = self.materialize_rootfs_bundle_inprocess(&image_artifact).await?;
        ArtifactRecord::new(image_artifact.digest, ArtifactKind::RootfsBundle, image_artifact.source)
            .map_err(ArtifactAcquireError::Artifact)
    }
```

- [ ] **Step 7: Run the test**

Run: `CARGO_TARGET_DIR=/tmp/denia-verify cargo test acquire_staged_runs_buildctl_over_staged_context`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add src/artifacts/acquirer.rs
git commit -m "feat(artifacts): build OCI image from a staged upload context (ADR-034)"
```

---

## Phase 2 — Upload endpoint with hardened extraction

### Task 2.1: Hardened extraction helper

**Files:** Create `src/api/uploads.rs` (extraction fn + tests)

Security: v1 accepts **only** regular files and directories. Symlinks, hardlinks, device/fifo/special entries, absolute paths, and `..` components are rejected. Uncompressed-size and entry-count caps guard against zip bombs. This sidesteps symlink-escape entirely (documented limitation: build contexts needing symlinks are unsupported in v1).

- [ ] **Step 1: Write failing tests** for the extractor:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn tar_zst(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut tar = tar::Builder::new(Vec::new());
        for (path, body) in entries {
            let mut h = tar::Header::new_gnu();
            h.set_size(body.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            tar.append_data(&mut h, path, *body).unwrap();
        }
        let tar = tar.into_inner().unwrap();
        zstd::stream::encode_all(&tar[..], 0).unwrap()
    }

    #[test]
    fn extracts_regular_files() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = tar_zst(&[("Dockerfile", b"FROM scratch\n"), ("src/main.rs", b"fn main(){}")]);
        let limits = ExtractLimits { max_uncompressed: 1 << 20, max_entries: 100 };
        extract_tar_zst(&bytes, dir.path(), &limits).unwrap();
        assert!(dir.path().join("Dockerfile").exists());
        assert!(dir.path().join("src/main.rs").exists());
    }

    #[test]
    fn rejects_parent_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = tar_zst(&[("../escape", b"x")]);
        let limits = ExtractLimits { max_uncompressed: 1 << 20, max_entries: 100 };
        assert!(extract_tar_zst(&bytes, dir.path(), &limits).is_err());
    }

    #[test]
    fn rejects_too_many_entries() {
        let dir = tempfile::tempdir().unwrap();
        let many: Vec<(String, Vec<u8>)> = (0..10).map(|i| (format!("f{i}"), vec![0u8])).collect();
        let refs: Vec<(&str, &[u8])> = many.iter().map(|(p, b)| (p.as_str(), b.as_slice())).collect();
        let bytes = tar_zst(&refs);
        let limits = ExtractLimits { max_uncompressed: 1 << 20, max_entries: 3 };
        assert!(extract_tar_zst(&bytes, dir.path(), &limits).is_err());
    }

    #[test]
    fn rejects_oversize_uncompressed() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = tar_zst(&[("big", &vec![7u8; 4096])]);
        let limits = ExtractLimits { max_uncompressed: 1024, max_entries: 100 };
        assert!(extract_tar_zst(&bytes, dir.path(), &limits).is_err());
    }
}
```

(Add a `rejects_symlink` test once you confirm how to author a symlink tar entry with `tar::Header::set_entry_type(tar::EntryType::Symlink)` + `set_link_name`.)

- [ ] **Step 2: Run, confirm fail** (fn missing).

Run: `CARGO_TARGET_DIR=/tmp/denia-verify cargo test -p denia uploads::tests`
Expected: FAIL.

- [ ] **Step 3: Implement** the extractor:

```rust
use std::io::Read;
use std::path::{Component, Path};

pub struct ExtractLimits {
    pub max_uncompressed: u64,
    pub max_entries: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    #[error("io: {0}")] Io(#[from] std::io::Error),
    #[error("archive rejected: {0}")] Rejected(String),
}

/// Extract a `tar.zst` into `dest`, accepting only regular files and dirs.
pub fn extract_tar_zst(bytes: &[u8], dest: &Path, limits: &ExtractLimits) -> Result<(), ExtractError> {
    let decoder = zstd::stream::read::Decoder::new(bytes)?;
    let mut archive = tar::Archive::new(decoder);
    archive.set_preserve_permissions(false);
    archive.set_unpack_xattrs(false);
    archive.set_overwrite(true);

    let mut entries = archive.entries()?;
    let mut count: u64 = 0;
    let mut total: u64 = 0;
    while let Some(entry) = entries.next() {
        let mut entry = entry?;
        count += 1;
        if count > limits.max_entries {
            return Err(ExtractError::Rejected("too many entries".into()));
        }
        let etype = entry.header().entry_type();
        if !(etype.is_file() || etype.is_dir()) {
            return Err(ExtractError::Rejected(format!("disallowed entry type: {etype:?}")));
        }
        let path = entry.path()?.into_owned();
        for c in path.components() {
            match c {
                Component::Normal(_) | Component::CurDir => {}
                _ => return Err(ExtractError::Rejected(format!("unsafe path: {}", path.display()))),
            }
        }
        total = total.saturating_add(entry.header().size()?);
        if total > limits.max_uncompressed {
            return Err(ExtractError::Rejected("uncompressed size cap exceeded".into()));
        }
        // unpack_in re-checks containment and refuses to escape `dest`.
        if !entry.unpack_in(dest)? {
            return Err(ExtractError::Rejected(format!("entry skipped/escaped: {}", path.display())));
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Run tests, confirm pass.**

Run: `CARGO_TARGET_DIR=/tmp/denia-verify cargo test -p denia uploads::tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/api/uploads.rs
git commit -m "feat(api): hardened tar.zst extraction for build-context uploads (ADR-034)"
```

### Task 2.2: Upload route + body cap + role check

**Files:** Modify `src/api/uploads.rs` (router + handler), `src/app.rs:387` (merge router), `src/api/mod.rs` (add `pub mod uploads;` if module list is centralized — check how `console` is declared). Test: `src/api/uploads.rs` test module (httpmock not needed — use `build_router` + `tower::ServiceExt::oneshot`, mirroring `src/api/console.rs:380-595`).

Handler contract:
- Path `service_id`; resolve service; `ensure_role(state, principal, service.project_id, Role::Operator)`.
- Stream the request body to `<uploads_dir>/<uuidv7>/context.tar.zst`, aborting + deleting if it exceeds `config.upload_max_bytes` → `413`.
- `extract_tar_zst` into `<uploads_dir>/<id>/context/` with the configured caps → `400` on `ExtractError::Rejected`.
- Delete the `.tar.zst` after successful extraction (keep only `context/`).
- Respond `200 { "upload_id": "<uuidv7>", "expires_at": "<rfc3339>" }` where `expires_at = now + upload_ttl_secs`.

- [ ] **Step 1: Write failing tests** (mirror `console.rs` test helpers `test_state`, `build_router`, `ADMIN_TOKEN`, `body_json`):
  - `operator_upload_returns_upload_id` — POST a small valid `tar.zst` with admin bearer → `200`, body has `upload_id`; `<uploads_dir>/<id>/context/Dockerfile` exists.
  - `viewer_cannot_upload` — viewer principal → `403`.
  - `oversize_body_returns_413` — set `config.upload_max_bytes` tiny → `413`.

- [ ] **Step 2: Run, confirm fail.**

Run: `CARGO_TARGET_DIR=/tmp/denia-verify cargo test -p denia uploads`
Expected: FAIL.

- [ ] **Step 3: Implement** `router()` and the handler. Use `axum::body::Body::into_data_stream()` and fold chunks into the file while tracking bytes; on cap breach return `ApiError`-mapped `413`. Add `pub fn router() -> Router<AppState>` with `.route("/services/{service_id}/uploads", post(upload_handler))`.

- [ ] **Step 4: Merge the router** in `src/app.rs` `authed` chain (after `.merge(api::deployments::router())`, L389):

```rust
        .merge(api::uploads::router())
```

Declare the module wherever `console` is declared (e.g. `src/api/mod.rs`).

- [ ] **Step 5: Run tests, confirm pass.**

Run: `CARGO_TARGET_DIR=/tmp/denia-verify cargo test -p denia uploads`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/api/uploads.rs src/app.rs src/api/mod.rs
git commit -m "feat(api): add POST /v1/services/{id}/uploads endpoint (ADR-034)"
```

### Task 2.3: Expired-upload cleanup task

**Files:** Modify `src/daemon.rs` (spawn a periodic sweep, mirror the registry GC task pattern), or add a small `sweep_expired_uploads(uploads_dir, ttl)` helper in `src/api/uploads.rs` and call it from the daemon boot.

- [ ] **Step 1:** Add `pub fn sweep_expired_uploads(uploads_dir: &Path, max_age: Duration)` that removes upload dirs whose mtime is older than `max_age`. Unit-test it with two temp dirs (one back-dated via `filetime` or by asserting a freshly-created one survives and a manually-removed-mtime one is collected — keep the test simple: create dir, call with `Duration::ZERO`, assert removed).
- [ ] **Step 2:** Spawn it on an interval in the daemon boot alongside other periodic tasks (find the registry GC `tokio::spawn` loop and follow it).
- [ ] **Step 3: Build + test.**

Run: `CARGO_TARGET_DIR=/tmp/denia-verify cargo test sweep_expired_uploads && CARGO_TARGET_DIR=/tmp/denia-verify cargo build`

- [ ] **Step 4: Commit**

```bash
git add src/api/uploads.rs src/daemon.rs
git commit -m "feat: periodically sweep expired build-context uploads (ADR-034)"
```

---

## Phase 3 — Deployment request + coordinator

### Task 3.1: `DeploymentRequest::Upload`

**Files:** Modify `src/domain/deployment.rs:5-32`

- [ ] **Step 1: Write a failing test** in the deployment domain test module (or add one):

```rust
#[test]
fn upload_request_round_trips_and_exposes_service_id() {
    let sid = Uuid::now_v7();
    let req = DeploymentRequest::Upload {
        service_id: sid,
        upload_id: "abc".into(),
        dockerfile_path: "Dockerfile".into(),
        context_path: ".".into(),
    };
    assert_eq!(req.service_id(), sid);
    let json = serde_json::to_string(&req).unwrap();
    assert!(json.contains("\"source\":\"upload\""));
    let back: DeploymentRequest = serde_json::from_str(&json).unwrap();
    assert_eq!(back, req);
}
```

- [ ] **Step 2: Run, confirm fail.**

Run: `CARGO_TARGET_DIR=/tmp/denia-verify cargo test upload_request_round_trips`
Expected: FAIL.

- [ ] **Step 3: Add the variant** to the `#[serde(tag = "source", rename_all = "snake_case")]` enum and extend `service_id()`:

```rust
    Upload {
        service_id: Uuid,
        upload_id: String,
        dockerfile_path: String,
        context_path: String,
    },
```

```rust
    pub fn service_id(&self) -> Uuid {
        match self {
            Self::Git { service_id, .. }
            | Self::ExternalImage { service_id, .. }
            | Self::Upload { service_id, .. } => *service_id,
        }
    }
```

- [ ] **Step 4: Run, confirm pass.**

Run: `CARGO_TARGET_DIR=/tmp/denia-verify cargo test upload_request_round_trips`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/domain/deployment.rs
git commit -m "feat(domain): add Upload deployment request variant (ADR-034)"
```

### Task 3.2: Coordinator `Upload` arm

**Files:** Modify `src/deploy/coordinator.rs:180-220` (`run_inner_with_deps` match)

Unlike Git/ExternalImage, the Upload arm does **not** require a matching `service.source` — the build inputs come entirely from the request + staged dir.

- [ ] **Step 1:** Add the arm to the `match &request` block:

```rust
    DeploymentRequest::Upload { upload_id, dockerfile_path, context_path, .. } => {
        deps.acquirer
            .acquire_rootfs_bundle_from_image_config(
                deps.runner,
                ArtifactAcquireRequest::Upload {
                    upload_id: upload_id.clone(),
                    dockerfile_path: dockerfile_path.clone(),
                    context_path: context_path.clone(),
                },
                RegistryAuth::Anonymous,
            )
            .await?
    }
```

- [ ] **Step 2: Build.** Run: `CARGO_TARGET_DIR=/tmp/denia-verify cargo build`. Expected: compiles (match now exhaustive).
- [ ] **Step 3:** Add a coordinator test mirroring `deploy_git_source` tests if a `FakeRuntime`/`FakeCommandRunner` fixture makes it cheap; otherwise rely on the Phase-1 acquirer test + the Phase-8 privileged E2E. (Do not fake your way to a false positive — if a unit test can't exercise it honestly, say so in the commit body.)
- [ ] **Step 4: Commit**

```bash
git add src/deploy/coordinator.rs
git commit -m "feat(deploy): build + deploy from an uploaded context (ADR-034)"
```

### Task 3.3: Staged-dir cleanup after deploy

**Files:** Modify `src/api/deployments.rs:70-153` (`create_deployment` spawned task)

The spawned task already runs `run_with_deps` then an autoscale block. After the run completes, if the request was `Upload`, delete `<uploads_dir>/<upload_id>`.

- [ ] **Step 1:** Capture the upload id before the spawn:

```rust
    let upload_cleanup = match &request {
        crate::domain::DeploymentRequest::Upload { upload_id, .. } => {
            Some(state.config.uploads_dir.join(upload_id))
        }
        _ => None,
    };
```

- [ ] **Step 2:** Inside the `tokio::spawn` move block, after `run` completes (success or failure), best-effort remove it:

```rust
        if let Some(dir) = upload_cleanup {
            let _ = std::fs::remove_dir_all(&dir);
        }
```

(Ensure `upload_cleanup` is moved into the task — add it to the captured set.)

- [ ] **Step 3: Build.** Run: `CARGO_TARGET_DIR=/tmp/denia-verify cargo build`. Expected: compiles.
- [ ] **Step 4: Commit**

```bash
git add src/api/deployments.rs
git commit -m "feat(api): clean up staged upload after deploy completes (ADR-034)"
```

---

## Phase 4 — Client config writer

### Task 4.1: `ClientConfig` writer

**Files:** Modify `src/cli/client/profile.rs` (add `Serialize` to `Profile`/`ClientConfig`, writer methods; tests in same file)

- [ ] **Step 1: Write failing tests:**

```rust
#[test]
fn upsert_set_active_and_save_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("client.toml");
    let mut cfg = ClientConfig::default();
    cfg.upsert_profile("prod", Profile { url: "https://x".into(), token: "t".into() });
    cfg.set_active("prod");
    cfg.save_to(&path).unwrap();
    let back = ClientConfig::load_from(&path).unwrap();
    assert_eq!(back.active.as_deref(), Some("prod"));
    assert_eq!(back.active_profile().unwrap().token, "t");
}

#[cfg(unix)]
#[test]
fn save_sets_owner_only_perms() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("client.toml");
    let mut cfg = ClientConfig::default();
    cfg.upsert_profile("p", Profile { url: "u".into(), token: "t".into() });
    cfg.save_to(&path).unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}
```

- [ ] **Step 2: Run, confirm fail.**

Run: `CARGO_TARGET_DIR=/tmp/denia-verify cargo test -p denia profile::`
Expected: FAIL.

- [ ] **Step 3: Implement.** Add `#[derive(Serialize)]` (alongside `Deserialize`) to `Profile` and `ClientConfig`, then:

```rust
impl ClientConfig {
    pub fn upsert_profile(&mut self, name: &str, profile: Profile) {
        self.profiles.insert(name.to_string(), profile);
    }
    pub fn set_active(&mut self, name: &str) {
        self.active = Some(name.to_string());
    }
    pub fn save_to(&self, path: &std::path::Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let toml = toml::to_string_pretty(self)?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, toml.as_bytes())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}
```

(`ClientConfig` already derives `Default`. `BTreeMap` serializes fine.)

- [ ] **Step 4: Run, confirm pass.**

Run: `CARGO_TARGET_DIR=/tmp/denia-verify cargo test -p denia profile::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/cli/client/profile.rs
git commit -m "feat(cli): writable client profile store (ADR-034)"
```

---

## Phase 5 — `.denia` manifest build config

### Task 5.1: Extend `DeniaManifest`

**Files:** Modify `src/cli/client/manifest.rs`

- [ ] **Step 1: Write failing tests:**

```rust
#[test]
fn build_fields_default_when_absent() {
    let m = DeniaManifest::parse("project=\"p\"\nservice=\"s\"\n").unwrap();
    assert_eq!(m.dockerfile(), "Dockerfile");
    assert_eq!(m.context(), ".");
    assert!(m.create.is_none());
}

#[test]
fn parses_build_and_create_blocks() {
    let raw = "project=\"p\"\nservice=\"s\"\ndockerfile=\"docker/Dockerfile\"\ncontext=\"app\"\n[create]\nport=8080\nhealth_path=\"/healthz\"\n";
    let m = DeniaManifest::parse(raw).unwrap();
    assert_eq!(m.dockerfile(), "docker/Dockerfile");
    assert_eq!(m.context(), "app");
    assert_eq!(m.create.as_ref().unwrap().port, 8080);
}
```

- [ ] **Step 2: Run, confirm fail.**

Run: `CARGO_TARGET_DIR=/tmp/denia-verify cargo test -p denia manifest::`
Expected: FAIL.

- [ ] **Step 3: Implement** (keep required `project`/`service`; add optional fields + `Serialize` for `--create` write-back):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeniaManifest {
    pub project: String,
    pub service: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dockerfile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create: Option<CreateDefaults>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateDefaults {
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health_path: Option<String>,
}

impl DeniaManifest {
    pub fn dockerfile(&self) -> &str { self.dockerfile.as_deref().unwrap_or("Dockerfile") }
    pub fn context(&self) -> &str { self.context.as_deref().unwrap_or(".") }
    pub fn write_to(&self, path: &std::path::Path) -> anyhow::Result<()> {
        std::fs::write(path, toml::to_string_pretty(self)?)?;
        Ok(())
    }
}
```

(Add `use serde::Serialize;`.)

- [ ] **Step 4: Run, confirm pass.**

Run: `CARGO_TARGET_DIR=/tmp/denia-verify cargo test -p denia manifest::`
Expected: PASS (including the existing `parses_project_and_service` / `rejects_missing_fields`).

- [ ] **Step 5: Commit**

```bash
git add src/cli/client/manifest.rs
git commit -m "feat(cli): extend .denia manifest with build + create config (ADR-034)"
```

---

## Phase 6 — `denia auth`

### Task 6.1: `ClientApi` auth methods

**Files:** Modify `src/cli/client/http.rs` (Test: same file with `httpmock`)

Add typed methods + small response structs:

> Note: `/v1/auth/login` returns `LoginResult { token, expires_at }`. The struct below only reads `token`; serde ignores the extra field, so this works as-is.

```rust
#[derive(serde::Deserialize)]
pub struct LoginResponse { pub token: String }
#[derive(serde::Deserialize)]
pub struct ApiTokenResponse { pub id: String, pub name: String, pub token: String }
#[derive(serde::Deserialize)]
pub struct MeResponse { pub principal: serde_json::Value } // shape unused; presence = token valid

impl ClientApi {
    pub async fn login(&self, username: &str, password: &str) -> Result<LoginResponse, ClientApiError> {
        // POST /v1/auth/login WITHOUT bearer
        let resp = self.http.post(format!("{}/v1/auth/login", self.base_url))
            .json(&serde_json::json!({ "username": username, "password": password }))
            .send().await?;
        Self::json_or_err(resp).await
    }
    pub async fn create_api_token(&self, bearer: &str, name: &str) -> Result<ApiTokenResponse, ClientApiError> {
        self.post_json("/v1/api-tokens", bearer, &serde_json::json!({ "name": name })).await
    }
    pub async fn me(&self, bearer: &str) -> Result<serde_json::Value, ClientApiError> {
        self.get_json("/v1/me", bearer).await
    }
}
```

(Add a `json_or_err` helper if not present; reuse the existing status-check pattern from `get_json`/`post_json`.)

- [ ] **Step 1: Write failing tests** with `httpmock`: mock `/v1/auth/login` → `{token}`, `/v1/api-tokens` → `{id,name,token}`, `/v1/me` → `{}`; assert each method parses.
- [ ] **Step 2: Run, confirm fail.** `CARGO_TARGET_DIR=/tmp/denia-verify cargo test -p denia http::`
- [ ] **Step 3: Implement** the methods.
- [ ] **Step 4: Run, confirm pass.**
- [ ] **Step 5: Commit**

```bash
git add src/cli/client/http.rs
git commit -m "feat(cli): client API methods for login, token mint, me (ADR-034)"
```

### Task 6.2: `denia auth` command

**Files:** Create `src/cli/client/auth.rs`; modify `src/cli/client/mod.rs` (`pub mod auth;`)

Password is read without echo via `crossterm` (already a dep): enable raw mode, read a line, disable raw mode. Provide a helper `read_password(prompt: &str) -> io::Result<String>`. Non-interactive override: accept `--password-stdin` (read one line from stdin) for scripting/tests.

`AuthArgs`: `--url`, `--username`, `--profile`, `--token-name`, `--password-stdin`.

Flow (no echo of secrets; never log token):
1. URL: flag or prompt; `trim_end_matches('/')`.
2. `ClientApi::new(&url)`; probe `GET /healthz` (add a tiny `healthz()` method or reuse `get_json` to `/healthz`) — fail with a clear message if unreachable.
3. username (flag or prompt), password (`--password-stdin` or `read_password`).
4. `login` → session token.
5. `create_api_token(session, token_name)` → long-lived token.
6. `me(token)` to verify.
7. `ClientConfig::load_from(config_path()).unwrap_or_default()`, `upsert_profile(profile_name, Profile{url, token})`, `set_active(profile_name)`, `save_to(config_path())`.
8. Print: `Authenticated as <username>; profile '<name>' saved to <path>.` (no token printed).

- [ ] **Step 1:** Write an integration test `tests/cli_auth.rs` using `assert_cmd` + `httpmock`: set `DENIA_CLIENT_CONFIG` to a temp path, run `denia auth --url <mock> --username u --password-stdin --profile test` piping the password on stdin; assert exit 0 and the temp `client.toml` contains the minted token and `active = "test"`.
- [ ] **Step 2: Run, confirm fail.** `CARGO_TARGET_DIR=/tmp/denia-verify cargo test --test cli_auth`
- [ ] **Step 3:** Implement `auth.rs` + the `run(args)` entry; declare module.
- [ ] **Step 4: Run, confirm pass.**
- [ ] **Step 5: Commit**

```bash
git add src/cli/client/auth.rs src/cli/client/mod.rs tests/cli_auth.rs
git commit -m "feat(cli): add denia auth (login + API token mint) (ADR-034)"
```

---

## Phase 7 — Working-tree packer

### Task 7.1: `pack.rs`

**Files:** Create `src/cli/client/pack.rs`; modify `src/cli/client/mod.rs` (`pub mod pack;`)

Use `ignore::WalkBuilder`. Defaults give: `.gitignore` honored only inside a git repo (`require_git(true)` default), hidden files included (set `.hidden(false)`), and `.dockerignore` honored via `add_custom_ignore_filename(".dockerignore")`. Always include the Dockerfile even if ignored. Stream selected files into `tar` → `zstd` temp file; enforce count/byte caps.

```rust
pub struct PackLimits { pub max_files: u64, pub max_bytes: u64 }

/// Pack the working tree under `context_root` into a tar.zst at `out`.
/// Honors .gitignore (only in a git repo) and .dockerignore; always includes `dockerfile_rel`.
pub fn pack_context(
    context_root: &Path,
    dockerfile_rel: &str,
    out: &Path,
    limits: &PackLimits,
) -> anyhow::Result<()> {
    let mut walk = ignore::WalkBuilder::new(context_root);
    walk.hidden(false)                       // include dotfiles like .env unless ignored
        .git_ignore(true).git_exclude(true).git_global(true)
        .add_custom_ignore_filename(".dockerignore");
    let mut paths: Vec<PathBuf> = Vec::new();
    for dent in walk.build() {
        let dent = dent?;
        if dent.file_type().is_some_and(|t| t.is_file()) {
            paths.push(dent.path().to_path_buf());
        }
    }
    // Always include the Dockerfile even if an ignore rule excluded it.
    let dockerfile_abs = context_root.join(dockerfile_rel);
    if dockerfile_abs.is_file() && !paths.iter().any(|p| p == &dockerfile_abs) {
        paths.push(dockerfile_abs);
    }
    paths.sort();                            // deterministic ordering
    if paths.len() as u64 > limits.max_files {
        anyhow::bail!("context has {} files (limit {}); tighten .dockerignore", paths.len(), limits.max_files);
    }
    let mut total = 0u64;
    let file = std::fs::File::create(out)?;
    let enc = zstd::stream::write::Encoder::new(file, 0)?.auto_finish();
    let mut tar = tar::Builder::new(enc);
    for p in &paths {
        let rel = p.strip_prefix(context_root).unwrap_or(p);
        total += std::fs::metadata(p)?.len();
        if total > limits.max_bytes {
            anyhow::bail!("context exceeds {} bytes; tighten .dockerignore", limits.max_bytes);
        }
        tar.append_path_with_name(p, rel)?;
    }
    tar.finish()?;
    Ok(())
}
```

- [ ] **Step 1: Write failing tests** in `pack.rs`:
  - `respects_dockerignore` — create context with `a.txt`, `skip.log`, `.dockerignore` (`*.log`), `Dockerfile`; pack; extract via `extract_tar_zst` (reuse the Phase-2 fn — make it `pub`); assert `a.txt` + `Dockerfile` present, `skip.log` absent.
  - `always_includes_dockerfile_even_if_ignored` — `.dockerignore` contains `Dockerfile`; assert it is still packed.
  - `enforces_file_cap` — `max_files = 1` with 2 files → error.

- [ ] **Step 2: Run, confirm fail.** `CARGO_TARGET_DIR=/tmp/denia-verify cargo test -p denia pack::`
- [ ] **Step 3:** Implement; declare module.
- [ ] **Step 4: Run, confirm pass.**
- [ ] **Step 5: Commit**

```bash
git add src/cli/client/pack.rs src/cli/client/mod.rs
git commit -m "feat(cli): working-tree packer honoring .gitignore/.dockerignore (ADR-034)"
```

---

## Phase 8 — `denia push` + wiring

### Task 8.1: `ClientApi` deploy methods

**Files:** Modify `src/cli/client/http.rs` (Test: same file with `httpmock`)

Add:
- `create_project(bearer, name) -> ProjectView` (`POST /v1/projects`).
- `create_service(bearer, body: serde_json::Value) -> ServiceView` — **important:** the services route is `.route("/services", get(list_services).post(put_service))`; `put_service` is an **upsert that deserializes a full `ServiceConfig`** (nil/absent id → create, keyed on `(project_id, name)`). So `--create` must construct a **complete** `ServiceConfig` JSON, not a small create body: project_id, name, source (e.g. an upload/placeholder source consistent with the existing variants — read `src/domain/service.rs` + `src/api/services.rs`), internal port (from `[create].port`), health check (from `[create].health_path` if set), and default limits/domains. This is heavier than the `[create]` block implies — read `src/api/services.rs` `put_service` + `src/domain/service.rs` `ServiceConfig` before writing this method. `ServiceView` deserializes from the returned `ServiceConfig` superset.
- `upload_context(bearer, service_id, bytes: Vec<u8>) -> UploadResponse` (`POST /v1/services/{id}/uploads`, `Content-Type: application/zstd`, body = bytes; struct `{ upload_id: String }`).
- `create_deployment(bearer, body: serde_json::Value) -> DeploymentView` (`POST /v1/deployments`; body `{ source:"upload", service_id, upload_id, dockerfile, context }`; expect `202`).
- `stream_deployment_log(bearer, deployment_id)` — GET `/v1/deployments/{id}/logs` (SSE); read lines, print, stop on a terminal `HEALTHY`/`FAILED` marker (match the markers `run_inner_with_deps` writes: `HEALTHY`, and failures surface via status — also poll `GET /v1/deployments/{id}` for status to decide exit code).

- [ ] **Step 1:** Write `httpmock` tests for `upload_context` (asserts content-type + returns id) and `create_deployment` (asserts `source:"upload"` in body, accepts `202`).
- [ ] **Step 2: Run, confirm fail.**
- [ ] **Step 3:** Implement. For uploads use `.header("content-type","application/zstd").body(bytes)`; accept `200`. For deployments accept `202` as success in the status check.
- [ ] **Step 4: Run, confirm pass.**
- [ ] **Step 5: Commit**

```bash
git add src/cli/client/http.rs
git commit -m "feat(cli): client API methods for upload + deploy (ADR-034)"
```

### Task 8.2: `denia push` command

**Files:** Create `src/cli/client/push.rs`; modify `src/cli/client/mod.rs` (`pub mod push;`)

`PushArgs`: `--create`, `--project`, `--service`, `--dockerfile`, `--context`, `--path` (default `.`), `--profile`, `--no-follow`.

Flow:
1. Load profile: `ClientConfig::load_from(config_path())`; pick `--profile` or `active_profile()`.
2. Read `.denia` from `--path` (reuse `push`'s own helper or the one `console.rs` uses); flags override `project`/`service`/`dockerfile`/`context`.
3. Resolve service: `list_services`; find by project + service name. Missing → if `--create`: ensure project (`list_projects` then `create_project` if absent), `create_service` with `CreateDefaults` (port + optional health), then write the manifest back via `DeniaManifest::write_to`. Else: `anyhow::bail!("service '<p>/<s>' not found; pass --create to create it")`.
4. Resolve `context_root = --path/<context>`; assert `context_root/<dockerfile>` exists, else bail `no Dockerfile at <path> (required)`.
5. `pack_context(context_root, dockerfile, &tmp, &PackLimits{ max_files, max_bytes })` (use generous client caps, e.g. 50k files / 512 MiB).
6. `upload_context(token, service_id, std::fs::read(tmp)?)` → `upload_id` (print byte count).
7. `create_deployment(token, json!({ "source":"upload","service_id":service_id,"upload_id":upload_id,"dockerfile":dockerfile,"context":context }))` → deployment id; print it.
8. Unless `--no-follow`: `stream_deployment_log`; then `GET /v1/deployments/{id}` until status `Healthy` (exit 0) or `Failed` (exit non-zero).

- [ ] **Step 1:** Integration test `tests/cli_push.rs` with `assert_cmd` + `httpmock`: temp dir with `Dockerfile` + `.denia` (`project`/`service`); set `DENIA_CLIENT_CONFIG` to a temp `client.toml` (pre-seeded profile); mock `/v1/services` (list returns the service), `/v1/services/{id}/uploads` (returns `{upload_id}`), `/v1/deployments` (`202` + id), `/v1/deployments/{id}` (status `Healthy`), `/v1/deployments/{id}/logs` (SSE with `HEALTHY`). Run `denia push --no-follow` first (simpler) → exit 0; assert the upload + deployment mocks were hit with `source:"upload"`.
- [ ] **Step 2: Run, confirm fail.** `CARGO_TARGET_DIR=/tmp/denia-verify cargo test --test cli_push`
- [ ] **Step 3:** Implement `push.rs`; declare module.
- [ ] **Step 4: Run, confirm pass.**
- [ ] **Step 5: Commit**

```bash
git add src/cli/client/push.rs src/cli/client/mod.rs tests/cli_push.rs
git commit -m "feat(cli): add denia push (working-tree upload deploy) (ADR-034)"
```

### Task 8.3: Wire subcommands into the CLI

**Files:** Modify `src/cli/mod.rs:34-77`

- [ ] **Step 1:** Add variants to `Commands`:

```rust
    /// Authenticate to a remote Denia and store a profile.
    Auth(client::auth::AuthArgs),
    /// Build + deploy the current working tree to a remote Denia.
    Push(client::push::PushArgs),
```

- [ ] **Step 2:** Add dispatch arms (mirror `Console` — build a tokio runtime):

```rust
        Some(Commands::Auth(args)) => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(crate::cli::client::auth::run(args))
        }
        Some(Commands::Push(args)) => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(crate::cli::client::push::run(args))
        }
```

- [ ] **Step 3: Verify CLI surface.**

Run: `CARGO_TARGET_DIR=/tmp/denia-verify cargo run -- --help 2>&1 | grep -E 'auth|push'`
Expected: both subcommands listed.

- [ ] **Step 4: Commit**

```bash
git add src/cli/mod.rs
git commit -m "feat(cli): register denia auth + denia push subcommands (ADR-034)"
```

---

## Phase 9 — Docs & full verification

### Task 9.1: README

**Files:** Modify `README.md`

- [ ] **Step 1:** Add a Features bullet (client-driven deploy), a CLI subcommands table row each for `denia auth` and `denia push`, an API highlight for `POST /v1/services/{id}/uploads`, and a new "## Deploy from your machine" section documenting `denia auth` → `.denia` → `denia push` with the manifest example and the Dockerfile requirement. Reference ADR-034.
- [ ] **Step 2: Commit**

```bash
git add README.md
git commit -m "docs: document denia auth + denia push client deploy (ADR-034)"
```

### Task 9.2: Full verification

- [ ] **Step 1: Format.** `cargo fmt --all`
- [ ] **Step 2: Lint.** `CARGO_TARGET_DIR=/tmp/denia-verify cargo clippy --all-targets --all-features`. Expected: no warnings; fix any.
- [ ] **Step 3: Full test suite.** `CARGO_TARGET_DIR=/tmp/denia-verify cargo test`. Expected: all pass.
- [ ] **Step 4: Privileged E2E (manual, root).** With a BuildKit daemon available, run the daemon, `denia auth` against it, create a service, and `denia push` a tiny `Dockerfile` project; confirm the deployment reaches `Healthy` and the service responds. If privileged/buildkit infra is unavailable in this environment, document that the E2E was not run rather than claiming it passed.

```bash
DENIA_RUN_PRIVILEGED_TESTS=1 cargo test --test linux_runtime_privileged -- --ignored
```

- [ ] **Step 5: Commit any fmt/clippy fixes.**

```bash
git add -A
git commit -m "chore: rustfmt + clippy for client-driven deploy (ADR-034)"
```

---

## Open items for the implementer to confirm during execution

- **`src/config.rs` constructor/field names** — Task 0.2 assumes a `FileConfig` → `AppConfig` resolution with per-field env overrides + a default-template writer. Match the real shapes.
- **`src/api/mod.rs` module declaration style** — confirm whether modules are declared there or inline in `app.rs`; declare `uploads` the same way `console` is.
- **`POST /v1/services` is an upsert (`put_service`) taking a full `ServiceConfig`** — not a lightweight create. `--create` (Task 8.2) must build a complete `ServiceConfig` from `[create]` defaults + a source. Read `src/api/services.rs` `put_service` and `src/domain/service.rs` `ServiceConfig` first. If constructing a full `ServiceConfig` from the CLI proves too broad for v1, fall back to requiring a pre-existing service (drop `--create`) and surface that to the user — do not ship a half-built create.
- **SSE terminal markers** — confirm the exact log markers / status transitions in `run_inner_with_deps` so `stream_deployment_log` stops correctly; the status poll on `GET /v1/deployments/{id}` is the authoritative exit-code signal.
- **Symlinks unsupported in v1 uploads** — documented limitation (extractor rejects them). Note in the README if relevant.
