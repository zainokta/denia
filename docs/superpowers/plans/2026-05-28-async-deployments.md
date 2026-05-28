# Async Deployments Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `POST /v1/deployments` return `202 Accepted` immediately while a background tokio task runs the deploy pipeline, writes per-phase log lines to a per-deployment file, and updates `DeploymentStatus` through `Pending → Building → Starting → Healthy|Failed`. Add `GET /v1/deployments/{id}` and `GET /v1/deployments/{id}/logs` (SSE tail).

**Architecture:** Coordinator splits into (a) `create_pending(service, request)` — synchronous DB row creation returned to client; (b) `run(deployment_id, …)` — async task that emits phase log lines via a new `DeploymentLogWriter`, transitions status, and finalizes routing. Boot adds `fail_orphan_deployments()` mirroring `fail_orphan_runs`. Log files live at `<log_dir>/deployments/<id>.log` (`0600`), tailed via the existing `LogTailer` from `src/observability/logs.rs`. SSE handler reuses the service-log streaming pattern with a terminal-status check that closes the stream once status is `Healthy|Failed|Stopped` AND the tailer is caught up.

**Tech Stack:** Rust 2024, axum 0.8 (`response::sse`), tokio (spawn/interval/mpsc), `tracing`, existing `LogTailer`. No new crates.

**Spec:** `docs/adr/024-async-deployments.md`

**Worktree:** Skipped — implementation runs directly on `master` per operator request.

---

## File map

- **Create**
  - `src/deploy/log.rs` — `DeploymentLogWriter` (file append) + `DeploymentLogPath` helper.
  - `tests/async_deploy.rs` — end-to-end test covering POST → status transitions → SSE log.
- **Modify**
  - `src/repo/sqlite/deployments.rs` — `fail_orphan_deployments_q` + store method.
  - `src/deploy/mod.rs` — export `log` submodule.
  - `src/deploy/coordinator.rs` — split into `create_pending` + `run`; thread log writer through phases; rename existing `deploy` body into `finalize`.
  - `src/deploy/error.rs` — add `Phase` annotations if needed (only if tests require it; otherwise skip).
  - `src/api/deployments.rs` — 202 + spawn; add `GET /{id}` and `GET /{id}/logs`.
  - `src/api/error.rs` — already logs 500s; no change unless a new variant added.
  - `src/main.rs` — call `store.fail_orphan_deployments()` next to `fail_orphan_runs`; write synthetic "control plane restarted; deployment aborted" line to each orphaned log file.
  - `web/src/effect/schema.ts` and the deployment detail page under `web/src/` — subscribe to the SSE log endpoint. Frontend touch is in scope because the operator asked for it on the deployment page.

---

### Task 1: Boot orphan recovery for deployments

**Files:**
- Modify: `src/repo/sqlite/deployments.rs`
- Test: same file (`mod tests`)

- [ ] **Step 1: Write the failing test**

Append to the `#[cfg(test)] mod tests` block in `src/repo/sqlite/deployments.rs`:

```rust
#[test]
fn fail_orphan_deployments_marks_in_flight_failed() {
    let store = SqliteStore::open_in_memory().unwrap();
    store.migrate().unwrap();

    let svc = Uuid::now_v7();
    let d_pending = store
        .create_deployment(DeploymentRequest::external_image(svc, "img"))
        .unwrap();
    let d_starting = store
        .create_deployment(DeploymentRequest::external_image(svc, "img"))
        .unwrap();
    store
        .update_deployment_status(d_starting.id, DeploymentStatus::Starting)
        .unwrap();
    let d_healthy = store
        .create_deployment(DeploymentRequest::external_image(svc, "img"))
        .unwrap();
    store
        .update_deployment_status(d_healthy.id, DeploymentStatus::Healthy)
        .unwrap();

    let n = store.fail_orphan_deployments().unwrap();
    assert_eq!(n, 2, "two in-flight rows must be marked failed");

    let all = store.list_deployments(svc).unwrap();
    let by_id = |id: Uuid| all.iter().find(|d| d.id == id).unwrap().status.clone();
    assert_eq!(by_id(d_pending.id), DeploymentStatus::Failed);
    assert_eq!(by_id(d_starting.id), DeploymentStatus::Failed);
    assert_eq!(by_id(d_healthy.id), DeploymentStatus::Healthy);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p denia --lib fail_orphan_deployments_marks_in_flight_failed`
Expected: FAIL with `no method named fail_orphan_deployments`.

- [ ] **Step 3: Implement the query**

Add to `src/repo/sqlite/deployments.rs` after `update_deployment_status_q`:

```rust
pub(super) fn fail_orphan_deployments_q(conn: &Connection) -> Result<Vec<Uuid>, RepoError> {
    let pending = serde_json::to_string(&DeploymentStatus::Pending)?;
    let building = serde_json::to_string(&DeploymentStatus::Building)?;
    let starting = serde_json::to_string(&DeploymentStatus::Starting)?;
    let failed = serde_json::to_string(&DeploymentStatus::Failed)?;

    let mut stmt = conn.prepare(
        "SELECT id FROM deployments WHERE status IN (?1, ?2, ?3)",
    )?;
    let ids: Vec<Uuid> = stmt
        .query_map(params![&pending, &building, &starting], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|s| Uuid::parse_str(&s))
        .collect::<Result<Vec<_>, _>>()?;

    conn.execute(
        "UPDATE deployments SET status = ?1 WHERE status IN (?2, ?3, ?4)",
        params![&failed, &pending, &building, &starting],
    )?;
    Ok(ids)
}
```

