# src/ Modularization Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Convert the flat `src/*.rs` Denia backend into folder-modules with one concern per file, and replace the single-struct `SqliteStore` with per-aggregate repository traits backed by `rusqlite` implementations.

**Architecture:** Each previously-flat multi-concern file (`app.rs`, `runtime.rs`, `state.rs`, `domain.rs`, `auth.rs`, `deploy.rs`, plus `traefik`/`bridge`/`socket_proxy` and the observability files) becomes a folder-module. `src/repo/` introduces one trait per aggregate (`ServiceRepo`, `ProjectRepo`, `UserRepo`, `DeploymentRepo`, `JobRepo`, `TokenRepo`, `CredentialRepo`). `AppState` holds `Arc<dyn …Repo>` per aggregate plus an `AppStateBuilder` for test wiring. Handlers move from `app.rs` into `src/api/<resource>.rs`. Re-exports in each `mod.rs` keep external import paths byte-stable. The API surface, DB schema, and SPA embed are unchanged.

**Tech Stack:** Rust 2024 edition, axum 0.8, rusqlite 0.39 (synchronous), tokio 1, thiserror 2, async-trait 0.1 (already in deps), tower 0.5 (`ServiceExt::oneshot` for tests), tempfile (dev-dep). No new dependencies.

**Spec:** [docs/superpowers/specs/2026-05-25-src-modularization-design.md](../specs/2026-05-25-src-modularization-design.md)
**ADR:** [docs/adr/012-src-modularization.md](../../adr/012-src-modularization.md)

---

## Important Corrections vs Spec

The design spec sketched trait shapes against `sqlx`/async. Reality of the codebase:

- `state.rs` uses **synchronous** `rusqlite::Connection` wrapped in `Arc<Mutex<>>`.
- All current persistence methods are **sync** (`fn`, not `async fn`) returning `Result<_, StateError>`.
- Aggregate lookups are by `Uuid` (`service_id`, `project_id`, etc.), not `(project, name)` strings.

The implementation in this plan uses **sync trait methods** matching current rusqlite reality, **`RepoError`** (mirrors current `StateError`), and **`Uuid` lookup keys** matching current signatures. `async_trait` is not used for repo traits — only for axum extractors that already require it.

If a future migration to async rusqlite or `sqlx` happens, that is a separate ADR. This refactor preserves current behavior.

---

## File Structure Overview

Created folder-modules (each contains a `mod.rs` re-exporting public symbols of its children):

```
src/api/        {mod, error, auth, services, deployments, workloads, projects, members, jobs, secrets, tokens, observability, ingress, health}.rs
src/domain/     {mod, error, service, deployment, project, user, credential, job}.rs
src/repo/       {mod, error, service_repo, project_repo, user_repo, deployment_repo, job_repo, token_repo, credential_repo, mock}.rs
src/repo/sqlite/{mod, pool, services, projects, users, deployments, jobs, tokens, credentials}.rs
src/runtime/    {mod, error, runtime_trait, plan, validation, fs_helpers, linux, fake}.rs
src/ingress/    {mod, traefik, bridge, socket_proxy}.rs
src/observability/{mod, metrics, node_metrics, access_log, logs}.rs
src/deploy/     {mod, error, coordinator, routes}.rs
src/auth/       {mod, principal, guards, middleware}.rs
```

Unchanged (flat) files: `main.rs`, `lib.rs` (updated mod list), `app.rs` (shrunk), `config.rs`, `command.rs`, `health.rs`, `cgroup_launcher.rs`, `scheduler.rs`, `secrets.rs`, `web.rs`. Unchanged folder-modules: `artifacts/`, `oci/`, `syscall/`.

Removed files: `src/state.rs` (after step 10).

---

## Verification Gate (Run After Every Step)

```bash
cargo build
cargo test
cargo fmt --all
cargo clippy --all-targets -- -D warnings
```

All four must pass before committing. Step 2 and step 14 additionally run:

```bash
DENIA_RUN_PRIVILEGED_TESTS=1 cargo test --test linux_runtime_privileged -- --ignored
```

---

## Task 1: Split `domain.rs` into `domain/` folder-module

**Files:**
- Delete: `src/domain.rs`
- Create: `src/domain/mod.rs`
- Create: `src/domain/error.rs`
- Create: `src/domain/service.rs`
- Create: `src/domain/deployment.rs`
- Create: `src/domain/project.rs`
- Create: `src/domain/user.rs`
- Create: `src/domain/credential.rs`
- Create: `src/domain/job.rs`
- Modify: `src/lib.rs` (no change needed — `pub mod domain;` still resolves to the folder)

**Mapping (which type goes where):**

| Source line range in `domain.rs` | Destination file | Public items |
|--|--|--|
| `DomainError` enum (line ~11) | `domain/error.rs` | `pub enum DomainError` |
| `ResourceLimits`, `HealthCheck`, `ServiceSource`, `GitSource`, `ExternalImageSource`, `ServiceConfig`, `impl ServiceConfig` | `domain/service.rs` | all `pub` |
| `Deployment`, `DeploymentRequest`, `DeploymentStatus`, `RuntimeStartRequest`, `RuntimeStatus`, `impl DeploymentRequest` | `domain/deployment.rs` | all `pub` |
| `Project`, `ProjectMembership`, `impl Project` | `domain/project.rs` | all `pub` |
| `User`, `Role`, `Session`, `ApiToken`, `Me`, `PrincipalView`, `LoginResult`, `impl User` | `domain/user.rs` | all `pub` |
| `Credential`, `CredentialKind` | `domain/credential.rs` | all `pub` |
| `Job`, `JobRun`, `JobRunRequest`, `JobRunStatus`, `JobOutcome`, `impl Job` | `domain/job.rs` | all `pub` |

- [ ] **Step 1.1: Create `domain/error.rs` with `DomainError`**

Move the `DomainError` enum verbatim. Add `use thiserror::Error;` and any imports it needs (likely none beyond `thiserror`).

- [ ] **Step 1.2: Create `domain/service.rs`**

Move `ResourceLimits`, `HealthCheck`, `ServiceSource`, `GitSource`, `ExternalImageSource`, `ServiceConfig`, and the `impl ServiceConfig` block verbatim. Imports inside the file:

```rust
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use uuid::Uuid;
use crate::domain::error::DomainError;
use crate::secrets::SecretRef;
```

Internal references to other domain types use `use super::*` or explicit `use crate::domain::...` — prefer explicit.

- [ ] **Step 1.3: Create `domain/deployment.rs`**

Move `Deployment`, `DeploymentRequest`, `DeploymentStatus`, `RuntimeStartRequest`, `RuntimeStatus` plus their impls. Imports: serde, chrono, uuid, `crate::domain::service::ServiceConfig` if referenced.

- [ ] **Step 1.4: Create `domain/project.rs`**

Move `Project`, `ProjectMembership`, `impl Project`. Imports: serde, chrono, uuid.

- [ ] **Step 1.5: Create `domain/user.rs`**

Move `User`, `Role`, `Session`, `ApiToken`, `Me`, `PrincipalView`, `LoginResult`, `impl User`. Imports: serde, chrono, uuid, `crate::domain::project::ProjectMembership` if referenced (it is, by `Me`).

- [ ] **Step 1.6: Create `domain/credential.rs`**

Move `Credential`, `CredentialKind`. Imports: serde, uuid, `crate::secrets::SecretRef`.

- [ ] **Step 1.7: Create `domain/job.rs`**

Move `Job`, `JobRun`, `JobRunRequest`, `JobRunStatus`, `JobOutcome`, `impl Job`. Imports: serde, chrono, uuid.

- [ ] **Step 1.8: Create `domain/mod.rs` with `pub use`**

```rust
pub mod credential;
pub mod deployment;
pub mod error;
pub mod job;
pub mod project;
pub mod service;
pub mod user;

pub use credential::*;
pub use deployment::*;
pub use error::*;
pub use job::*;
pub use project::*;
pub use service::*;
pub use user::*;
```

This preserves every previously-public path: `crate::domain::ServiceConfig`, `crate::domain::User`, etc.

- [ ] **Step 1.9: Delete `src/domain.rs`**

```bash
git rm src/domain.rs
```

- [ ] **Step 1.10: Verify and commit**

```bash
cargo build && cargo test && cargo fmt --all && cargo clippy --all-targets -- -D warnings
```

