# Design: src/ Modularization and Per-Aggregate Repositories

- **Date**: 2026-05-25
- **ADR**: [012-src-modularization](../../adr/012-src-modularization.md)
- **Status**: Approved by user, ready for implementation plan

## Goal

Refactor the Denia Rust backend (`src/`) into folder-modules with one concern per file, and replace the single-struct persistence layer with per-aggregate repository traits. SRP, ISP, DIP, and OCP wins without changing the API surface, DB schema, or any external contract.

## Non-Goals

- No API surface changes — every `/v1/*` route, request body, and response body byte-identical before and after.
- No DB schema migration.
- No SOPS backend abstraction.
- No `BridgeAllocator` trait promotion.
- No scheduler refactor.
- No multi-node control plane.
- No frontend changes.
- No new runtime behavior; ADR-004 (SPA embed) and ADR-005 (runtime hardening) untouched.

## Target `src/` Layout

```
src/
  main.rs                    // unchanged (binary entry)
  lib.rs                     // pub mod declarations only
  app.rs                     // AppState + AppStateBuilder + build_router (thin assembler, ~150 lines)
  config.rs                  // unchanged
  command.rs                 // unchanged (CommandRunner trait + TokioCommandRunner)
  health.rs                  // unchanged (HealthChecker trait + FakeHealthChecker)
  cgroup_launcher.rs         // unchanged (small, single concern)
  scheduler.rs               // unchanged for this refactor
  secrets.rs                 // unchanged (flat, single concern)
  web.rs                     // unchanged (ADR-004 SPA embed)
  api/
    mod.rs                   // pub use sub-router fns
    error.rs                 // ApiError + IntoResponse + From<*Error> conversions
    auth.rs                  // /v1/auth/* router + handlers
    services.rs              // /v1/projects/:p/services/* router + handlers
    deployments.rs           // /v1/projects/:p/deployments/*
    workloads.rs             // /v1/projects/:p/workloads/*
    projects.rs              // /v1/projects + /v1/projects/:p
    members.rs               // /v1/projects/:p/members/*
    jobs.rs                  // /v1/projects/:p/jobs/*
    secrets.rs               // /v1/projects/:p/secrets/*
    tokens.rs                // /v1/tokens/*
    observability.rs         // /v1/node, /v1/projects/:p/access-log, /v1/projects/:p/logs/*
    ingress.rs               // /v1/projects/:p/routes/* inspection
    health.rs                // /healthz (or keep in app.rs)
  domain/
    mod.rs                   // pub use service::*; pub use deployment::*; ...
    error.rs                 // DomainError
    service.rs               // ResourceLimits, HealthCheck, ServiceSource, GitSource, ExternalImageSource, ServiceConfig
    deployment.rs            // Deployment, DeploymentRequest, DeploymentStatus, RuntimeStartRequest, RuntimeStatus
    project.rs               // Project, ProjectMembership
    user.rs                  // User, Role, Session, ApiToken, Me, PrincipalView, LoginResult
    credential.rs            // Credential, CredentialKind
    job.rs                   // Job, JobRun, JobRunRequest, JobRunStatus, JobOutcome
  repo/
    mod.rs                   // pub use traits + RepoError; pub mod sqlite; #[cfg(...)] pub mod mock;
    error.rs                 // RepoError
    service_repo.rs          // trait ServiceRepo
    project_repo.rs
    user_repo.rs
    deployment_repo.rs
    job_repo.rs
    token_repo.rs
    credential_repo.rs
    sqlite/
      mod.rs                 // pub use each Sqlite* repo
      pool.rs                // init_pool(path), init_pool_memory(), run_migrations()
      services.rs            // SqliteServiceRepo
      projects.rs
      users.rs
      deployments.rs
      jobs.rs
      tokens.rs
      credentials.rs
    mock.rs                  // #[cfg(any(test, feature = "test-support"))] in-memory impls
  runtime/
    mod.rs                   // pub use trait + impls + plan + error
    error.rs                 // RuntimeError
    runtime_trait.rs         // trait Runtime
    plan.rs                  // LinuxRuntimePlan, LinuxRuntimeProcessSpec, TrackedChild
    validation.rs            // validate_service_name, validate_process_spec, validate_resource_limits, validate_namespace_launcher
    fs_helpers.rs            // create_runtime_directory, remove_*_if_exists, safe_artifact_name, cpu_max, validate_runtime_directory, wait_for_cgroup_ready, terminate_tracked_child, resolve_setpriv
    linux.rs                 // LinuxRuntime + impl Runtime
    fake.rs                  // FakeRuntime
  ingress/
    mod.rs                   // pub use traefik::*; pub use bridge::*; pub use socket_proxy::*;
    traefik.rs               // moved from src/traefik.rs
    bridge.rs                // moved from src/bridge.rs
    socket_proxy.rs          // moved from src/socket_proxy.rs
  observability/
    mod.rs                   // pub use metrics::*; pub use node_metrics::*; pub use access_log::*; pub use logs::*;
    metrics.rs               // CgroupMetricsReader (was src/metrics.rs)
    node_metrics.rs          // (was src/node_metrics.rs)
    access_log.rs            // AccessLogStore (was src/access_log.rs)
    logs.rs                  // LogStore (was src/logs.rs)
  deploy/
    mod.rs                   // pub use coordinator::*; pub use routes::*; pub use error::*;
    error.rs                 // DeployError
    coordinator.rs           // DeploymentCoordinator
    routes.rs                // SharedRoutes
  auth/
    mod.rs                   // pub use principal::*; pub use guards::*; pub use middleware::*;
    principal.rs             // Principal (axum extractor)
    guards.rs                // require_project_role, require_super_admin, ensure_role
    middleware.rs            // resolve_auth (axum middleware fn)
  artifacts/                 // unchanged (already folder-module)
  oci/                       // unchanged (already folder-module)
  syscall/                   // unchanged (already folder-module)
```