- [ ] **Step 4: Expose on `SqliteStore`**

In the same file, inside `impl SqliteStore`:

```rust
pub fn fail_orphan_deployments(&self) -> Result<Vec<Uuid>, StateError> {
    let connection = self.connection()?;
    fail_orphan_deployments_q(&connection).map_err(StateError::from)
}
```

- [ ] **Step 5: Adjust the test assertion if needed and re-run**

Run: `cargo test -p denia --lib fail_orphan_deployments_marks_in_flight_failed`
Expected: PASS. If failing on the returned Vec shape, update either the assertion or the query to match.

- [ ] **Step 6: Commit**

```bash
git add src/repo/sqlite/deployments.rs
git commit -m "feat(deploy): fail_orphan_deployments for boot recovery"
```

---

### Task 2: `DeploymentLogWriter`

A synchronous append-only writer used by the async deploy task. Each line is timestamped + phase-tagged. File mode `0600` on first create. Path is `<log_dir>/deployments/<deployment_id>.log`.

**Files:**
- Create: `src/deploy/log.rs`
- Modify: `src/deploy/mod.rs` (`pub mod log;`)
- Test: `src/deploy/log.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Write the failing tests**

Create `src/deploy/log.rs`:

```rust
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::Mutex,
};

use chrono::Utc;
use uuid::Uuid;

/// Resolve the per-deployment log path used by the writer, SSE handler, and
/// orphan-recovery synthetic line.
pub fn deployment_log_path(log_dir: &Path, deployment_id: Uuid) -> PathBuf {
    log_dir.join("deployments").join(format!("{deployment_id}.log"))
}

#[derive(Debug)]
pub struct DeploymentLogWriter {
    path: PathBuf,
    handle: Mutex<std::fs::File>,
}

impl DeploymentLogWriter {
    pub fn create(log_dir: &Path, deployment_id: Uuid) -> std::io::Result<Self> {
        let path = deployment_log_path(log_dir, deployment_id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let handle = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&path)?;
        Ok(Self { path, handle: Mutex::new(handle) })
    }

    pub fn path(&self) -> &Path { &self.path }