All four green.

```bash
git add src/domain
git commit -m "refactor(domain): split domain.rs into domain/ folder-module"
```

---

## Task 2: Split `runtime.rs` into `runtime/` folder-module

**Files:**
- Delete: `src/runtime.rs`
- Create: `src/runtime/mod.rs`
- Create: `src/runtime/error.rs`
- Create: `src/runtime/runtime_trait.rs`
- Create: `src/runtime/plan.rs`
- Create: `src/runtime/validation.rs`
- Create: `src/runtime/fs_helpers.rs`
- Create: `src/runtime/linux.rs`
- Create: `src/runtime/fake.rs`

**Mapping:**

| Symbol in current `runtime.rs` | Destination |
|--|--|
| `RuntimeError` enum | `runtime/error.rs` |
| `Runtime` trait | `runtime/runtime_trait.rs` |
| `LinuxRuntimeProcessSpec`, `LinuxRuntimePlan`, `TrackedChild` | `runtime/plan.rs` |
| `validate_service_name`, `validate_process_spec`, `validate_resource_limits`, `validate_namespace_launcher` | `runtime/validation.rs` |
| `safe_artifact_name`, `cpu_max`, `remove_dir_if_exists`, `remove_file_if_exists`, `validate_runtime_directory`, `create_runtime_directory`, `remove_existing_runtime_file`, `wait_for_cgroup_ready`, `terminate_tracked_child`, `resolve_setpriv` | `runtime/fs_helpers.rs` |
| `LinuxRuntime` struct + `impl LinuxRuntime` + `impl Runtime for LinuxRuntime` | `runtime/linux.rs` |
| `FakeRuntime` + `impl FakeRuntime` + `impl Runtime for FakeRuntime` | `runtime/fake.rs` |

Cross-file calls (e.g. `linux.rs` calling `validate_service_name`) use `use crate::runtime::validation::validate_service_name;` (or `super::validation::...`). Make every helper currently `fn` in `runtime.rs` `pub(crate)` so cross-module use compiles.

- [ ] **Step 2.1: Create `runtime/error.rs`**

Move `RuntimeError` enum verbatim with its `use thiserror::Error;`.

- [ ] **Step 2.2: Create `runtime/runtime_trait.rs`**

Move the `Runtime` trait. Imports: `crate::domain::{RuntimeStartRequest, RuntimeStatus}`, `crate::runtime::error::RuntimeError`. Trait is async (uses `async_trait`).

- [ ] **Step 2.3: Create `runtime/plan.rs`**

Move `LinuxRuntimeProcessSpec`, `LinuxRuntimePlan`, `TrackedChild`. Make each at least `pub(crate)`.

- [ ] **Step 2.4: Create `runtime/validation.rs`**

Move the four validators. Make them `pub(crate) fn ...`. Imports: `crate::domain::{ResourceLimits, RuntimeStartRequest}`, `crate::runtime::error::RuntimeError`.

- [ ] **Step 2.5: Create `runtime/fs_helpers.rs`**

Move the ten helper fns. Use `pub(crate)`. Note: `resolve_setpriv` currently takes `&Path`; keep signature. `wait_for_cgroup_ready` and `terminate_tracked_child` are async — preserve `async fn`. Imports: `std::path::{Path, PathBuf}`, `tokio::process::Child` for `TrackedChild` (re-import from `runtime::plan`), `crate::runtime::error::RuntimeError`, `crate::runtime::plan::TrackedChild`.

- [ ] **Step 2.6: Create `runtime/linux.rs`**

Move `LinuxRuntime` struct + both impl blocks. Use `use crate::runtime::{error::RuntimeError, plan::*, validation::*, fs_helpers::*, runtime_trait::Runtime};`. Use `use crate::domain::{...}` for domain types.

- [ ] **Step 2.7: Create `runtime/fake.rs`**

Move `FakeRuntime` + its impls.

- [ ] **Step 2.8: Create `runtime/mod.rs`**

```rust
pub mod error;
pub mod fake;
pub mod fs_helpers;
pub mod linux;
pub mod plan;
pub mod runtime_trait;
pub mod validation;

pub use error::RuntimeError;
pub use fake::FakeRuntime;
pub use linux::{LinuxRuntime};
pub use plan::{LinuxRuntimePlan, LinuxRuntimeProcessSpec};
pub use runtime_trait::Runtime;
```

Plus any other symbols that were re-exported via `crate::runtime::X` previously. Grep for `crate::runtime::` across the codebase to enumerate.

- [ ] **Step 2.9: Delete `src/runtime.rs`**

```bash
git rm src/runtime.rs
```

- [ ] **Step 2.10: Verify (incl. privileged tests) and commit**

```bash
cargo build && cargo test && cargo fmt --all && cargo clippy --all-targets -- -D warnings
DENIA_RUN_PRIVILEGED_TESTS=1 cargo test --test linux_runtime_privileged -- --ignored
```

If privileged tests skip (no env or no root), note in commit body.

```bash
git add src/runtime
git commit -m "refactor(runtime): split runtime.rs into runtime/ folder-module"
```

---

## Task 3: Group observability modules under `observability/`

**Files:**
- Delete: `src/metrics.rs`, `src/node_metrics.rs`, `src/access_log.rs`, `src/logs.rs`
- Create: `src/observability/mod.rs`
- Create: `src/observability/metrics.rs`
- Create: `src/observability/node_metrics.rs`
- Create: `src/observability/access_log.rs`
- Create: `src/observability/logs.rs`
- Modify: `src/lib.rs` (replace four `pub mod` lines with `pub mod observability;`)

- [ ] **Step 3.1: Move `src/metrics.rs` → `src/observability/metrics.rs`**

```bash
mkdir -p src/observability
git mv src/metrics.rs src/observability/metrics.rs
```

- [ ] **Step 3.2: Move the other three**

```bash
git mv src/node_metrics.rs src/observability/node_metrics.rs
git mv src/access_log.rs src/observability/access_log.rs
git mv src/logs.rs src/observability/logs.rs
```

- [ ] **Step 3.3: Create `src/observability/mod.rs`**

```rust
pub mod access_log;
pub mod logs;
pub mod metrics;
pub mod node_metrics;

pub use access_log::*;
pub use logs::*;
pub use metrics::*;
pub use node_metrics::*;
```

- [ ] **Step 3.4: Update `src/lib.rs`**

Remove the four old lines (`pub mod access_log;`, `pub mod logs;`, `pub mod metrics;`, `pub mod node_metrics;`) and add a single `pub mod observability;`. Re-add top-level re-exports if other code uses `crate::metrics::CgroupMetricsReader` directly — verify with grep:

```bash
grep -rn "crate::\(metrics\|node_metrics\|access_log\|logs\)" src/ tests/
```

If non-`crate::observability::*` paths exist anywhere, add to `lib.rs`:

```rust
pub use observability::{access_log, logs, metrics, node_metrics};
```

So that `crate::metrics::X` still resolves through the re-export.

- [ ] **Step 3.5: Verify and commit**

```bash
cargo build && cargo test && cargo fmt --all && cargo clippy --all-targets -- -D warnings
git add src/lib.rs src/observability
git commit -m "refactor(observability): group metrics/access_log/logs under observability/"
```

---

## Task 4: Group ingress modules under `ingress/`

**Files:**
- Delete: `src/traefik.rs`, `src/bridge.rs`, `src/socket_proxy.rs`
- Create: `src/ingress/mod.rs`
- Create: `src/ingress/traefik.rs`, `src/ingress/bridge.rs`, `src/ingress/socket_proxy.rs`
- Modify: `src/lib.rs`

- [ ] **Step 4.1: Move files**

```bash
mkdir -p src/ingress
git mv src/traefik.rs src/ingress/traefik.rs
git mv src/bridge.rs src/ingress/bridge.rs
git mv src/socket_proxy.rs src/ingress/socket_proxy.rs
```

- [ ] **Step 4.2: Create `src/ingress/mod.rs`**

```rust
pub mod bridge;
pub mod socket_proxy;
pub mod traefik;

pub use bridge::*;
pub use socket_proxy::*;
pub use traefik::*;
```

- [ ] **Step 4.3: Update `src/lib.rs`**

Remove `pub mod traefik;`, `pub mod bridge;`, `pub mod socket_proxy;`. Add `pub mod ingress;`. Add top-level re-exports if grep shows direct `crate::traefik::*` usage:

```bash
grep -rn "crate::\(traefik\|bridge\|socket_proxy\)" src/ tests/
```

If yes:

```rust
pub use ingress::{bridge, socket_proxy, traefik};
```

- [ ] **Step 4.4: Verify and commit**

```bash
cargo build && cargo test && cargo fmt --all && cargo clippy --all-targets -- -D warnings
git add src/lib.rs src/ingress
git commit -m "refactor(ingress): group traefik/bridge/socket_proxy under ingress/"
```

---

## Task 5: Split `deploy.rs` into `deploy/` folder-module

**Files:**
- Delete: `src/deploy.rs`
- Create: `src/deploy/mod.rs`
- Create: `src/deploy/error.rs`, `src/deploy/coordinator.rs`, `src/deploy/routes.rs`

Inspect `src/deploy.rs` (314 lines) to map: `DeployError`, `DeploymentCoordinator`, `SharedRoutes`. Mapping:

| Symbol | Destination |
|--|--|
| `DeployError` enum | `deploy/error.rs` |
| `DeploymentCoordinator` struct + impl + helper fns it owns | `deploy/coordinator.rs` |
| `SharedRoutes` + helpers | `deploy/routes.rs` |

- [ ] **Step 5.1: Create `deploy/error.rs`**

Move `DeployError` verbatim.

- [ ] **Step 5.2: Create `deploy/routes.rs`**

Move `SharedRoutes` and any private helper it owns.

- [ ] **Step 5.3: Create `deploy/coordinator.rs`**

Move `DeploymentCoordinator`, its `impl` block, and any private helpers used only by it.

- [ ] **Step 5.4: Create `deploy/mod.rs`**

```rust
pub mod coordinator;
pub mod error;
pub mod routes;

pub use coordinator::*;
pub use error::*;
pub use routes::*;
```

- [ ] **Step 5.5: Delete `src/deploy.rs` and verify**

```bash
git rm src/deploy.rs
cargo build && cargo test && cargo fmt --all && cargo clippy --all-targets -- -D warnings
git add src/deploy
git commit -m "refactor(deploy): split deploy.rs into deploy/ folder-module"
```

---

## Task 6: Split `auth.rs` into `auth/` folder-module

**Files:**
- Delete: `src/auth.rs`
- Create: `src/auth/mod.rs`
- Create: `src/auth/principal.rs`, `src/auth/guards.rs`, `src/auth/middleware.rs`

Inspect current `auth.rs` (158 lines). Expected mapping:

| Symbol | Destination |
|--|--|
| `Principal` struct + axum `FromRequestParts` impl | `auth/principal.rs` |
| `resolve_auth` (middleware fn) | `auth/middleware.rs` |
| `require_project_role`, `require_super_admin`, `ensure_role` (guard helpers) | `auth/guards.rs` |

- [ ] **Step 6.1: Create `auth/principal.rs`**

Move `Principal` + extractor impl. Imports: `axum::{...}`, `crate::domain::{User, Role, PrincipalView}`, `crate::app::AppState` (or use a forward decl — Principal extracts from `State<AppState>`).

- [ ] **Step 6.2: Create `auth/guards.rs`**

Move `require_project_role`, `require_super_admin`, `ensure_role`. Take `&Principal` + identifiers; return `Result<(), ApiError>` (or whatever current error type is).

- [ ] **Step 6.3: Create `auth/middleware.rs`**

Move `resolve_auth` axum middleware fn.

- [ ] **Step 6.4: Create `auth/mod.rs`**

```rust
pub mod guards;
pub mod middleware;
pub mod principal;

pub use guards::*;
pub use middleware::*;
pub use principal::*;
```

- [ ] **Step 6.5: Delete and verify**

```bash
git rm src/auth.rs
cargo build && cargo test && cargo fmt --all && cargo clippy --all-targets -- -D warnings
git add src/auth
git commit -m "refactor(auth): split auth.rs into auth/ folder-module"
```

---

## Task 7: Create `repo/` skeleton (traits + `RepoError` + pool ctor only)

**Files:**
- Create: `src/repo/mod.rs`
- Create: `src/repo/error.rs`
- Create: `src/repo/service_repo.rs`
- Create: `src/repo/project_repo.rs`
- Create: `src/repo/user_repo.rs`
- Create: `src/repo/deployment_repo.rs`
- Create: `src/repo/job_repo.rs`
- Create: `src/repo/token_repo.rs`
- Create: `src/repo/credential_repo.rs`
- Create: `src/repo/sqlite/mod.rs`
- Create: `src/repo/sqlite/pool.rs`
- Modify: `src/lib.rs` (add `pub mod repo;`)

This task is **additive** — `SqliteStore` keeps working. New code is unused until task 9.

- [ ] **Step 7.1: Create `src/repo/error.rs`**

```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RepoError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("uuid error: {0}")]
    Uuid(#[from] uuid::Error),
    #[error("time parse error: {0}")]
    Time(#[from] chrono::ParseError),
    #[error("state lock poisoned")]
    LockPoisoned,
    #[error("not found")]
    NotFound,
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("project not empty")]
    ProjectNotEmpty,
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error("last super admin")]
    LastSuperAdmin,
}
```

`RepoError` mirrors current `StateError` so the eventual replacement is a 1:1 swap. Variants preserved to avoid behavior change at error mapping sites.

- [ ] **Step 7.2: Create `src/repo/service_repo.rs`**

Signatures derived from current `state.rs` methods (sync, `Uuid` keys). Use `#[allow(dead_code)]` on the trait until task 9 wires consumers — the lint will fire otherwise.

```rust
use uuid::Uuid;
use crate::domain::ServiceConfig;
use crate::repo::error::RepoError;

#[allow(dead_code)]
pub trait ServiceRepo: Send + Sync + 'static {
    fn put_service(&self, config: ServiceConfig) -> Result<ServiceConfig, RepoError>;
    fn list_services(&self) -> Result<Vec<ServiceConfig>, RepoError>;
    fn get_service(&self, service_id: Uuid) -> Result<Option<ServiceConfig>, RepoError>;
}
```

- [ ] **Step 7.3: Create `src/repo/project_repo.rs`**

```rust
use uuid::Uuid;
use crate::domain::Project;
use crate::repo::error::RepoError;

#[allow(dead_code)]
pub trait ProjectRepo: Send + Sync + 'static {
    fn default_project_id(&self) -> Result<Uuid, RepoError>;
    fn put_project(&self, project: Project) -> Result<Project, RepoError>;
    fn get_project(&self, project_id: Uuid) -> Result<Option<Project>, RepoError>;
    fn list_projects(&self) -> Result<Vec<Project>, RepoError>;
    fn count_services_in_project(&self, project_id: Uuid) -> Result<i64, RepoError>;
    fn delete_project(&self, project_id: Uuid) -> Result<(), RepoError>;
}
```

- [ ] **Step 7.4: Create `src/repo/user_repo.rs`**

```rust
use uuid::Uuid;
use crate::domain::{Project, ProjectMembership, Role, Session, User};
use crate::repo::error::RepoError;

#[allow(dead_code)]
pub trait UserRepo: Send + Sync + 'static {
    fn create_user(&self, username: &str, password: &str, super_admin: bool) -> Result<User, RepoError>;
    fn get_user(&self, user_id: Uuid) -> Result<Option<User>, RepoError>;
    fn list_users(&self) -> Result<Vec<User>, RepoError>;
    fn delete_user(&self, user_id: Uuid) -> Result<(), RepoError>;
    fn verify_login(&self, username: &str, password: &str) -> Result<User, RepoError>;
    fn create_session(&self, user_id: Uuid, ttl_hours: i64) -> Result<Session, RepoError>;
    fn user_for_session(&self, token_hash: &str) -> Result<Option<User>, RepoError>;
    fn delete_session(&self, token_hash: &str) -> Result<(), RepoError>;
    fn set_membership(&self, user_id: Uuid, project_id: Uuid, role: Role) -> Result<(), RepoError>;
    fn role_for(&self, user_id: Uuid, project_id: Uuid) -> Result<Option<Role>, RepoError>;
    fn list_members(&self, project_id: Uuid) -> Result<Vec<ProjectMembership>, RepoError>;
    fn remove_membership(&self, user_id: Uuid, project_id: Uuid) -> Result<(), RepoError>;
    fn list_memberships_for_user(&self, user_id: Uuid) -> Result<Vec<(Project, Role)>, RepoError>;
}
```

