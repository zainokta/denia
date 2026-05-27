# Concrete Repositories: Drop Repo Traits Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the nine `Arc<dyn ...Repo>` data-access abstractions with their concrete `Sqlite*Repo` types, deleting the repo traits and in-memory repo mocks, while keeping the genuine behavioral seams (`Runtime`, `HealthChecker`, `CommandRunner`, `BridgeManager`, `DomainVerifier`) as `Arc<dyn>`.

**Architecture:** The repo traits exist only to inject in-memory mocks into tests. Integration tests (`tests/*.rs`) already build `AppState` against a real in-memory SQLite store; only three `#[cfg(test)]` handler modules use the mock path, via `AppState::builder(cfg).build()`. We convert each `Sqlite*Repo`'s `impl XRepo for SqliteXRepo` into an inherent `impl SqliteXRepo` (bodies unchanged), make `AppState`/`DeploymentRepos` hold the concrete types (cheap to `Clone` because `SqlitePool` is `Arc`-backed), **repurpose `AppStateBuilder`** so `build()` constructs the concrete repos from an in-memory migrated store (preserving the `AppState::builder(cfg).build()` call sites), fix the one non-handler consumer (`auth::resolve_auth`), then delete the trait files and the now-unused repo mocks. Infra traits whose production impls have unfakeable side effects (spawning Linux namespaces, real HTTP, loopback sockets) stay abstract.

**Tech Stack:** Rust 2024, axum, rusqlite (`SqlitePool` = `Arc<Mutex<Connection>>`), uuid v7.

---

## Scope

**In scope — concretize these 9 repos** (`AppState` fields + `DeploymentRepos` bundle):
`ServiceRepo`, `DomainRepo`, `RegistryRepo`, `ProjectRepo`, `UserRepo`, `DeploymentRepo`, `JobRepo`, `TokenRepo`, `CredentialRepo`.

**Out of scope — keep as `Arc<dyn>` (genuine abstractions, side-effecting prod impls):**
`Runtime`, `HealthChecker`, `CommandRunner`, `BridgeManager`, `DomainVerifier`. Their fakes (`FakeRuntime`, `FakeHealthChecker`, `FakeBridgeManager`, `StubDomainVerifier`) are real test seams with no cheap real alternative; keep the traits and `AppState::with_domain_verifier`.

**Why this split:** A trait whose only second implementation is a test mock — and whose real implementation is cheap to run in tests (in-memory SQLite) — is not earning its abstraction. The infra traits avoid unfakeable side effects, so they stay.

## Complete Breaking-Site Inventory

Verified via full scan — these are every site that references a repo trait or relies on trait-object behavior:

- **Trait definitions:** `src/repo/{service,domain,registry,project,user,deployment,job,token,credential}_repo.rs` (9 files).
- **Trait impls (concrete):** `src/repo/sqlite/{services,domains,registries,projects,users,deployments,jobs,tokens,credentials}.rs` — `impl XRepo for SqliteXRepo`.
- **Trait impls (mocks):** `src/repo/mock.rs` — 9 `InMemory*Repo` + impls (plus `StubDomainVerifier`, which is kept).
- **Re-exports:** `src/repo/mod.rs`.
- **`AppState` fields + constructors + `AppStateBuilder`:** `src/app.rs` (fields `:39-47`, builder Option fields `:191-199`, setters `:223-258`, `build()` mock defaults `:286-314`).
- **`DeploymentRepos` bundle:** `src/deploy/coordinator.rs:34-37`.
- **Trait-object consumer:** `src/auth/middleware.rs:10` (import), `:29-30` (`state.users.as_ref()` / `state.tokens.as_ref()`), `:43-44` (`resolve_auth(users: &dyn UserRepo, tokens: &dyn TokenRepo, ...)`). `resolve_auth` is re-exported at `src/auth/mod.rs`.
- **Builder call sites (handler `#[cfg(test)]` modules):** `src/api/services.rs:210`, `src/api/projects.rs:82`, `src/api/domains.rs:206` — each `AppState::builder(AppConfig::for_test(...)).build()`.
- **Trait imports in tests:** `tests/repo_contract.rs:19-29`.

**Confirmed NOT affected:** production handler bodies in `src/api/*` (call repos via `state.<field>.<method>()`, resolve identically to inherent methods), `src/state.rs` (`SqliteStore` facade), `src/main.rs`. No generic `T: XRepo` bounds exist anywhere.