    pub fn write(&self, phase: &str, message: &str) -> std::io::Result<()> {
        let ts = Utc::now().to_rfc3339();
        let line = format!("{ts} {phase} {message}\n");
        let mut g = self.handle.lock().expect("log writer mutex poisoned");
        g.write_all(line.as_bytes())?;
        g.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn writes_one_line_per_call() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::now_v7();
        let w = DeploymentLogWriter::create(dir.path(), id).unwrap();
        w.write("OCI_PULL", "starting").unwrap();
        w.write("OCI_PULL", "done").unwrap();
        let body = std::fs::read_to_string(w.path()).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("OCI_PULL starting"));
        assert!(lines[1].contains("OCI_PULL done"));
    }

    #[test]
    fn path_is_under_log_dir_deployments_subdir() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::now_v7();
        let p = deployment_log_path(dir.path(), id);
        assert_eq!(p.parent().unwrap(), dir.path().join("deployments"));
        assert_eq!(p.extension().unwrap(), "log");
    }

    #[cfg(unix)]
    #[test]
    fn file_mode_is_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::now_v7();
        let w = DeploymentLogWriter::create(dir.path(), id).unwrap();
        w.write("BOOT", "ok").unwrap();
        let mode = std::fs::metadata(w.path()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
```

- [ ] **Step 2: Add module export**

In `src/deploy/mod.rs`, add a public submodule next to the others:

```rust
pub mod log;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p denia --lib deploy::log`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/deploy/log.rs src/deploy/mod.rs
git commit -m "feat(deploy): per-deployment log writer (0600 append file)"
```

---

### Task 3: Coordinator split — `create_pending` + `run`

Rip `create_deployment` out of `DeploymentCoordinator::deploy()` so the API can persist the row up front and return it. Convert the existing body into a phase-emitting `run(deployment_id, …)` that takes a `&DeploymentLogWriter`. Existing `deploy_external_image_source` / `deploy_git_source` become thin entry points used by tests; they create the pending row then call `run`.

**Files:**
- Modify: `src/deploy/coordinator.rs`
- Test: `tests/deploy_orchestration.rs` (existing) + new unit tests inline.

- [ ] **Step 1: Write the failing tests**

Append to the bottom of `src/deploy/coordinator.rs` (inside `#[cfg(test)] mod tests` — add one if missing):

```rust
#[cfg(test)]
mod async_tests {
    use super::*;
    use crate::deploy::log::DeploymentLogWriter;
    // existing test helpers in tests/deploy_orchestration.rs build a fake
    // runtime + health. Re-use them via a small local helper if needed.

    #[tokio::test]
    async fn create_pending_persists_row_in_pending_status() {
        // build a coordinator with in-memory store + fakes (mirror
        // coordinator_promotes_only_after_health_check_passes setup).
        let (_dir, coord, svc, request) = test_helpers::coord_for_pending();
        let d = coord.create_pending(&svc, request.clone()).await.unwrap();
        assert_eq!(d.status, DeploymentStatus::Pending);
        let row = coord
            .repos
            .deployments
            .list_deployments(svc.id)
            .unwrap()
            .into_iter()
            .find(|d2| d2.id == d.id)
            .unwrap();
        assert_eq!(row.status, DeploymentStatus::Pending);
    }

    #[tokio::test]
    async fn run_transitions_pending_building_starting_healthy() {
        let (dir, coord, svc, request) = test_helpers::coord_for_pending();
        let d = coord.create_pending(&svc, request.clone()).await.unwrap();
        let log = DeploymentLogWriter::create(dir.path(), d.id).unwrap();
        coord.run(d.id, svc.clone(), request, &log).await.unwrap();
        let final_row = coord
            .repos
            .deployments
            .list_deployments(svc.id)
            .unwrap()
            .into_iter()
            .find(|d2| d2.id == d.id)
            .unwrap();
        assert_eq!(final_row.status, DeploymentStatus::Healthy);
        let body = std::fs::read_to_string(log.path()).unwrap();
        assert!(body.contains("OCI_PULL"));
        assert!(body.contains("RUNTIME_START"));
        assert!(body.contains("HEALTHCHECK"));
    }
}
```

> The exact `test_helpers::coord_for_pending` helper should reuse what
> `tests/deploy_orchestration.rs` already does. If duplicating is cheaper than
> exposing, inline a minimal builder in the new test module that returns
> `(tempdir, coordinator, ServiceConfig, DeploymentRequest)`.

- [ ] **Step 2: Run tests to verify failure**

Run: `cargo test -p denia --lib coordinator::async_tests`
Expected: FAIL with `no method create_pending` / `no method run`.

- [ ] **Step 3: Implement `create_pending`**

In `src/deploy/coordinator.rs`, inside the `impl<R, H> DeploymentCoordinator<R, H>` block, ABOVE the existing `deploy` method:

```rust
pub async fn create_pending(
    &self,
    service: &ServiceConfig,
    request: DeploymentRequest,
) -> Result<Deployment, DeployError> {
    let _ = service; // reserved for future per-service validation
    let deployment = self.repos.deployments.create_deployment(request)?;
    Ok(deployment)
}
```

- [ ] **Step 4: Implement `run`**

Add the orchestration entrypoint that previously lived split across
`deploy_external_image_source` / `deploy_git_source` / `deploy`:

```rust
pub async fn run(
    &self,
    deployment_id: Uuid,
    service: ServiceConfig,
    request: DeploymentRequest,
    log: &crate::deploy::log::DeploymentLogWriter,
) -> Result<(), DeployError> {
    let res = self
        .run_inner(deployment_id, service, request, log)
        .await;
    if let Err(ref e) = res {
        let _ = log.write("ERROR", &format!("{e:?}"));
        let _ = self
            .repos
            .deployments
            .update_deployment_status(deployment_id, DeploymentStatus::Failed);
    }
    res
}

async fn run_inner(
    &self,
    deployment_id: Uuid,
    service: ServiceConfig,
    request: DeploymentRequest,
    log: &crate::deploy::log::DeploymentLogWriter,
) -> Result<(), DeployError> {
    log.write("START", &format!("deployment_id={deployment_id}")).ok();

    // BUILDING — acquire artifact. Today this branches on `request`; keep that
    // shape but consume the existing helpers. `acquirer` / `runner` /
    // `secret_store` / `sops_binary` are injected from the API handler.
    self.repos
        .deployments
        .update_deployment_status(deployment_id, DeploymentStatus::Building)?;
    log.write("BUILDING", "acquiring artifact").ok();

    // The previous code lives in `deploy_external_image_source` /
    // `deploy_git_source`. Inline only the artifact-acquire portion here and
    // call `finalize` for the runtime + routing portion. Signature change:
    // existing entry points must pass `deployment_id` down rather than
    // creating a new row.
    let artifact = self
        .acquire_artifact_for_run(&service, &request, log)
        .await?;
    self.repos.deployments.put_artifact(artifact.clone())?;
    self.repos
        .deployments
        .set_deployment_artifact(deployment_id, &artifact.digest)?;

    // STARTING + HEALTHCHECK + promotion + routing — mirrors the body of the
    // current `deploy()`, but uses the supplied deployment_id and emits log
    // lines.
    self.repos
        .deployments
        .update_deployment_status(deployment_id, DeploymentStatus::Starting)?;
    log.write("STARTING", "launching runtime").ok();
    self.finalize(deployment_id, &service, artifact, log).await?;
    self.repos
        .deployments
        .update_deployment_status(deployment_id, DeploymentStatus::Healthy)?;
    log.write("HEALTHY", "deployment promoted").ok();
    Ok(())
}

async fn acquire_artifact_for_run(
    &self,
    service: &ServiceConfig,
    request: &DeploymentRequest,
    log: &crate::deploy::log::DeploymentLogWriter,
) -> Result<ArtifactRecord, DeployError> {
    // The control plane wires the acquirer + runner + secret store via
    // injection (next task wires this). For now, accept them as `self` fields
    // OR via a `&dyn` injected slice. Simplest: extend the constructor.
    //
    // Implementation note: copy the existing `deploy_external_image_source`
    // / `deploy_git_source` body that resolves auth + calls
    // `acquirer.acquire_rootfs_bundle_from_image_config(...)`, then return
    // the resulting `ArtifactRecord`. Emit `log.write("OCI_PULL", ...)`
    // before/after.
    //
    // To avoid widening the coordinator's surface, the API handler will
    // own these dependencies and pass them via a helper struct. See Task 4.
    let _ = (service, request, log);
    unimplemented!("filled in alongside the API handler refactor in Task 4")
}

async fn finalize(
    &self,
    deployment_id: Uuid,
    service: &ServiceConfig,
    artifact: ArtifactRecord,
    log: &crate::deploy::log::DeploymentLogWriter,
) -> Result<(), DeployError> {
    let project = self
        .repos
        .projects
        .get_project(service.project_id)?
        .ok_or(DeployError::Repo(RepoError::UnknownProject))?;
    let limits = service.effective_limits(&project);
    let env: Vec<(String, String)> = service.effective_env(&project).into_iter().collect();

    log.write("RUNTIME_START", &format!("port={}", service.internal_port)).ok();
    let runtime_status = self
        .runtime
        .start(RuntimeStartRequest {
            service_name: service.name.clone(),
            service_id: service.id,
            deployment_id,
            artifact,
            internal_port: service.internal_port,
            socket_path: format!("/var/lib/denia/runtime/{}/current.sock", service.id).into(),
            cpu_millis: limits.cpu_millis,
            memory_bytes: limits.memory_bytes,
            env,
            pids_max: None,
            memory_swap_max: None,
            io_weight: None,
            replica_index: 0,
        })
        .await?;

    log.write("HEALTHCHECK", "starting").ok();
    self.health
        .check(
            &format!("http://127.0.0.1:{}", service.internal_port),
            &service.health_check,
        )
        .await?;
    log.write("HEALTHCHECK", "passed").ok();

    self.repos
        .deployments
        .promote_deployment(service.id, deployment_id)?;
    self.write_routing_config(service, &runtime_status.socket_path)
        .await?;
    Ok(())
}
```

> **Important:** `acquire_artifact_for_run` is intentionally `unimplemented!`
> at this step. Task 4 wires the acquirer + runner + secret store through and
> fills it in. This split keeps Task 3 reviewable.

- [ ] **Step 5: Keep the existing entry points working**

Leave `deploy_external_image_source` and `deploy_git_source` in place for now
(integration tests reference them). They will be deleted in Task 4 once the
API handler stops calling them.

- [ ] **Step 6: Run tests**

Run: `cargo test -p denia --lib coordinator`
Expected: `create_pending_persists_row_in_pending_status` passes.
The `run_…` test should still fail because `acquire_artifact_for_run` is
`unimplemented!` — mark that test `#[ignore]` until Task 4. Add a comment in
the source linking the ignore reason to Task 4.

- [ ] **Step 7: Commit**

```bash
git add src/deploy/coordinator.rs
git commit -m "refactor(deploy): split coordinator into create_pending + run skeleton"
```

---

### Task 4: API handler — 202 + spawn + dependency wiring

Move the artifact-acquire + auth resolution into the spawned task (where it
belongs) so the coordinator no longer needs the acquirer/runner/secret
store/sops binary at construction time. Handler creates the pending row,
returns `202`, and spawns the deploy task.

**Files:**
- Modify: `src/api/deployments.rs`
- Modify: `src/deploy/coordinator.rs` (fill in `acquire_artifact_for_run`; delete `deploy_external_image_source` / `deploy_git_source`).
- Test: `tests/async_deploy.rs` (new)

- [ ] **Step 1: Write the failing integration test**

Create `tests/async_deploy.rs`:

```rust
use axum::body::Body;
use axum::http::{Request, StatusCode};
use denia::{
    app::{AppState, build_router},
    config::AppConfig,
    domain::{DeploymentRequest, DeploymentStatus, ServiceConfig, ServiceSource, ResourceLimits, HealthCheck, ExternalImageSource},
};
use serde_json::Value;
use tower::ServiceExt;
use uuid::Uuid;

const ADMIN_TOKEN: &str = "test-admin-token-0123456789abcdef-0123456789abcdef";

#[tokio::test]
async fn post_deployments_returns_202_and_row_is_pending() {
    // Build AppState backed by an in-memory store with fakes that always
    // succeed. Reuse the existing test_state() helper pattern from
    // tests/backend_contract.rs or build a minimal AppState here.
    let state = test_support::accepting_app_state();
    let project_id = test_support::seed_project(&state);
    let service = test_support::seed_service(&state, project_id);

    let request = DeploymentRequest::external_image(service.id, "alpine:3");
    let body = serde_json::to_vec(&request).unwrap();

    let app = build_router(state.clone());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/deployments")
                .header("authorization", format!("Bearer {ADMIN_TOKEN}"))
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    let id: Uuid = v["id"].as_str().unwrap().parse().unwrap();
    let status = state
        .deployments
        .list_deployments(service.id)
        .unwrap()
        .into_iter()
        .find(|d| d.id == id)
        .unwrap()
        .status;
    assert!(matches!(status, DeploymentStatus::Pending | DeploymentStatus::Building | DeploymentStatus::Starting | DeploymentStatus::Healthy));
}

mod test_support {
    // Inline minimal helpers; if duplication grows, extract to a shared
    // module under `tests/common/`.
}
```

> If the existing test layout uses an `axum_test`/`tower` helper, mirror it.
> The point is to drive the handler and check the response is `202` plus the
> row exists.

- [ ] **Step 2: Run the test to verify failure**

Run: `cargo test --test async_deploy post_deployments_returns_202_and_row_is_pending`
Expected: FAIL (handler currently returns `200` and blocks).

- [ ] **Step 3: Rewrite the handler**

Replace `create_deployment` in `src/api/deployments.rs` with:

```rust
async fn create_deployment(
    State(state): State<AppState>,
    principal: Principal,
    Json(request): Json<DeploymentRequest>,
) -> Result<(StatusCode, Json<crate::domain::Deployment>), ApiError> {
    let Some(service) = state.services.get_service(request.service_id())? else {
        return Err(ApiError::NotFound("service not found".to_string()));
    };
    ensure_role(&state, &principal, service.project_id, Role::Operator)?;

    let coordinator = DeploymentCoordinator::new_with_shared_routing(
        state.deployment_repos(),
        state.runtime.clone(),
        state.health.clone(),
        state.ingress.clone(),
        state.routes.clone(),
    );
    let deployment = coordinator.create_pending(&service, request.clone()).await?;

    let log = crate::deploy::log::DeploymentLogWriter::create(
        &state.config.log_dir,
        deployment.id,
    )
    .map_err(ApiError::Log)?;

    // Capture everything the spawned task needs. The task cannot borrow from
    // the request, so clone owned values.
    let svc = service.clone();
    let req = request.clone();
    let deployment_id = deployment.id;
    let acquirer = match state.oci_cache.clone() {
        Some(cache) => ArtifactAcquirer::new_with_cache(state.config.clone(), cache),
        None => ArtifactAcquirer::new(state.config.clone()),
    };
    let secret_store = crate::secrets::SopsSecretStore::new(state.config.data_dir.clone());
    let sops_binary = state.config.sops_binary.clone();
    let runner = state.command_runner.clone();
    let coordinator_for_task = DeploymentCoordinator::new_with_shared_routing(
        state.deployment_repos(),
        state.runtime.clone(),
        state.health.clone(),
        state.ingress.clone(),
        state.routes.clone(),
    );

    tokio::spawn(async move {
        let deps = crate::deploy::coordinator::RunDeps {
            acquirer: &acquirer,
            runner: runner.as_ref(),
            secret_store: &secret_store,
            sops_binary: &sops_binary,
        };
        let _ = coordinator_for_task
            .run_with_deps(deployment_id, svc, req, &log, deps)
            .await;
    });

    Ok((StatusCode::ACCEPTED, Json(deployment)))
}
```

- [ ] **Step 4: Add `RunDeps` + `run_with_deps` to the coordinator**

In `src/deploy/coordinator.rs`, replace the stub `acquire_artifact_for_run` with a real implementation that receives a `RunDeps`. Add:

```rust
pub struct RunDeps<'a> {
    pub acquirer: &'a ArtifactAcquirer,
    pub runner: &'a dyn CommandRunner,
    pub secret_store: &'a crate::secrets::SopsSecretStore,
    pub sops_binary: &'a std::path::Path,
}

impl<R, H> DeploymentCoordinator<R, H>
where
    R: Runtime,
    H: HealthChecker,
{
    pub async fn run_with_deps(
        &self,
        deployment_id: Uuid,
        service: ServiceConfig,
        request: DeploymentRequest,
        log: &crate::deploy::log::DeploymentLogWriter,
        deps: RunDeps<'_>,
    ) -> Result<(), DeployError> {
        // Same wrapper as the earlier `run`, but resolves the artifact using
        // the supplied deps. On error: write ERROR line + mark Failed.
        let res = self
            .run_inner_with_deps(deployment_id, service, request, log, deps)
            .await;
        if let Err(ref e) = res {
            let _ = log.write("ERROR", &format!("{e:?}"));
            let _ = self
                .repos
                .deployments
                .update_deployment_status(deployment_id, DeploymentStatus::Failed);
        }
        res
    }

    async fn run_inner_with_deps(
        &self,
        deployment_id: Uuid,
        service: ServiceConfig,
        request: DeploymentRequest,
        log: &crate::deploy::log::DeploymentLogWriter,
        deps: RunDeps<'_>,
    ) -> Result<(), DeployError> {
        log.write("START", &format!("deployment_id={deployment_id}")).ok();
        self.repos
            .deployments
            .update_deployment_status(deployment_id, DeploymentStatus::Building)?;
        log.write("BUILDING", "resolving auth + acquiring artifact").ok();

        let artifact = match &request {
            DeploymentRequest::ExternalImage { .. } => {
                let ServiceSource::ExternalImage(source) = &service.source else {
                    return Err(DeployError::UnsupportedServiceSource);
                };
                let (full_ref, auth) = resolve_external_auth(
                    &self.repos,
                    source,
                    service.project_id,
                    deps.secret_store,
                    deps.runner,
                    deps.sops_binary,
                )
                .await?;
                deps.acquirer
                    .acquire_rootfs_bundle_from_image_config(
                        deps.runner,
                        ArtifactAcquireRequest::ExternalImage { image: full_ref },
                        auth,
                    )
                    .await?
            }
            DeploymentRequest::Git { .. } => deps
                .acquirer
                .acquire_rootfs_bundle_from_image_config(
                    deps.runner,
                    ArtifactAcquireRequest::Git {
                        // mirror existing git acquire path
                    },
                    crate::oci::RegistryAuth::Anonymous,
                )
                .await?,
        };
        log.write("OCI_PULL", "done").ok();
        self.repos.deployments.put_artifact(artifact.clone())?;
        self.repos
            .deployments
            .set_deployment_artifact(deployment_id, &artifact.digest)?;

        self.repos
            .deployments
            .update_deployment_status(deployment_id, DeploymentStatus::Starting)?;
        log.write("STARTING", "launching runtime").ok();
        self.finalize(deployment_id, &service, artifact, log).await?;
        self.repos
            .deployments
            .update_deployment_status(deployment_id, DeploymentStatus::Healthy)?;
        log.write("HEALTHY", "deployment promoted").ok();
        Ok(())
    }
}

async fn resolve_external_auth(
    repos: &DeploymentRepos,
    source: &crate::domain::ExternalImageSource,
    project_id: Uuid,
    secret_store: &crate::secrets::SopsSecretStore,
    runner: &dyn CommandRunner,
    sops_binary: &std::path::Path,
) -> Result<(String, crate::oci::RegistryAuth), DeployError> {
    // copy/move the body currently in
    // `deploy_external_image_source` that builds (full_ref, auth).
    unimplemented!()
}
```

> Move the auth-resolution code from `deploy_external_image_source` into
> `resolve_external_auth`. Once that helper exists, **delete**
> `deploy_external_image_source` and `deploy_git_source` and the original
> `deploy()` (no callers will remain after Task 4 + Task 5 land).

- [ ] **Step 5: Remove the `unimplemented!` and old test for `run`**

Un-ignore the `run_transitions_pending_building_starting_healthy` test from
Task 3, switching it to construct `RunDeps` from fake helpers. If the
fakes are not yet runnable inline, leave it `#[ignore]` and rely on the
integration test in `tests/async_deploy.rs`.

- [ ] **Step 6: Run tests**

Run: `cargo build && cargo test --test async_deploy`
Expected: `post_deployments_returns_202_and_row_is_pending` passes. Watch
the build for unused imports — clean them.

- [ ] **Step 7: Commit**

```bash
git add src/api/deployments.rs src/deploy/coordinator.rs tests/async_deploy.rs
git commit -m "feat(deploy): async deploy via 202 + spawn (ADR-024)"
```

---

### Task 5: `GET /v1/deployments/{id}` endpoint

**Files:**
- Modify: `src/api/deployments.rs`
- Modify: `src/repo/sqlite/deployments.rs` (add `get_deployment` by id)
- Test: `tests/async_deploy.rs`

- [ ] **Step 1: Write the failing test**

Append to `tests/async_deploy.rs`:

```rust
#[tokio::test]
async fn get_deployment_returns_row() {
    let state = test_support::accepting_app_state();
    let project_id = test_support::seed_project(&state);
    let service = test_support::seed_service(&state, project_id);
    let deployment = state
        .deployments
        .create_deployment(DeploymentRequest::external_image(service.id, "alpine:3"))
        .unwrap();

    let app = build_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/v1/deployments/{}", deployment.id))
                .header("authorization", format!("Bearer {ADMIN_TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
```

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test --test async_deploy get_deployment_returns_row`
Expected: FAIL (404 — route does not exist yet).

- [ ] **Step 3: Implement repo + handler**

In `src/repo/sqlite/deployments.rs`, add:

```rust
pub(super) fn get_deployment_q(conn: &Connection, id: Uuid) -> Result<Option<Deployment>, RepoError> {
    let row = conn
        .query_row(
            "SELECT id, service_id, request_json, status, created_at FROM deployments WHERE id = ?1",
            params![id.to_string()],
            |r| {
                Ok(DeploymentRow {
                    id: r.get(0)?,
                    service_id: r.get(1)?,
                    request_json: r.get(2)?,
                    status_json: r.get(3)?,
                    created_at: r.get(4)?,
                })
            },
        )
        .optional()?;
    row.map(|row| {
        Ok(Deployment {
            id: Uuid::parse_str(&row.id)?,
            service_id: Uuid::parse_str(&row.service_id)?,
            request: serde_json::from_str(&row.request_json)?,
            status: serde_json::from_str(&row.status_json)?,
            created_at: row.created_at.parse()?,
        })
    })
    .transpose()
}
```

Expose on both `SqliteStore` and `SqliteDeploymentRepo` (`get_deployment`).

In `src/api/deployments.rs`, add the route + handler:

```rust
.route("/deployments/{deployment_id}", get(get_deployment))
```

```rust
async fn get_deployment(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(deployment_id): axum::extract::Path<Uuid>,
) -> Result<Json<crate::domain::Deployment>, ApiError> {
    let Some(deployment) = state.deployments.get_deployment(deployment_id)? else {
        return Err(ApiError::NotFound("deployment not found".to_string()));
    };
    let Some(service) = state.services.get_service(deployment.service_id)? else {
        return Err(ApiError::NotFound("service not found".to_string()));
    };
    ensure_role(&state, &principal, service.project_id, Role::Viewer)?;
    Ok(Json(deployment))
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test --test async_deploy get_deployment_returns_row`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/api/deployments.rs src/repo/sqlite/deployments.rs
git commit -m "feat(api): GET /v1/deployments/{id}"
```

---

### Task 6: SSE log endpoint `GET /v1/deployments/{id}/logs`

Reuse `LogTailer` + the service-log SSE pattern. Stream closes when (status is terminal) AND (tailer caught up to EOF).

**Files:**
- Modify: `src/api/deployments.rs`
- Test: `tests/async_deploy.rs`

- [ ] **Step 1: Write the failing test**

Append to `tests/async_deploy.rs`:

```rust
#[tokio::test]
async fn deployment_log_stream_returns_text_event_stream() {
    let state = test_support::accepting_app_state();
    let project_id = test_support::seed_project(&state);
    let service = test_support::seed_service(&state, project_id);
    let deployment = state
        .deployments
        .create_deployment(DeploymentRequest::external_image(service.id, "alpine:3"))
        .unwrap();

    // Pre-create a log file so the tailer has something to backlog.
    let log = denia::deploy::log::DeploymentLogWriter::create(
        &state.config.log_dir,
        deployment.id,
    )
    .unwrap();
    log.write("START", "hello").unwrap();

    let app = build_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/v1/deployments/{}/logs", deployment.id))
                .header("authorization", format!("Bearer {ADMIN_TOKEN}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let ct = response.headers().get("content-type").unwrap().to_str().unwrap();
    assert!(ct.starts_with("text/event-stream"));
}
```

- [ ] **Step 2: Run test to verify failure**

Run: `cargo test --test async_deploy deployment_log_stream_returns_text_event_stream`
Expected: FAIL.

- [ ] **Step 3: Implement the handler**

In `src/api/deployments.rs`:

```rust
.route("/deployments/{deployment_id}/logs", get(deployment_log_stream))
```

```rust
async fn deployment_log_stream(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(deployment_id): axum::extract::Path<Uuid>,
) -> Result<impl axum::response::IntoResponse, ApiError> {
    use axum::response::sse::{Event, KeepAlive, Sse};
    use std::convert::Infallible;
    use std::time::Duration;
    use tokio_stream::wrappers::ReceiverStream;

    let Some(deployment) = state.deployments.get_deployment(deployment_id)? else {
        return Err(ApiError::NotFound("deployment not found".to_string()));
    };
    let Some(service) = state.services.get_service(deployment.service_id)? else {
        return Err(ApiError::NotFound("service not found".to_string()));
    };
    ensure_role(&state, &principal, service.project_id, Role::Operator)?;

    let log_path = crate::deploy::log::deployment_log_path(&state.config.log_dir, deployment_id);
    let store = state.deployments.clone();
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(256);
    tokio::spawn(async move {
        let mut tailer = crate::observability::logs::LogTailer::new(&log_path);
        if let Ok(lines) = tokio::task::block_in_place(|| tailer.backlog(2000)) {
            for line in lines {
                if tx.send(Ok(Event::default().data(line))).await.is_err() {
                    return;
                }
            }
        }
        let mut interval = tokio::time::interval(Duration::from_millis(300));
        loop {
            interval.tick().await;
            let lines = tokio::task::block_in_place(|| tailer.poll()).unwrap_or_default();
            for line in lines {
                if tx.send(Ok(Event::default().data(line))).await.is_err() {
                    return;
                }
            }
            // close once status is terminal AND no more lines this tick.
            if let Ok(Some(d)) = store.get_deployment(deployment_id) {
                let terminal = matches!(
                    d.status,
                    crate::domain::DeploymentStatus::Healthy
                        | crate::domain::DeploymentStatus::Failed
                        | crate::domain::DeploymentStatus::Stopped
                );
                if terminal {
                    let _ = tx.send(Ok(Event::default().event("done").data(""))).await;
                    return;
                }
            }
        }
    });

    Ok(Sse::new(ReceiverStream::new(rx))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test --test async_deploy deployment_log_stream_returns_text_event_stream`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/api/deployments.rs
git commit -m "feat(api): SSE deployment log tail at /v1/deployments/{id}/logs"
```

---

### Task 7: Boot recovery + synthetic restart line

**Files:**
- Modify: `src/main.rs`

- [ ] **Step 1: Write the integration check**

This is a manual verification step — boot recovery is well-tested at the repo level in Task 1. Just wire it.

- [ ] **Step 2: Implement**

In `src/main.rs`, next to the existing `fail_orphan_runs` block:

```rust
let orphan_deployments = store.fail_orphan_deployments()?;
for id in &orphan_deployments {
    let path = denia::deploy::log::deployment_log_path(&config.log_dir, *id);
    if let Ok(writer) = denia::deploy::log::DeploymentLogWriter::create(&config.log_dir, *id) {
        let _ = writer.write("RESTART", "control plane restarted; deployment aborted");
    }
    tracing::warn!(deployment_id = %id, path = %path.display(), "orphan deployment marked Failed");
}
```

- [ ] **Step 3: Build**

Run: `cargo build`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add src/main.rs
git commit -m "feat(deploy): mark orphan deployments Failed on boot + synthetic log line"
```

---

### Task 8: Frontend — deployment detail page + log viewer

Operator asked for it on the deployment page. Frontend lives under `web/`.

**Files:**
- Modify: `web/src/effect/schema.ts` (DeploymentStatus already exists — confirm). Add a route binding for the deployment detail page.
- Create or modify: the deployment detail page component (search `web/src/` for the existing deployment list page; place the detail next to it).

- [ ] **Step 1: Locate the existing deployment list page**

Run: `rg --files-with-matches "Deployments" web/src/` (or via your editor).
Identify the file that renders the deployment list. The detail page lives in
the same directory; mirror the existing routing convention used by the
service detail page.

- [ ] **Step 2: Add a detail route and component**

The page polls `GET /v1/deployments/{id}` every 2 seconds for status and
opens an `EventSource` to `/v1/deployments/{id}/logs` for live log lines.
Render lines in a scrollable monospace pane. When the SSE stream emits the
`done` event, stop subscribing and freeze the pane.

- [ ] **Step 3: Verify in browser**

Build: `cd web && pnpm build` → re-run the backend → open the page after
triggering a deployment. Confirm:
- The list page shows the new deployment within 1 second.
- The detail page shows status transitions Pending → Building → Starting → Healthy (or → Failed).
- Logs stream into the pane phase-by-phase.

- [ ] **Step 4: Commit**

```bash
git add web/
git commit -m "feat(web): deployment detail page with SSE log viewer"
```

---

### Task 9: Verification

- [ ] **Step 1: Format + lint**

Run: `cargo fmt --all && cargo clippy --all-targets --all-features`
Expected: clean.

- [ ] **Step 2: All tests**

Run: `cargo test`
Expected: all green. Privileged runtime tests stay opt-in (skip).

- [ ] **Step 3: Manual repro**

```bash
sudo RUST_LOG=info,denia=debug \
     DENIA_ADMIN_TOKEN=... \
     DENIA_AGE_KEY_FILE=/home/rakei/.config/denia/age.key \
     SOPS_AGE_KEY_FILE=/home/rakei/.config/denia/age.key \
     cargo run --release
```

`curl -X POST -H "Authorization: Bearer …" -H "Content-Type: application/json" -d '{"source":"external_image","service_id":"…","image":"alpine:3"}' http://127.0.0.1:7180/v1/deployments` → expect `202` + body.

`curl -N -H "Authorization: Bearer …" http://127.0.0.1:7180/v1/deployments/{id}/logs` → expect a live `text/event-stream` with phase lines + terminal `done` event.

- [ ] **Step 4: Update GitNexus index**

Run: `npx gitnexus analyze --embeddings`

- [ ] **Step 5: No commit unless changes — verify clean tree**

Run: `git status`
Expected: nothing to commit OR a single docs follow-up.

---

## Notes for the executing agent

- Existing `tests/deploy_orchestration.rs` calls `deploy_external_image_source` directly. Once Task 4 deletes that method, port the test to call `coordinator.create_pending` + `coordinator.run_with_deps` with the same fakes.
- `src/api/deployments.rs` currently constructs the coordinator twice in the handler (once for ExternalImage, once for Git). Collapse into one construction.
- Tracing was wired in a prior step (`src/main.rs` `tracing_subscriber::fmt`). The deploy task can use both `tracing::info!` (operator-facing process log) AND `log.write(...)` (per-deployment file). Prefer `log.write` for anything the SSE viewer should show; `tracing` for control-plane diagnostics.
- Secrets discipline: never call `log.write` with a `SecretPayload`, sops stdout, key authorization, or registry credential. Only error variants (`format!("{e:?}")`) — verify each variant of `DeployError` redacts its inner payload before logging in production by inspecting `src/deploy/error.rs`.
- ADR-024 is the source of truth for behavior. If a step here conflicts with the ADR, update one of them in the same commit.