Verify each signature against `state.rs` — adjust arg types and return shapes to match exactly. (E.g. `set_membership` may take `(&User, &Project, Role)` instead — read current source first.)

- [ ] **Step 7.5: Create `src/repo/deployment_repo.rs`**

```rust
use uuid::Uuid;
use crate::domain::{Deployment, DeploymentRequest, DeploymentStatus};
use crate::repo::error::RepoError;

#[allow(dead_code)]
pub trait DeploymentRepo: Send + Sync + 'static {
    fn create_deployment(&self, request: DeploymentRequest) -> Result<Deployment, RepoError>;
    fn list_deployments(&self, service_id: Uuid) -> Result<Vec<Deployment>, RepoError>;
    fn update_deployment_status(&self, deployment_id: Uuid, status: DeploymentStatus) -> Result<(), RepoError>;
    fn promote_deployment(&self, deployment_id: Uuid, service_id: Uuid) -> Result<(), RepoError>;
    fn promoted_deployment(&self, service_id: Uuid) -> Result<Option<Uuid>, RepoError>;
    fn clear_promoted_deployment(&self, service_id: Uuid) -> Result<(), RepoError>;
}
```

- [ ] **Step 7.6: Create `src/repo/job_repo.rs`**

```rust
use chrono::{DateTime, Utc};
use uuid::Uuid;
use crate::domain::{Job, JobRun};
use crate::repo::error::RepoError;

#[allow(dead_code)]
pub trait JobRepo: Send + Sync + 'static {
    fn put_job(&self, job: Job) -> Result<Job, RepoError>;
    fn get_job(&self, job_id: Uuid) -> Result<Option<Job>, RepoError>;
    fn list_jobs(&self, project_id: Uuid) -> Result<Vec<Job>, RepoError>;
    fn delete_job(&self, job_id: Uuid) -> Result<(), RepoError>;
    fn create_job_run(&self, job_id: Uuid) -> Result<JobRun, RepoError>;
    fn list_job_runs(&self, job_id: Uuid) -> Result<Vec<JobRun>, RepoError>;
    fn update_job_run(&self, run_id: Uuid, /* args matching current signature */) -> Result<(), RepoError>;
    fn active_run(&self, job_id: Uuid) -> Result<Option<JobRun>, RepoError>;
    fn fail_orphan_runs(&self) -> Result<usize, RepoError>;
    fn claim_due_jobs(&self, now: DateTime<Utc>) -> Result<Vec<Job>, RepoError>;
    fn set_job_next_run(&self, /* args matching current signature */) -> Result<(), RepoError>;
}
```

Open `src/state.rs` and copy exact `update_job_run`, `set_job_next_run`, `create_job_run`, etc. argument lists. Do not guess.

- [ ] **Step 7.7: Create `src/repo/token_repo.rs`**

```rust
use uuid::Uuid;
use crate::domain::{ApiToken, User};
use crate::repo::error::RepoError;

#[allow(dead_code)]
pub trait TokenRepo: Send + Sync + 'static {
    fn create_api_token(&self, user_id: Uuid, name: &str) -> Result<ApiToken, RepoError>;
    fn user_for_api_token(&self, token_hash: &str) -> Result<Option<User>, RepoError>;
    fn list_api_tokens(&self, user_id: Uuid) -> Result<Vec<ApiToken>, RepoError>;
    fn revoke_api_token(&self, token_id: Uuid) -> Result<(), RepoError>;
}
```

- [ ] **Step 7.8: Create `src/repo/credential_repo.rs`**

```rust
use uuid::Uuid;
use crate::domain::{Credential, CredentialKind};
use crate::repo::error::RepoError;
use crate::secrets::SecretRef;

#[allow(dead_code)]
pub trait CredentialRepo: Send + Sync + 'static {
    fn put_credential(&self, /* current signature */) -> Result<(), RepoError>;
}
```

Open `state.rs:348` for the exact signature.

- [ ] **Step 7.9: Create `src/repo/sqlite/pool.rs`**

```rust
use rusqlite::Connection;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::repo::error::RepoError;

#[derive(Clone)]
pub struct SqlitePool {
    pub(crate) inner: Arc<Mutex<Connection>>,
}

impl SqlitePool {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, RepoError> {
        Ok(Self {
            inner: Arc::new(Mutex::new(Connection::open(path)?)),
        })
    }

    pub fn open_in_memory() -> Result<Self, RepoError> {
        Ok(Self {
            inner: Arc::new(Mutex::new(Connection::open_in_memory()?)),
        })
    }

    pub(crate) fn connection(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, Connection>, RepoError> {
        self.inner.lock().map_err(|_| RepoError::LockPoisoned)
    }
}

pub fn run_migrations(pool: &SqlitePool) -> Result<(), RepoError> {
    // Placeholder; real impl moves the migrate() body from state.rs into this fn in task 8.
    let _ = pool;
    Ok(())
}
```

- [ ] **Step 7.10: Create `src/repo/sqlite/mod.rs`**

```rust
pub mod pool;
pub use pool::{SqlitePool, run_migrations};
```

- [ ] **Step 7.11: Create `src/repo/mod.rs`**

```rust
pub mod credential_repo;
pub mod deployment_repo;
pub mod error;
pub mod job_repo;
pub mod project_repo;
pub mod service_repo;
pub mod sqlite;
pub mod token_repo;
pub mod user_repo;

pub use credential_repo::CredentialRepo;
pub use deployment_repo::DeploymentRepo;
pub use error::RepoError;
pub use job_repo::JobRepo;
pub use project_repo::ProjectRepo;
pub use service_repo::ServiceRepo;
pub use token_repo::TokenRepo;
pub use user_repo::UserRepo;
```

- [ ] **Step 7.12: Update `src/lib.rs`**

Add `pub mod repo;`.

- [ ] **Step 7.13: Verify and commit**

```bash
cargo build && cargo test && cargo fmt --all && cargo clippy --all-targets -- -D warnings
git add src/repo src/lib.rs
git commit -m "refactor(repo): add per-aggregate repo traits + RepoError skeleton"
```

`#[allow(dead_code)]` on each trait silences "unused" warnings; clippy passes.

---

## Task 8: Split `state.rs` impl across `repo/sqlite/<aggregate>.rs` (single struct kept)

**Files:**
- Modify: `src/state.rs` — strip per-aggregate impl blocks
- Create: `src/repo/sqlite/services.rs`, `projects.rs`, `users.rs`, `deployments.rs`, `jobs.rs`, `tokens.rs`, `credentials.rs`
- Modify: `src/repo/sqlite/pool.rs` — move `migrate` body here as `run_migrations`

Behavior unchanged. `SqliteStore` keeps its full public method surface; methods are now defined in per-aggregate impl blocks across files via Rust's split-impl feature:

```rust
// src/repo/sqlite/services.rs
impl crate::state::SqliteStore {
    pub fn put_service(&self, config: ServiceConfig) -> Result<ServiceConfig, StateError> { /* moved */ }
    pub fn list_services(&self) -> Result<Vec<ServiceConfig>, StateError> { /* moved */ }
    pub fn get_service(&self, service_id: Uuid) -> Result<Option<ServiceConfig>, StateError> { /* moved */ }
}
```

This is the **minimum-risk step 8**: no signature changes, no type renames, no caller changes. Just file relocation of impl blocks.

- [ ] **Step 8.1: Read current `state.rs` end-to-end**

Use Read to load the whole file. Identify every method and assign to an aggregate file. The boundaries are clear from method names (`put_service`, `list_projects`, `create_user`, etc.).

- [ ] **Step 8.2: Move `migrate()` body to `repo/sqlite/pool.rs::run_migrations`**

Strip the placeholder body from step 7.9 and paste in the real migration `execute_batch` calls. Keep schema_version logic intact. Imports adjusted (`rusqlite::Connection`, etc.). Replace `state.rs::migrate` impl with:

```rust
impl SqliteStore {
    pub fn migrate(&self) -> Result<(), StateError> {
        let pool = crate::repo::sqlite::SqlitePool { inner: Arc::clone(&self.connection) };
        crate::repo::sqlite::run_migrations(&pool).map_err(StateError::from)
    }
}
```