**Re-export contract**: every `mod.rs` re-exports the previously-public symbols of its split children. `use crate::domain::ServiceConfig`, `use crate::traefik::RouteSpec`, etc. continue to resolve. Only `app.rs` (and the test files it touches) edit imports.

## Repository Trait Shape

```rust
// src/repo/error.rs
#[derive(Debug, thiserror::Error)]
pub enum RepoError {
    #[error("not found")]
    NotFound,
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("db: {0}")]
    Db(#[from] sqlx::Error),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
}
```

```rust
// src/repo/service_repo.rs
#[async_trait::async_trait]
pub trait ServiceRepo: Send + Sync + 'static {
    async fn get(&self, project_id: &str, name: &str) -> Result<Option<ServiceConfig>, RepoError>;
    async fn list(&self, project_id: &str) -> Result<Vec<ServiceConfig>, RepoError>;
    async fn upsert(&self, project_id: &str, svc: &ServiceConfig) -> Result<(), RepoError>;
    async fn delete(&self, project_id: &str, name: &str) -> Result<(), RepoError>;
}
```

One trait per aggregate. Each trait contains only the methods that aggregate's handlers and coordinators need — ISP holds. Method signatures are derived directly from the current `SqliteStore::*_service`, `*_project`, `*_user`, etc. methods.

Sqlite implementations:

```rust
// src/repo/sqlite/services.rs
pub struct SqliteServiceRepo { pool: SqlitePool }

impl SqliteServiceRepo {
    pub fn new(pool: SqlitePool) -> Self { Self { pool } }
}

#[async_trait::async_trait]
impl ServiceRepo for SqliteServiceRepo { /* SQL bodies lifted verbatim from current state.rs */ }
```

Pool ctor + migrations in `repo/sqlite/pool.rs::init_pool(path) -> Result<SqlitePool, RepoError>` and `init_pool_memory() -> Result<SqlitePool, RepoError>` for tests.

## AppState

```rust
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
    pub command:         Arc<dyn CommandRunner>,
    pub health:          Arc<dyn HealthChecker>,
    pub metrics:         Arc<dyn CgroupMetricsReader>,
    pub node_metrics:    Arc<dyn NodeMetricsReader>,
    pub bridge_alloc:    Arc<dyn BridgeAllocator>,
    pub bridge_supervisor: Arc<dyn LoopbackBridgeSupervisor>,
    pub access_log:      AccessLogStore,
    pub coordinator:     Arc<DeploymentCoordinator>,
}
```

Constructor:

```rust
impl AppState {
    pub fn new(config: AppConfig, pool: SqlitePool) -> Self { /* wires Sqlite* repos + defaults */ }
    pub fn builder() -> AppStateBuilder { ... }
}
```

`AppStateBuilder` exposes a setter per field defaulting to the production wiring. Test code overrides only what it needs.

## API Router Assembly

Each `api/<resource>.rs` exports `pub fn router() -> Router<AppState>` with free-fn handlers. `app.rs::build_router` merges them:

```rust
pub fn build_router(state: AppState) -> Router {
    let v1 = Router::new()
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
        .layer(middleware::from_fn_with_state(state.clone(), resolve_auth));

    Router::new()
        .route("/healthz", get(healthz))
        .nest("/v1", v1)
        .fallback_service(web::spa_service())
        .with_state(state)
}
```

`api/error.rs` owns `ApiError`, `IntoResponse for ApiError`, and `From<RepoError | DomainError | RuntimeError | DeployError>` conversions — single mapping site for all error → HTTP code translation. Adding a new error variant or backend never touches a handler.

Auth guards (`require_project_role`, `require_super_admin`) stay free-function calls inside handler bodies — same pattern as today.

## Migration Order (14 Steps)

Each step ends with `cargo build && cargo test && cargo fmt --all && cargo clippy --all-targets -- -D warnings` green. One step = one commit.

| # | Step | Risk | Notes |
|---|------|------|-------|
| 1 | `domain.rs` → `domain/` folder, full `pub use` re-exports | Low | Pure move + re-exports |
| 2 | `runtime.rs` → `runtime/` folder | Med | Helper visibility audit; run privileged tests opt-in |
| 3 | `metrics`, `node_metrics`, `access_log`, `logs` → `observability/` | Low | Move + re-export from `lib.rs` |
| 4 | `traefik`, `bridge`, `socket_proxy` → `ingress/` | Low | Move + re-export |
| 5 | `deploy.rs` → `deploy/` folder | Low | Split coordinator/routes/error |
| 6 | `auth.rs` → `auth/` folder | Low | Split principal/guards/middleware |
| 7 | New `repo/` skeleton — traits + `RepoError` + `pool.rs` ctor only, no impls | None | Additive |
| 8 | `state.rs` `SqliteStore` impl split into `repo/sqlite/{aggregate}.rs` modules — still single struct exposing the same methods | Med | Pure file split, behavior unchanged |
| 9 | Add per-aggregate `Sqlite*Repo` structs implementing each trait. Keep `SqliteStore` as a thin adapter that forwards `as_dyn_service_repo()` etc. for callers not yet migrated | High | Adapter pattern keeps step 10 independent |
| 10 | Rewire `AppState` to hold `Arc<dyn ...Repo>` per aggregate + `AppStateBuilder`. Update every `app.rs` handler call site | High | Single commit. Delete adapter from step 9. |
| 11 | Extract handlers from `app.rs` into `api/<resource>.rs` modules. Each exports `pub fn router() -> Router<AppState>`. `app.rs::build_router` merges them | Med | Mechanical move; verify with handler oneshot tests |
| 12 | `api/error.rs` central `ApiError` + `From<…>` conversions; unify handler return types | Low | Mechanical sweep |
| 13 | Add `repo/mock.rs` in-memory impls (cfg(test) or feature = "test-support") + handler oneshot tests + repo contract tests | None | Additive tests only |
| 14 | Remove any leftover re-export shims that are now dead; run final `cargo clippy -D warnings`; run privileged runtime tests | Low | Cleanup pass |

**Steps 9 and 10 are the risk window.** Mitigated by:
- Step 9 introduces the new traits alongside `SqliteStore` via an adapter — repo trait callers compile against either.
- Step 10 deletes the adapter and finalizes `AppState`. If step 10 reveals integration issues, revert step 10 only; step 9's traits remain available.

## Testing Strategy

**6.1 Repo contract tests** — `tests/repo/<aggregate>.rs`. Shared scenarios run against `SqliteServiceRepo` (and each other) backed by `:memory:` SQLite. Assert: get/list/upsert/delete behavior, `NotFound` on missing keys, `Conflict` on unique violations, JSON round-trip for embedded fields.

**6.2 Handler oneshot tests** — `tests/api/<resource>.rs`. Use `axum::Router::oneshot` via `tower::ServiceExt`. Build `AppState` via `AppStateBuilder` injecting `InMemoryServiceRepo` etc. + `FakeRuntime`. Coverage per handler:

- One happy path
- One auth-denied path
- One not-found path
- Cross-repo handlers (e.g. `create_deployment` reads service + writes deployment) get an extra two-repo assertion.

**6.3 Existing integration tests** unchanged. Privileged runtime tests stay opt-in via `DENIA_RUN_PRIVILEGED_TESTS=1`. ADR-005 hardening tests unaffected.