## Safety Net & Known Risk

Behavior-preserving refactor; the existing suite is the regression guard (`tests/backend_contract.rs` exercises the full router, `tests/repo_contract.rs` covers repo SQL, `tests/domain_verification.rs` covers the kept `DomainVerifier` seam).

**RISK — handler unit tests switch from mocks to real SQLite:** `src/repo/mock.rs` documents that the in-memory mocks "do not reproduce every SQL constraint." The three `#[cfg(test)]` handler modules currently run against those loose mocks; after the builder is repurposed they run against a real migrated SQLite store. Tests that relied on mock leniency (missing FK/unique enforcement, no migration-seeded default project) may now fail. Each such failure is a test that was previously passing under unrealistic conditions — fix the test setup (seed required rows via the concrete repos), do not weaken assertions. Budget for this in Task 1 Step 8.

---

### Task 1: Concretize repos, repurpose builder, fix middleware — one green commit

This is one coupled change (removing a trait impl while `AppState` still holds `Arc<dyn>`, or repurposing the builder while mocks are referenced, breaks the build mid-way), so all source edits land together. The repo mocks become dead after this task and are deleted in Task 2.

**Files:**
- Modify: `src/repo/sqlite/{services,domains,registries,projects,users,deployments,jobs,tokens,credentials}.rs`
- Modify: `src/app.rs`
- Modify: `src/deploy/coordinator.rs`
- Modify: `src/auth/middleware.rs`
- Modify: `tests/repo_contract.rs`

- [ ] **Step 1: Impact analysis (project requirement)**

Run: `gitnexus_impact({target: "AppState", direction: "upstream"})`; report HIGH/CRITICAL to the user before editing.

- [ ] **Step 2: Convert each `Sqlite*Repo` to inherent methods + `#[derive(Clone)]`**

For every file in `src/repo/sqlite/{...}.rs`:
- Change `impl <X>Repo for Sqlite<X>Repo {` → `impl Sqlite<X>Repo {` (method bodies unchanged).
- Remove the now-unused `use crate::repo::<x>_repo::<X>Repo;`.
- Add `#[derive(Clone)]` above `pub struct Sqlite<X>Repo {`.

Example (`services.rs`):

```rust
// remove: use crate::repo::service_repo::ServiceRepo;

#[derive(Clone)]
pub struct SqliteServiceRepo {
    pool: SqlitePool,
}

impl SqliteServiceRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
    pub fn put_service(&self, config: ServiceConfig) -> Result<ServiceConfig, RepoError> {
        let conn = self.pool.connection()?;
        put_service_q(&conn, &config)?;
        Ok(config)
    }
    // list_services / get_service unchanged, now inherent
}
```

Leave the `impl SqliteStore { ... }` facade blocks in these files exactly as-is.

- [ ] **Step 3: `AppState` holds concrete repos**

In `src/app.rs`:
- In the `use crate::{ ... }` block, drop the 9 repo-trait imports (`CredentialRepo, DeploymentRepo, ...`); keep the `sqlite::{ Sqlite* }` imports.
- Change each `AppState` repo field from `Arc<dyn XRepo>` to `SqliteXRepo` (e.g. `pub services: SqliteServiceRepo,`).
- In `new_with_deploy_dependencies_and_log`, drop the `Arc::new(...)` wrapper on each repo: `services: SqliteServiceRepo::new(pool.clone()),` etc. Keep `Arc::new(...)` for `runtime`, `health`, `command_runner`, `bridge_manager`, `domain_verifier`.

- [ ] **Step 4: Repurpose `AppStateBuilder` to build concrete repos from an in-memory store**

In `src/app.rs`, keep the `AppState::builder(config)` / `.build()` API (3 call sites depend on it) but rebuild its internals:
- Delete the 9 repo Option fields (`:191-199`) and their 9 setter methods (`:223-258`). Keep the `runtime` and `domain_verifier` Option fields + setters.
- Rewrite `build()` to open an in-memory store, migrate, and construct concrete repos from its pool + fake infra:

```rust
pub fn build(self) -> AppState {
    let store = crate::state::SqliteStore::open_in_memory().expect("open in-memory store");
    store.migrate().expect("run migrations");
    let pool = store.pool();
    let bridge_start_port = self.config.bridge_start_port;
    let ingress_options = IngressRenderOptions {
        acme_resolver: self.config.acme_resolver.clone(),
        control_domain: self.config.control_domain.clone(),
        control_tls: self.config.control_tls,
        control_backend_addr: format!("http://{}", self.config.bind_addr),
    };
    AppState {
        config: self.config,
        services: SqliteServiceRepo::new(pool.clone()),
        domains: SqliteDomainRepo::new(pool.clone()),
        registries: SqliteRegistryRepo::new(pool.clone()),
        projects: SqliteProjectRepo::new(pool.clone()),
        users: SqliteUserRepo::new(pool.clone()),
        deployments: SqliteDeploymentRepo::new(pool.clone()),
        jobs: SqliteJobRepo::new(pool.clone()),
        tokens: SqliteTokenRepo::new(pool.clone()),
        credentials: SqliteCredentialRepo::new(pool),
        runtime: self
            .runtime
            .unwrap_or_else(|| Arc::new(crate::runtime::FakeRuntime::default())),
        health: Arc::new(FakeHealthChecker::healthy()),
        command_runner: Arc::new(TokioCommandRunner),
        bridge_allocator: Arc::new(Mutex::new(BridgeAllocator::new(bridge_start_port))),
        bridge_manager: Arc::new(crate::bridge::FakeBridgeManager::default()),
        routes: Arc::new(Mutex::new(BTreeMap::new())),
        ingress_options,
        access_log: AccessLogStore::new(),
        domain_verifier: self
            .domain_verifier
            .unwrap_or_else(|| Arc::new(crate::verification::HttpDomainVerifier::new())),
        verifying_domains: Arc::new(Mutex::new(std::collections::HashSet::new())),
    }
}
```

Remove the `use crate::repo::mock::{ InMemory* };` import inside `build()`. The in-memory `SqliteStore` connection lives as long as the cloned `SqlitePool` (Arc-shared), so dropping `store` at end of `build()` is safe.

- [ ] **Step 5: Make `DeploymentRepos` concrete**

In `src/deploy/coordinator.rs`:
- Replace the repo-trait imports with `use crate::repo::sqlite::{SqliteDeploymentRepo, SqliteProjectRepo, SqliteRegistryRepo, SqliteDomainRepo};`.
- Concrete fields:

```rust
#[derive(Clone)]
pub struct DeploymentRepos {
    pub deployments: SqliteDeploymentRepo,
    pub projects: SqliteProjectRepo,
    pub registries: SqliteRegistryRepo,
    pub domains: SqliteDomainRepo,
}
```

`DeploymentCoordinator<R, H>` and its constructors are unchanged (take `repos: DeploymentRepos` by value; `.clone()` works because each `SqliteXRepo` is now `Clone`). `AppState::deployment_repos()` needs no change beyond compiling against concrete fields.

- [ ] **Step 6: Fix `auth::resolve_auth` (only non-handler trait-object consumer)**

In `src/auth/middleware.rs`:
- Replace `use crate::repo::{TokenRepo, UserRepo};` with `use crate::repo::sqlite::{SqliteTokenRepo, SqliteUserRepo};`.
- Change `resolve_auth` signature params to concrete refs:

```rust
pub fn resolve_auth(
    users: &SqliteUserRepo,
    tokens: &SqliteTokenRepo,
    token: &str,
    admin_token: &str,
) -> Option<Principal> {
```

- At the call site (`:28-30`), drop `.as_ref()`: pass `&state.users, &state.tokens`.

(Method bodies `users.user_for_session(...)` / `tokens.user_for_api_token(...)` resolve to inherent methods — unchanged.)

- [ ] **Step 7: Clean trait imports in the contract test**

In `tests/repo_contract.rs`, remove the trait `use` lines (`deployment_repo::DeploymentRepo`, `domain_repo::DomainRepo`, `job_repo::JobRepo`, `project_repo::ProjectRepo`, `service_repo::ServiceRepo`, `token_repo::TokenRepo`, `user_repo::UserRepo`). Keep the `sqlite::{ Sqlite* }` import and all test bodies.

- [ ] **Step 8: Build + test (fix any mock-leniency fallout)**

Run: `cargo build` — Expected: PASS. (`InMemory*Repo` structs are now dead code but still compile under `cfg(test)`; deleted in Task 2.)
Run: `cargo test` — Expected: PASS. If any of the 3 handler test modules fail, it is the known SQL-constraint risk: seed required rows (e.g. the migration-seeded default project, FK parents) via the concrete repos in that module's `test_state()`/helpers. Fix setup, not assertions.