Add `impl From<RepoError> for StateError` (or vice versa) — temporary glue, deleted in task 10. Use `From<RepoError>` so `RepoError::Sqlite(e)` maps to `StateError::Sqlite(e)`, etc.

- [ ] **Step 8.3: Create `repo/sqlite/services.rs`**

Move `put_service`, `list_services`, `get_service` (and any private helpers used only by these) from `state.rs`. Keep method signatures verbatim. Add file header:

```rust
use uuid::Uuid;
use rusqlite::{OptionalExtension, params};

use crate::domain::ServiceConfig;
use crate::state::{SqliteStore, StateError};
```

If a private helper (e.g. row parsing) is used only here, move it too as a free fn. If shared across aggregates, leave in `state.rs` (will move in task 9).

- [ ] **Step 8.4: Create `repo/sqlite/projects.rs`**

Move `default_project_id`, `put_project`, `get_project`, `list_projects`, `count_services_in_project`, `delete_project`.

- [ ] **Step 8.5: Create `repo/sqlite/users.rs`**

Move `create_user`, `get_user`, `list_users`, `delete_user`, `verify_login`, `create_session`, `user_for_session`, `delete_session`, `set_membership`, `role_for`, `list_members`, `remove_membership`, `list_memberships_for_user`.

- [ ] **Step 8.6: Create `repo/sqlite/deployments.rs`**

Move `create_deployment`, `list_deployments`, `update_deployment_status`, `promote_deployment`, `promoted_deployment`, `clear_promoted_deployment`. Also `put_artifact`, `list_artifacts` — these are deployment-adjacent. Decision: keep with deployments.

- [ ] **Step 8.7: Create `repo/sqlite/jobs.rs`**

Move `put_job`, `get_job`, `list_jobs`, `delete_job`, `create_job_run`, `list_job_runs`, `update_job_run`, `active_run`, `fail_orphan_runs`, `claim_due_jobs`, `set_job_next_run`.

- [ ] **Step 8.8: Create `repo/sqlite/tokens.rs`**

Move `create_api_token`, `user_for_api_token`, `list_api_tokens`, `revoke_api_token`.

- [ ] **Step 8.9: Create `repo/sqlite/credentials.rs`**

Move `put_credential`.

- [ ] **Step 8.10: Update `repo/sqlite/mod.rs`**

```rust
pub mod credentials;
pub mod deployments;
pub mod jobs;
pub mod pool;
pub mod projects;
pub mod services;
pub mod tokens;
pub mod users;

pub use pool::{SqlitePool, run_migrations};
```

- [ ] **Step 8.11: `src/state.rs` should now contain only**

- `StateError` enum
- `SqliteStore` struct definition
- `open`, `open_in_memory`, `migrate`, `schema_version`, `connection()` helper
- `DeploymentRow` (line 1123) and any other private row struct used by impls — move it to `repo/sqlite/deployments.rs` if only used there

Run `cargo build` after each file move to catch missed helpers.

- [ ] **Step 8.12: Verify and commit**

```bash
cargo build && cargo test && cargo fmt --all && cargo clippy --all-targets -- -D warnings
git add src/state.rs src/repo/sqlite
git commit -m "refactor(repo): split SqliteStore impl across per-aggregate files"
```

---

## Task 9: Introduce per-aggregate `Sqlite*Repo` structs implementing the traits (adapter pattern)

**Files:**
- Modify: `src/repo/sqlite/{services,projects,users,deployments,jobs,tokens,credentials}.rs`
- Modify: `src/state.rs`

This task adds new structs alongside `SqliteStore`. `SqliteStore` still exists. Handlers don't change yet.

Pattern per aggregate (example for services):

```rust
// src/repo/sqlite/services.rs

use std::sync::{Arc, Mutex};
use rusqlite::Connection;
use uuid::Uuid;

use crate::domain::ServiceConfig;
use crate::repo::error::RepoError;
use crate::repo::service_repo::ServiceRepo;
use crate::repo::sqlite::pool::SqlitePool;

pub struct SqliteServiceRepo {
    pool: SqlitePool,
}

impl SqliteServiceRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl ServiceRepo for SqliteServiceRepo {
    fn put_service(&self, config: ServiceConfig) -> Result<ServiceConfig, RepoError> {
        let conn = self.pool.connection()?;
        /* SQL body lifted from SqliteStore::put_service, with StateError → RepoError mapping */
    }
    fn list_services(&self) -> Result<Vec<ServiceConfig>, RepoError> { /* ... */ }
    fn get_service(&self, service_id: Uuid) -> Result<Option<ServiceConfig>, RepoError> { /* ... */ }
}
```

`SqliteStore::put_service` etc. become thin forwarders to the new struct **OR** stay as duplicate impls. **Decision: keep `SqliteStore` methods identical, do not call new impls from old ones**. Reason: avoids any subtle behavior change. The duplicate code lives for one commit (this one); task 10 deletes the old.

If duplication makes you uneasy, the alternative is to extract a `pub(crate) fn put_service_impl(conn: &Connection, config: ServiceConfig) -> Result<ServiceConfig, RepoError>` free fn that both call. Pick whichever is faster.

- [ ] **Step 9.1: Add `SqliteServiceRepo`** in `repo/sqlite/services.rs` per pattern above.

- [ ] **Step 9.2: Add `SqliteProjectRepo`** in `repo/sqlite/projects.rs`.

- [ ] **Step 9.3: Add `SqliteUserRepo`** in `repo/sqlite/users.rs`. Note `UserRepo` is the largest — 13 methods. Reuse existing query bodies; only the receiver and error type change.

- [ ] **Step 9.4: Add `SqliteDeploymentRepo`** in `repo/sqlite/deployments.rs`. Move `DeploymentRow` here if not already.

- [ ] **Step 9.5: Add `SqliteJobRepo`** in `repo/sqlite/jobs.rs`.

- [ ] **Step 9.6: Add `SqliteTokenRepo`** in `repo/sqlite/tokens.rs`.

- [ ] **Step 9.7: Add `SqliteCredentialRepo`** in `repo/sqlite/credentials.rs`.

- [ ] **Step 9.8: Re-export from `repo/sqlite/mod.rs`**

```rust
pub use credentials::SqliteCredentialRepo;
pub use deployments::SqliteDeploymentRepo;
pub use jobs::SqliteJobRepo;
pub use pool::{SqlitePool, run_migrations};
pub use projects::SqliteProjectRepo;
pub use services::SqliteServiceRepo;
pub use tokens::SqliteTokenRepo;
pub use users::SqliteUserRepo;
```

- [ ] **Step 9.9: Add `Sqlite*Repo::from_store(store: &SqliteStore) -> Self` helpers** in each repo file — temporary adapter for task 10. They reuse the pool by cloning `Arc<Mutex<Connection>>`:

```rust
impl SqliteServiceRepo {
    pub fn from_store(store: &crate::state::SqliteStore) -> Self {
        Self { pool: SqlitePool { inner: Arc::clone(&store.connection) } }
    }
}
```

Make `SqliteStore::connection` field `pub(crate)` if it isn't already.

- [ ] **Step 9.10: Verify and commit**

```bash
cargo build && cargo test && cargo fmt --all && cargo clippy --all-targets -- -D warnings
git add src/repo src/state.rs
git commit -m "refactor(repo): add Sqlite*Repo structs implementing each repo trait"
```

Clippy will warn about duplicate code if step 9 used the duplicate approach — use `#[allow(clippy::too_many_lines)]` only where a method legitimately exceeds the threshold; do not blanket-allow duplication.

---

## Task 10: Rewire `AppState` to hold `Arc<dyn …Repo>` per aggregate + `AppStateBuilder`

**Files:**
- Modify: `src/app.rs` — `AppState` struct + constructors
- Modify: every handler in `src/app.rs` — change `state.store.put_service(...)` to `state.services.put_service(...)`
- Modify: `src/main.rs` — adjust `AppState` construction
- Modify: `src/deploy/coordinator.rs` and any other consumer of `SqliteStore`
- Delete: `src/state.rs`
- Delete: `from_store` adapter helpers from step 9

This is the highest-risk task. Single commit. Plan:

1. Update `AppState` shape.
2. Sweep every reference to `state.store.X` → `state.<aggregate>.X`.
3. Update constructors.
4. Delete `SqliteStore`.
5. Delete adapters.
6. Verify.