**6.4 Mocks** — `#[cfg(any(test, feature = "test-support"))]` gates a `repo/mock.rs` module containing `InMemoryServiceRepo { Mutex<HashMap<(String,String), ServiceConfig>> }` etc. Plain Rust, no proc-macro mocking framework. Mock impls never ship in release.

**6.5 What we do not test** — folder structure, axum routing internals, framework code, file paths.

**6.6 CI gate** — every commit: `cargo build && cargo test && cargo fmt --all && cargo clippy --all-targets -- -D warnings`. Steps 2 and 14 additionally run privileged runtime tests.

## Risks and Mitigations

| Risk | Mitigation |
|------|------------|
| Step 10 `AppState` rewire breaks many handlers at once | Step 9 adapter lets traits compile before handlers migrate; step 11 also depends on step 10 being green |
| Re-export shims rot | One-time `cargo public-api` (optional) snapshot at end of step 14, or audit each `mod.rs` |
| Hidden coupling between `state.rs` modules (shared private helpers) | Step 8 keeps single struct; private helpers stay private in `repo/sqlite/mod.rs` until step 9 separates structs |
| `Arc<dyn>` vtable indirection regression | Bench is unnecessary — HTTP request cost dwarfs vtable dispatch. Spot-check with `cargo build --release` runs identically. |
| Privileged runtime tests pass before but fail after `runtime.rs` split | Step 2 ends with `DENIA_RUN_PRIVILEGED_TESTS=1 cargo test --test linux_runtime_privileged -- --ignored` |
| ADR-004 SPA embed fallback regression | `app.rs::build_router` keeps `.fallback_service(web::spa_service())` invocation byte-identical |
| Traefik file-provider contract regression | `ingress/traefik.rs` is a pure move — confirm with a manual `curl /v1/projects/.../routes` smoke after step 11 |

## Verification Plan

Per step:
```
cargo build
cargo test
cargo fmt --all
cargo clippy --all-targets -- -D warnings
```

End of step 2 and step 14:
```
DENIA_RUN_PRIVILEGED_TESTS=1 cargo test --test linux_runtime_privileged -- --ignored
```

Final manual smoke (after step 14):
- `curl /healthz` → 200
- `curl /v1/projects` with admin token → identical payload to pre-refactor snapshot
- Browse to embedded SPA → loads
- Create a service via API, deploy, check `/v1/projects/:p/workloads` → status reports
- Tail access log → entries appear

## File-by-File Mapping

| Before | After |
|--------|-------|
| `src/domain.rs` | `src/domain/{mod,error,service,deployment,project,user,credential,job}.rs` |
| `src/runtime.rs` | `src/runtime/{mod,error,runtime_trait,plan,validation,fs_helpers,linux,fake}.rs` |
| `src/state.rs` | `src/repo/{error,service_repo,project_repo,user_repo,deployment_repo,job_repo,token_repo,credential_repo}.rs` + `src/repo/sqlite/{mod,pool,services,projects,users,deployments,jobs,tokens,credentials}.rs` |
| `src/app.rs` (handlers) | `src/api/{auth,services,deployments,workloads,projects,members,jobs,secrets,tokens,observability,ingress,health,error}.rs` |
| `src/app.rs` (AppState, build_router) | `src/app.rs` (shrunk) |
| `src/auth.rs` | `src/auth/{mod,principal,guards,middleware}.rs` |
| `src/deploy.rs` | `src/deploy/{mod,error,coordinator,routes}.rs` |
| `src/traefik.rs`, `src/bridge.rs`, `src/socket_proxy.rs` | `src/ingress/{mod,traefik,bridge,socket_proxy}.rs` |
| `src/metrics.rs`, `src/node_metrics.rs`, `src/access_log.rs`, `src/logs.rs` | `src/observability/{mod,metrics,node_metrics,access_log,logs}.rs` |
| `src/main.rs`, `src/lib.rs`, `src/config.rs`, `src/command.rs`, `src/health.rs`, `src/cgroup_launcher.rs`, `src/scheduler.rs`, `src/secrets.rs`, `src/web.rs` | unchanged (or `lib.rs` mod declarations updated) |
| `src/artifacts/`, `src/oci/`, `src/syscall/` | unchanged |

## References

- [ADR-012 src/ Modularization](../../adr/012-src-modularization.md)
- ADR-001 Initial Backend Architecture
- ADR-003 Linux Runtime Process Runner
- ADR-004 Embed Web Console
- ADR-005 Runtime Security Hardening