- [ ] **Step 9: Commit**

```bash
git add src/repo/sqlite src/app.rs src/deploy/coordinator.rs src/auth/middleware.rs tests/repo_contract.rs
git commit -m "refactor(repo): hold concrete Sqlite*Repo in AppState instead of Arc<dyn>"
```

---

### Task 2: Delete repo traits, repo mocks, and trait re-exports

Nothing implements or names the repo traits now; remove the dead abstraction.

**Files:**
- Delete: `src/repo/{service,domain,registry,project,user,deployment,job,token,credential}_repo.rs`
- Modify: `src/repo/mod.rs`, `src/repo/mock.rs`

- [ ] **Step 1: Delete the 9 trait files**

```bash
git rm src/repo/service_repo.rs src/repo/domain_repo.rs src/repo/registry_repo.rs \
       src/repo/project_repo.rs src/repo/user_repo.rs src/repo/deployment_repo.rs \
       src/repo/job_repo.rs src/repo/token_repo.rs src/repo/credential_repo.rs
```

- [ ] **Step 2: Trim `src/repo/mod.rs`**

Remove the 9 `pub mod *_repo;` declarations and the 9 `pub use *_repo::*Repo;` re-exports. Keep `pub mod error;`, `pub mod sqlite;`, the `mock` module gate, and `pub use error::RepoError;`. Update the module doc comment to state repos are concrete (`Sqlite*Repo` in `sqlite/`).

- [ ] **Step 3: Trim `src/repo/mock.rs` to just `StubDomainVerifier`**

Delete the 9 `InMemory*Repo` structs + impls. Keep `StubDomainVerifier` + its `impl DomainVerifier` and the imports it needs (`DomainVerifier`, `DomainVerifyError`). Prune the now-unused imports (repo traits, `lock`, domain types only the deleted mocks used). File stays `#![cfg(any(test, feature = "test-support"))]`.

> A `DomainVerifier` stub under `repo/` is slightly off-topic, but relocating to `src/verification/` would churn its import path; keep in place — relocation is a separate cleanup.

- [ ] **Step 4: Build + test**

Run: `cargo build` — Expected: PASS (errors mean a deleted symbol is still referenced; grep + remove).
Run: `cargo test` — Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/repo
git commit -m "refactor(repo): delete repo traits and in-memory repo mocks"
```

---

### Task 3: Verify, lint, format

- [ ] **Step 1: Confirm no orphaned repo-trait references**

Check each name (`ServiceRepo` … `CredentialRepo`) across `src/` and `tests/`. Expected: zero hits outside `docs/`. In particular `src/auth/middleware.rs` must no longer match.

- [ ] **Step 2: Format** — `cargo fmt --all`

- [ ] **Step 3: Lint** — `cargo clippy --all-targets --all-features`. Expected: no new warnings. `dead_code` on `StubDomainVerifier` (cfg-gated) is acceptable.

- [ ] **Step 4: Full test pass** — `cargo test` — Expected: PASS.

- [ ] **Step 5: Detect changes scope (project requirement)**

Run: `gitnexus_detect_changes({scope: "all"})`. Expected: changes confined to `src/repo/**`, `src/app.rs`, `src/deploy/coordinator.rs`, `src/auth/middleware.rs`, `tests/repo_contract.rs`. Investigate anything outside that set.

- [ ] **Step 6: Commit fixups**

```bash
git add -A
git commit -m "refactor(repo): fmt + clippy after concrete-repo migration"
```

---

## Notes for the Implementer

- **`Arc` stays for infra:** Do not touch `runtime`, `health`, `command_runner`, `bridge_manager`, `domain_verifier`, `bridge_allocator`, or `routes` — they remain `Arc<...>`.
- **No repo behavior change:** every repo method body is copied verbatim; only the `impl ... for` header becomes inherent `impl`.
- **`Clone` is cheap:** `SqlitePool { inner: Arc<Mutex<Connection>> }` is `Clone`, so cloning a `SqliteXRepo` clones an `Arc`, not a connection.
- **`SqliteStore` facade untouched:** the `impl SqliteStore { ... }` blocks in `src/repo/sqlite/*.rs` and `src/state.rs` are independent; leave them.
- **Builder is now a thin in-memory test factory:** it no longer injects repos; tests needing seeded data must seed through the concrete repos on the returned `AppState`.