If at any sub-step `cargo build` fails, do NOT commit. Fix or revert.

- [ ] **Step 10.1: Update `AppState`**

```rust
// src/app.rs
use std::sync::Arc;

use crate::repo::{
    CredentialRepo, DeploymentRepo, JobRepo, ProjectRepo, ServiceRepo, TokenRepo, UserRepo,
};

#[derive(Clone)]
pub struct AppState {
    pub config: AppConfig,
    pub services:        Arc<dyn ServiceRepo>,
    pub projects:        Arc<dyn ProjectRepo>,
    pub users:           Arc<dyn UserRepo>,
    pub deployments:     Arc<dyn DeploymentRepo>,
    pub jobs:            Arc<dyn JobRepo>,
    pub tokens:          Arc<dyn TokenRepo>,
    pub credentials:     Arc<dyn CredentialRepo>,
    pub runtime:         Arc<dyn Runtime>,
    pub access_log:      AccessLogStore,
    // ... keep other current fields exactly as they were
}
```

- [ ] **Step 10.2: Add `AppStateBuilder`** (in `src/app.rs`)

```rust
pub struct AppStateBuilder {
    config: Option<AppConfig>,
    services: Option<Arc<dyn ServiceRepo>>,
    projects: Option<Arc<dyn ProjectRepo>>,
    users: Option<Arc<dyn UserRepo>>,
    deployments: Option<Arc<dyn DeploymentRepo>>,
    jobs: Option<Arc<dyn JobRepo>>,
    tokens: Option<Arc<dyn TokenRepo>>,
    credentials: Option<Arc<dyn CredentialRepo>>,
    runtime: Option<Arc<dyn Runtime>>,
    access_log: Option<AccessLogStore>,
    // ... etc
}

impl AppStateBuilder {
    pub fn new() -> Self { Self { config: None, services: None, /* ... all None */ } }
    pub fn config(mut self, c: AppConfig) -> Self { self.config = Some(c); self }
    pub fn services(mut self, r: Arc<dyn ServiceRepo>) -> Self { self.services = Some(r); self }
    // ... one setter per field
    pub fn build(self) -> AppState {
        AppState {
            config: self.config.expect("config required"),
            services: self.services.expect("services repo required"),
            // ... unwrap each
        }
    }
}

impl AppState {
    pub fn new(config: AppConfig, pool: SqlitePool) -> Self {
        AppStateBuilder::new()
            .config(config)
            .services(Arc::new(SqliteServiceRepo::new(pool.clone())))
            .projects(Arc::new(SqliteProjectRepo::new(pool.clone())))
            .users(Arc::new(SqliteUserRepo::new(pool.clone())))
            .deployments(Arc::new(SqliteDeploymentRepo::new(pool.clone())))
            .jobs(Arc::new(SqliteJobRepo::new(pool.clone())))
            .tokens(Arc::new(SqliteTokenRepo::new(pool.clone())))
            .credentials(Arc::new(SqliteCredentialRepo::new(pool)))
            .runtime(/* default production runtime */)
            .access_log(/* ... */)
            .build()
    }

    pub fn builder() -> AppStateBuilder { AppStateBuilder::new() }
}
```

- [ ] **Step 10.3: Sweep handler call sites**

For each handler in `app.rs`, replace `state.store.<method>` with `state.<aggregate>.<method>`. Mapping by current method:

| Current method on `store` | New field |
|--|--|
| `put_service`, `list_services`, `get_service` | `state.services` |
| `*_project*`, `count_services_in_project`, `default_project_id` | `state.projects` |
| `*_user*`, `*_session*`, `*_membership*`, `verify_login`, `role_for`, `list_members`, `list_memberships_for_user` | `state.users` |
| `*_deployment*`, `*_artifact*`, `promote*`, `promoted_deployment`, `clear_promoted_deployment` | `state.deployments` |
| `*_job*`, `*_run*`, `claim_due_jobs`, `fail_orphan_runs`, `set_job_next_run`, `active_run` | `state.jobs` |
| `*_api_token*` | `state.tokens` |
| `put_credential` | `state.credentials` |

Mechanical sweep with grep + edit:

```bash
grep -n "state\.store\." src/app.rs
```

Map each match. Same sweep in `src/deploy/coordinator.rs` and any other consumer.

- [ ] **Step 10.4: Update error mapping**

Handlers currently map `StateError` → `ApiError`. After task 10, handlers see `RepoError`. Update the `From<StateError> for ApiError` impl to `From<RepoError> for ApiError`, mapping each variant. Keep the same HTTP status codes. (This impl can move to `api/error.rs` in task 12; for now, leave it where it is.)

- [ ] **Step 10.5: Update `src/main.rs`**

Replace `SqliteStore::open` + `migrate` with:

```rust
let pool = SqlitePool::open(&config.sqlite_path)?;
run_migrations(&pool)?;
let state = AppState::new(config, pool);
```

- [ ] **Step 10.6: Delete `src/state.rs`**

```bash
git rm src/state.rs
```

Delete the `from_store` adapter methods on each `Sqlite*Repo` (no longer needed).

- [ ] **Step 10.7: Verify and commit**

```bash
cargo build && cargo test && cargo fmt --all && cargo clippy --all-targets -- -D warnings
git add src/app.rs src/main.rs src/repo src/deploy
git commit -m "refactor(app): replace SqliteStore with per-aggregate repo traits in AppState"
```

If any test fails, fix in this commit. Do not split into a fix-up commit on master — keep the refactor atomic.

---

## Task 11: Extract handlers from `app.rs` into `api/<resource>.rs`

**Files:**
- Create: `src/api/mod.rs`
- Create: `src/api/auth.rs`, `services.rs`, `deployments.rs`, `workloads.rs`, `projects.rs`, `members.rs`, `jobs.rs`, `secrets.rs`, `tokens.rs`, `observability.rs`, `ingress.rs`, `health.rs`
- Modify: `src/app.rs` — leave only `AppState`, `AppStateBuilder`, `build_router`
- Modify: `src/lib.rs` — add `pub mod api;`

For each resource, the handler functions and their routes move together. The pattern per file:

```rust
// src/api/services.rs

use axum::{
    Json, Router,
    extract::{Path, State},
    routing::{delete, get},
};

use crate::app::AppState;
use crate::auth::{Principal, require_project_role};
use crate::domain::{Role, ServiceConfig};
use crate::api::error::ApiError;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/services", get(list).put(put))
        .route("/v1/services/{id}", get(get_one).delete(delete_one))
        // exact paths copied verbatim from current app.rs
}

async fn list(
    State(st): State<AppState>,
    principal: Principal,
    // path/query params as before
) -> Result<Json<Vec<ServiceConfig>>, ApiError> {
    require_project_role(&principal, /* project id */, Role::Viewer)?;
    Ok(Json(st.services.list_services()?))
}
// put, get_one, delete_one — bodies lifted verbatim from app.rs
```

**Route extraction rule:** before deleting a route from `app.rs::build_router`, ensure the new `api::<x>::router()` defines the **exact same path string and method**. Do not normalize, deduplicate, or reorganize routes during this task.

- [ ] **Step 11.1: Create `src/api/mod.rs`** (placeholder — fill in as you add modules)

```rust
pub mod auth;
pub mod deployments;
pub mod error;
pub mod health;
pub mod ingress;
pub mod jobs;
pub mod members;
pub mod observability;
pub mod projects;
pub mod secrets;
pub mod services;
pub mod tokens;
pub mod workloads;
```

`api/error.rs` is empty for now — created in task 12.

- [ ] **Step 11.2: Add `pub mod api;` to `src/lib.rs`**

- [ ] **Step 11.3: Extract `api/auth.rs`** — `/v1/auth/login`, `/v1/auth/logout`, `/v1/auth/me` etc. Each handler body lifted verbatim from `app.rs`. Exports `pub fn router() -> Router<AppState>`.

- [ ] **Step 11.4: Extract `api/services.rs`** — every route under `/v1/projects/:project/services` (and any plain `/v1/services` if present).

- [ ] **Step 11.5: Extract `api/deployments.rs`** — `/v1/projects/:project/deployments/*`, `create_deployment` handler.

- [ ] **Step 11.6: Extract `api/workloads.rs`** — `/v1/projects/:project/workloads/*`, `WorkloadView` (move struct too if local to this module).

- [ ] **Step 11.7: Extract `api/projects.rs`** — `/v1/projects` list/create/get/delete.

- [ ] **Step 11.8: Extract `api/members.rs`** — `/v1/projects/:project/members/*`.

- [ ] **Step 11.9: Extract `api/jobs.rs`** — `/v1/projects/:project/jobs/*` and job-run routes.

- [ ] **Step 11.10: Extract `api/secrets.rs`** — `/v1/projects/:project/secrets/*`.

- [ ] **Step 11.11: Extract `api/tokens.rs`** — `/v1/tokens/*`.

- [ ] **Step 11.12: Extract `api/observability.rs`** — `/v1/node` (node metrics), `/v1/projects/:project/access-log`, `/v1/projects/:project/logs/*`, `get_node_metrics` handler.

- [ ] **Step 11.13: Extract `api/ingress.rs`** — `/v1/projects/:project/routes/*` route inspection.

- [ ] **Step 11.14: Extract `api/health.rs`** — `pub async fn healthz() -> Json<HealthResponse>`. Plus `HealthResponse` struct.

- [ ] **Step 11.15: Rewrite `src/app.rs::build_router`**

```rust
pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(crate::api::health::healthz))
        .merge(api::auth::router())
        .merge(api::services::router())
        .merge(api::deployments::router())
        .merge(api::workloads::router())
        .merge(api::projects::router())
        .merge(api::members::router())
        .merge(api::jobs::router())
        .merge(api::secrets::router())
        .merge(api::tokens::router())
        .merge(api::observability::router())
        .merge(api::ingress::router())
        .layer(middleware::from_fn_with_state(state.clone(), crate::auth::resolve_auth))
        .fallback_service(crate::web::spa_service())
        .with_state(state)
}
```

**Verify**: `app.rs` is now ~150 lines: `AppState` + `AppStateBuilder` + `build_router` + nothing else.

- [ ] **Step 11.16: Verify route equivalence**

```bash
cargo build && cargo test
```

If integration tests check specific endpoints, all pass. Manual smoke:

```bash
cargo run &
PID=$!
sleep 2
curl -s -o /dev/null -w "%{http_code}\n" http://127.0.0.1:7180/healthz   # expect 200
kill $PID
```

- [ ] **Step 11.17: Final verify + commit**

```bash
cargo fmt --all && cargo clippy --all-targets -- -D warnings
git add src/api src/app.rs src/lib.rs
git commit -m "refactor(api): extract handlers from app.rs into per-resource modules"
```

---

## Task 12: Centralize `ApiError` in `api/error.rs` + unify handler error mapping

**Files:**
- Modify: `src/api/error.rs` (was empty)
- Modify: every `src/api/<resource>.rs` (only if their handler signatures or local error impls change)

- [ ] **Step 12.1: Move `ApiError` to `api/error.rs`**

`ApiError` currently lives in `app.rs` (or wherever the legacy `axum::IntoResponse` impl is). Cut and paste into `api/error.rs`. Imports:

```rust
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

use crate::deploy::DeployError;
use crate::domain::DomainError;
use crate::repo::RepoError;
use crate::runtime::RuntimeError;

#[derive(Debug, Serialize)]
pub struct ApiErrorBody {
    pub error: String,
    pub detail: Option<String>,
}

#[derive(Debug)]
pub enum ApiError {
    BadRequest(String),
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict(String),
    Internal(String),
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, body) = match self {
            ApiError::BadRequest(d)  => (StatusCode::BAD_REQUEST, ApiErrorBody { error: "bad_request".into(), detail: Some(d) }),
            ApiError::Unauthorized   => (StatusCode::UNAUTHORIZED, ApiErrorBody { error: "unauthorized".into(), detail: None }),
            ApiError::Forbidden      => (StatusCode::FORBIDDEN, ApiErrorBody { error: "forbidden".into(), detail: None }),
            ApiError::NotFound       => (StatusCode::NOT_FOUND, ApiErrorBody { error: "not_found".into(), detail: None }),
            ApiError::Conflict(d)    => (StatusCode::CONFLICT, ApiErrorBody { error: "conflict".into(), detail: Some(d) }),
            ApiError::Internal(d)    => (StatusCode::INTERNAL_SERVER_ERROR, ApiErrorBody { error: "internal".into(), detail: Some(d) }),
        };
        (status, Json(body)).into_response()
    }
}
```

Adjust shape to match the **current** `ApiError` in `app.rs` — do not introduce new variants or change status codes during this refactor. Goal: byte-identical responses.

- [ ] **Step 12.2: Implement `From<RepoError>`, `From<DomainError>`, `From<RuntimeError>`, `From<DeployError>` for `ApiError`**

Copy mapping from current `app.rs`. Each conversion stays semantically identical.

```rust
impl From<RepoError> for ApiError {
    fn from(e: RepoError) -> Self {
        match e {
            RepoError::NotFound => ApiError::NotFound,
            RepoError::Conflict(d) => ApiError::Conflict(d),
            RepoError::InvalidCredentials => ApiError::Unauthorized,
            RepoError::ProjectNotEmpty => ApiError::Conflict("project not empty".into()),
            RepoError::LastSuperAdmin => ApiError::Conflict("last super admin".into()),
            _ => ApiError::Internal(e.to_string()),
        }
    }
}
// same for DomainError, RuntimeError, DeployError
```

- [ ] **Step 12.3: Remove the old `ApiError` from `app.rs`**

- [ ] **Step 12.4: Confirm every handler imports `crate::api::error::ApiError`**

```bash
grep -rn "ApiError" src/api/
```

All should resolve to `crate::api::error::ApiError`.

- [ ] **Step 12.5: Verify and commit**

```bash
cargo build && cargo test && cargo fmt --all && cargo clippy --all-targets -- -D warnings
git add src/api src/app.rs
git commit -m "refactor(api): centralize ApiError + From<*> conversions in api/error.rs"
```

---

## Task 13: Add `repo/mock.rs` in-memory impls + repo contract tests + handler unit tests

**Files:**
- Create: `src/repo/mock.rs`
- Modify: `src/repo/mod.rs` (add `#[cfg(any(test, feature = "test-support"))] pub mod mock;`)
- Modify: `Cargo.toml` (add `[features] test-support = []`)
- Create: `tests/repo/services.rs`, `projects.rs`, `users.rs`, `deployments.rs`, `jobs.rs`, `tokens.rs`, `credentials.rs`
- Create: `tests/api/services.rs`, `auth.rs`, `projects.rs` (others as time permits)

These are new tests. Write tests first; they should fail with "method not implemented" until the in-memory impls are written.

- [ ] **Step 13.1: Add `test-support` cargo feature**

```toml
# Cargo.toml
[features]
default = []
ecr = []
gar = []
test-support = []
```

- [ ] **Step 13.2: Write the failing test for `InMemoryServiceRepo`**

```rust
// tests/repo/services.rs

use std::sync::Arc;
use denia::domain::{ResourceLimits, ServiceConfig, ServiceSource, ExternalImageSource};
use denia::repo::mock::InMemoryServiceRepo;
use denia::repo::service_repo::ServiceRepo;
use uuid::Uuid;

fn sample_service() -> ServiceConfig {
    ServiceConfig {
        id: Uuid::now_v7(),
        project_id: Uuid::now_v7(),
        name: "web".into(),
        source: ServiceSource::ExternalImage(ExternalImageSource { /* min fields */ }),
        env: Default::default(),
        secrets: Default::default(),
        ports: vec![],
        resource_limits: ResourceLimits::default(),
        health_check: None,
        // ... whatever else ServiceConfig requires
    }
}

#[test]
fn upsert_then_get_roundtrips() {
    let repo: Arc<dyn ServiceRepo> = Arc::new(InMemoryServiceRepo::default());
    let svc = sample_service();
    repo.put_service(svc.clone()).unwrap();
    let fetched = repo.get_service(svc.id).unwrap().unwrap();
    assert_eq!(fetched.name, "web");
}

#[test]
fn get_missing_is_none() {
    let repo: Arc<dyn ServiceRepo> = Arc::new(InMemoryServiceRepo::default());
    assert!(repo.get_service(Uuid::now_v7()).unwrap().is_none());
}
```

- [ ] **Step 13.3: Run the test — expect compile failure (`InMemoryServiceRepo` doesn't exist)**

```bash
cargo test --features test-support --test repo -- services
```

Expected: `error[E0432]: unresolved import denia::repo::mock::InMemoryServiceRepo`.

- [ ] **Step 13.4: Implement `InMemoryServiceRepo`**

```rust
// src/repo/mock.rs

#![cfg(any(test, feature = "test-support"))]

use std::collections::HashMap;
use std::sync::Mutex;
use uuid::Uuid;

use crate::domain::ServiceConfig;
use crate::repo::error::RepoError;
use crate::repo::service_repo::ServiceRepo;

#[derive(Default)]
pub struct InMemoryServiceRepo {
    inner: Mutex<HashMap<Uuid, ServiceConfig>>,
}

impl ServiceRepo for InMemoryServiceRepo {
    fn put_service(&self, config: ServiceConfig) -> Result<ServiceConfig, RepoError> {
        let mut g = self.inner.lock().map_err(|_| RepoError::LockPoisoned)?;
        g.insert(config.id, config.clone());
        Ok(config)
    }
    fn list_services(&self) -> Result<Vec<ServiceConfig>, RepoError> {
        let g = self.inner.lock().map_err(|_| RepoError::LockPoisoned)?;
        Ok(g.values().cloned().collect())
    }
    fn get_service(&self, service_id: Uuid) -> Result<Option<ServiceConfig>, RepoError> {
        let g = self.inner.lock().map_err(|_| RepoError::LockPoisoned)?;
        Ok(g.get(&service_id).cloned())
    }
}
```

Also add `pub mod mock;` to `src/repo/mod.rs` with the `#[cfg(...)]` gate.

- [ ] **Step 13.5: Run test, expect PASS**

```bash
cargo test --features test-support --test repo -- services
```

Expected: 2 tests pass.

- [ ] **Step 13.6: Repeat steps 13.2–13.5 for each other repo trait**

Each gets:
- `InMemory*Repo` with `Mutex<HashMap<Uuid, T>>` (or appropriate keyed map)
- 2–3 contract tests (happy path, missing → None/NotFound, conflict if applicable)

Keep mocks minimal — they exist for handler tests, not as a second prod backend.

- [ ] **Step 13.7: Write the first handler oneshot test**

```rust
// tests/api/services.rs

use std::sync::Arc;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;

use denia::app::{AppState, build_router};
use denia::repo::mock::{
    InMemoryServiceRepo, InMemoryProjectRepo, InMemoryUserRepo, InMemoryDeploymentRepo,
    InMemoryJobRepo, InMemoryTokenRepo, InMemoryCredentialRepo,
};
use denia::runtime::FakeRuntime;

fn test_state() -> AppState {
    AppState::builder()
        .config(/* test AppConfig */)
        .services(Arc::new(InMemoryServiceRepo::default()))
        .projects(Arc::new(InMemoryProjectRepo::default()))
        .users(Arc::new(InMemoryUserRepo::default()))
        .deployments(Arc::new(InMemoryDeploymentRepo::default()))
        .jobs(Arc::new(InMemoryJobRepo::default()))
        .tokens(Arc::new(InMemoryTokenRepo::default()))
        .credentials(Arc::new(InMemoryCredentialRepo::default()))
        .runtime(Arc::new(FakeRuntime::default()))
        // ... other required fields
        .build()
}

#[tokio::test]
async fn list_services_empty_returns_200() {
    let app = build_router(test_state());
    let resp = app.oneshot(
        Request::builder()
            .uri("/v1/projects/00000000-0000-0000-0000-000000000000/services")
            .header("Authorization", "Bearer test-admin-token")
            .body(Body::empty())
            .unwrap()
    ).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}
```

Auth token wiring may need a test admin token in `AppConfig` — replicate whatever the existing integration tests do.

- [ ] **Step 13.8: Run handler test, fix until green**

```bash
cargo test --features test-support --test api
```

- [ ] **Step 13.9: Add at least three handler tests per resource**

Pattern per resource:
- happy path
- auth-denied path (no bearer → 401, viewer-only on admin route → 403)
- not-found path (unknown id → 404)

Prioritize: `services`, `projects`, `auth`, `deployments`. Others as time allows. **YAGNI**: do not test every handler exhaustively. The contract tests + integration tests already cover most behavior.

- [ ] **Step 13.10: Verify and commit**

```bash
cargo test --features test-support
cargo build && cargo test && cargo fmt --all && cargo clippy --all-targets -- -D warnings
git add tests src/repo/mock.rs src/repo/mod.rs Cargo.toml
git commit -m "test(repo): add in-memory repo mocks + contract tests + handler oneshot tests"
```

---

## Task 14: Cleanup — dead re-export shims + final clippy gate + privileged tests

**Files:**
- Modify: `src/lib.rs` — remove any temporary top-level re-exports added in tasks 3 and 4 if no consumers exist

- [ ] **Step 14.1: Grep for callers of any temporary top-level re-export**

```bash
grep -rn "crate::\(metrics\|node_metrics\|access_log\|logs\|traefik\|bridge\|socket_proxy\)" src/ tests/
```

For each match outside the new folder-modules, decide: rewrite the call site to `crate::observability::metrics::X` (preferred — explicit) or keep the re-export (faster). **Preference: rewrite call sites**. The re-export shims were transitional only.

- [ ] **Step 14.2: Delete dead re-exports**

If step 14.1 shows zero outside callers, remove the lines from `src/lib.rs`. Otherwise keep them.

- [ ] **Step 14.3: Final clippy gate**

```bash
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
```

If any clippy lint fires, fix in this task. Allowed lints (via `#[allow(...)]` at item level) must have a one-line justification comment.

- [ ] **Step 14.4: Privileged runtime tests**

```bash
DENIA_RUN_PRIVILEGED_TESTS=1 cargo test --test linux_runtime_privileged -- --ignored
```

If skipped (no root / no env), note in commit body. Otherwise must pass.

- [ ] **Step 14.5: Manual smoke**

```bash
cargo run &
PID=$!
sleep 2
# Basic health
curl -s -o /dev/null -w "healthz: %{http_code}\n" http://127.0.0.1:7180/healthz
# Auth me (with whatever admin token init produces)
curl -s -H "Authorization: Bearer $DENIA_ADMIN_TOKEN" http://127.0.0.1:7180/v1/auth/me | head -c 200
# Projects list
curl -s -H "Authorization: Bearer $DENIA_ADMIN_TOKEN" http://127.0.0.1:7180/v1/projects | head -c 200
# SPA root
curl -s -o /dev/null -w "spa /: %{http_code}\n" http://127.0.0.1:7180/
kill $PID
```

Expected: 200/200/200/200. SPA serves index.html.

- [ ] **Step 14.6: Commit**

```bash
git add -A
git commit -m "refactor(src): final cleanup — drop transitional re-exports, clippy clean"
```

- [ ] **Step 14.7: Update GitNexus index**

```bash
npx gitnexus analyze
```

(Or run with `--embeddings` if the prior index had any — check `.gitnexus/meta.json`.)

---

## Out of Scope (Do Not Implement in This Plan)

- SOPS backend trait extraction.
- `BridgeAllocator` trait promotion (it may already be a trait — leave it as-is).
- Scheduler refactor.
- Multi-node control plane.
- API surface changes (paths, bodies, headers, status codes — all byte-stable).
- DB schema migration.
- Frontend changes.
- New runtime behavior. ADR-004 (SPA embed) and ADR-005 (runtime hardening) untouched.

If a task reveals an unrelated bug, **file it separately** and continue. The refactor must be behavior-preserving end to end.

---

## Rollback Strategy

Each task is one commit. To revert:

- `git revert <sha>` for tasks 1–7 (low-risk leaf moves, additive skeleton).
- Tasks 8–10 are paired: if task 10 fails review, `git revert` task 10's commit alone — tasks 7–9 are still useful (traits + impls exist, just not wired into `AppState`). Re-attempt task 10 with a tighter scope.
- Tasks 11–14 each revert cleanly.

---

## Reference Skills

- @superpowers:executing-plans — for batch execution with checkpoints.
- @superpowers:subagent-driven-development — for fresh-subagent-per-task execution.
- @superpowers:systematic-debugging — if any step's verify gate fails unexpectedly.
