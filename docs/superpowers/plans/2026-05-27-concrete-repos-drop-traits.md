# Concrete Repositories: Drop Repo Traits Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the nine `Arc<dyn ...Repo>` data-access abstractions with their concrete `Sqlite*Repo` types, deleting the repo traits and in-memory repo mocks, while keeping the genuine behavioral seams (`Runtime`, `HealthChecker`, `CommandRunner`, `BridgeManager`, `DomainVerifier`) as `Arc<dyn>`.

**Architecture:** The repo traits exist only to inject in-memory mocks into tests, but the live test suite already builds `AppState` against a real in-memory SQLite store (`SqliteStore::open_in_memory`) — the mocks and `AppStateBuilder` are dead weight. We convert each `Sqlite*Repo`'s `impl XRepo for SqliteXRepo` into an inherent `impl SqliteXRepo` (bodies unchanged), make `AppState`/`DeploymentRepos` hold the concrete types (cheap to `Clone` because `SqlitePool` is `Arc`-backed), then delete the trait files, repo mocks, and unused builder. Infra traits whose production impls have unfakeable side effects (spawning Linux namespaces, real HTTP, loopback sockets) stay abstract.

**Tech Stack:** Rust 2024, axum, rusqlite (`SqlitePool` = `Arc<Mutex<Connection>>`), uuid v7.

---

## Scope

**In scope — concretize these 9 repos** (`AppState` fields + `DeploymentRepos` bundle):
`ServiceRepo`, `DomainRepo`, `RegistryRepo`, `ProjectRepo`, `UserRepo`, `DeploymentRepo`, `JobRepo`, `TokenRepo`, `CredentialRepo`.

**Out of scope — keep as `Arc<dyn>` (genuine abstractions, side-effecting prod impls):**
`Runtime`, `HealthChecker`, `CommandRunner`, `BridgeManager`, `DomainVerifier`. Their fakes (`FakeRuntime`, `FakeHealthChecker`, `FakeBridgeManager`, `StubDomainVerifier`) are real test seams with no cheap real alternative; keep the traits and `AppState::with_domain_verifier`.

**Why this split:** A trait whose only second implementation is a test mock — and whose real implementation is cheap to run in tests (in-memory SQLite) — is not earning its abstraction. The infra traits avoid unfakeable side effects, so they stay.

## File-by-File Surface

- `src/repo/service_repo.rs`, `domain_repo.rs`, `registry_repo.rs`, `project_repo.rs`, `user_repo.rs`, `deployment_repo.rs`, `job_repo.rs`, `token_repo.rs`, `credential_repo.rs` — **DELETE** (9 trait files).
- `src/repo/sqlite/services.rs`, `domains.rs`, `registries.rs`, `projects.rs`, `users.rs`, `deployments.rs`, `jobs.rs`, `tokens.rs`, `credentials.rs` — **MODIFY**: `impl XRepo for SqliteXRepo` → inherent `impl SqliteXRepo`; drop `use crate::repo::x_repo::XRepo;`; add `#[derive(Clone)]` to each `SqliteXRepo` struct.
- `src/repo/mod.rs` — **MODIFY**: remove the 9 `pub mod *_repo;` lines and their `pub use` re-exports. Keep `error`, `sqlite`, and `mock` (trimmed).
- `src/repo/mock.rs` — **MODIFY**: delete the 9 `InMemory*Repo` structs + impls. Keep `StubDomainVerifier` (still implements the retained `DomainVerifier` trait) and the `lock`/imports it needs.
- `src/app.rs` — **MODIFY**: `AppState` repo fields `Arc<dyn XRepo>` → `SqliteXRepo`; constructors `Arc::new(SqliteXRepo::new(pool))` → `SqliteXRepo::new(pool)`; delete `AppStateBuilder` + `AppState::builder()` (verify unused first); fix `use` imports; `deployment_repos()` builds concrete bundle. Keep `with_domain_verifier`.
- `src/deploy/coordinator.rs` — **MODIFY**: `DeploymentRepos` four fields `Arc<dyn ...>` → concrete `SqliteXRepo`; update `use` imports.
- `tests/repo_contract.rs` — **MODIFY**: drop `use denia::repo::*_repo::XRepo;` trait imports (methods are now inherent on the `Sqlite*Repo` structs; test bodies unchanged). Keep the file — its real-SQL coverage is the regression net for this refactor.

**Untouched:** `src/api/*.rs` handlers (no `use crate::repo::` imports; they call methods through `state.<field>`), `src/state.rs` (`SqliteStore` facade unchanged), all infra-trait modules.

## Safety Net

This is a behavior-preserving refactor. The existing suite is the regression guard: `tests/backend_contract.rs` exercises the full router over an in-memory store, `tests/repo_contract.rs` covers repo SQL, `tests/domain_verification.rs` covers the kept `DomainVerifier` seam. Do not write new tests; rely on `cargo test` staying green. Run impact analysis before editing per project `AGENTS.md`.

---

### Task 1: Convert SQLite repos to inherent impls + derive Clone, switch AppState/DeploymentRepos to concrete

This is one coupled change (removing a trait impl while `AppState` still holds `Arc<dyn>` breaks the build), so all edits land in a single green commit. Existing tests are the safety net.

**Files:**
- Modify: `src/repo/sqlite/services.rs`, `domains.rs`, `registries.rs`, `projects.rs`, `users.rs`, `deployments.rs`, `jobs.rs`, `tokens.rs`, `credentials.rs`
- Modify: `src/app.rs`
- Modify: `src/deploy/coordinator.rs`
- Modify: `tests/repo_contract.rs`

- [ ] **Step 1: Run impact analysis (project requirement)**

Run: `gitnexus_impact({target: "AppState", direction: "upstream"})` and note risk. Report HIGH/CRITICAL to the user before editing. Expected: handlers across `src/api/*` depend on `AppState` fields, but only via `state.<repo>.<method>()` calls that resolve identically to inherent methods.

- [ ] **Step 2: Convert each `Sqlite*Repo` to inherent methods + `#[derive(Clone)]`**

For every file in `src/repo/sqlite/{services,domains,registries,projects,users,deployments,jobs,tokens,credentials}.rs`:
- Change `impl <X>Repo for Sqlite<X>Repo {` to `impl Sqlite<X>Repo {` (method bodies unchanged).
- Remove the now-unused `use crate::repo::<x>_repo::<X>Repo;` line.
- Add `#[derive(Clone)]` immediately above `pub struct Sqlite<X>Repo {`.

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

- [ ] **Step 3: Make `AppState` hold concrete repos**

In `src/app.rs`:
- In the `use crate::{ ... }` block, drop the repo-trait imports (`CredentialRepo, DeploymentRepo, DomainRepo, JobRepo, ProjectRepo, RegistryRepo, ServiceRepo, TokenRepo, UserRepo`) and keep the `sqlite::{ Sqlite* }` imports.
- Change each `AppState` repo field type from `Arc<dyn XRepo>` to `SqliteXRepo`, e.g. `pub services: SqliteServiceRepo,`.
- In `new_with_deploy_dependencies_and_log`, drop the `Arc::new(...)` wrapper on each repo: `services: SqliteServiceRepo::new(pool.clone()),` etc. Keep `Arc::new(...)` for `runtime`, `health`, `command_runner`, `bridge_manager`, `domain_verifier`.

- [ ] **Step 4: Make `DeploymentRepos` concrete**

In `src/deploy/coordinator.rs`:
- Update the `use` imports: remove the repo-trait imports, add `use crate::repo::sqlite::{SqliteDeploymentRepo, SqliteProjectRepo, SqliteRegistryRepo, SqliteDomainRepo};`.
- Change the four `DeploymentRepos` fields to concrete types:

```rust
#[derive(Clone)]
pub struct DeploymentRepos {
    pub deployments: SqliteDeploymentRepo,
    pub projects: SqliteProjectRepo,
    pub registries: SqliteRegistryRepo,
    pub domains: SqliteDomainRepo,
}
```

`DeploymentCoordinator<R, H>` and its constructors are unchanged (they take `repos: DeploymentRepos` by value; `.clone()` still works because each `SqliteXRepo` is now `Clone`).

- [ ] **Step 5: Fix `AppState::deployment_repos()` and delete the unused builder**

In `src/app.rs`:
- `deployment_repos(&self)` already builds the bundle via `self.deployments.clone()` etc.; this now clones concrete repos — no code change needed beyond confirming it compiles.
- Verify `AppStateBuilder` / `AppState::builder()` have no callers: `gitnexus_context({name: "AppStateBuilder"})` (or grep). Confirmed unused by the suite (tests build via `AppState::new(...)`). Delete the entire `#[cfg(any(test, feature = "test-support"))] pub struct AppStateBuilder { ... }` block, its `impl AppStateBuilder { ... }`, and the `impl AppState { pub fn builder(...) }` block. Keep `with_domain_verifier`.

- [ ] **Step 6: Clean trait imports in the contract test**

In `tests/repo_contract.rs`, remove the trait `use` lines (`use denia::repo::deployment_repo::DeploymentRepo;`, `domain_repo::DomainRepo`, `job_repo::JobRepo`, `project_repo::ProjectRepo`, `service_repo::ServiceRepo`, `token_repo::TokenRepo`, `user_repo::UserRepo`). Keep the `sqlite::{ Sqlite* }` import and all test bodies — the method calls now resolve to inherent methods.

- [ ] **Step 7: Build**

Run: `cargo build`
Expected: PASS. If errors mention "no method named X", a `Sqlite*Repo` impl block was missed in Step 2 or a stray trait bound remains.

- [ ] **Step 8: Test**

Run: `cargo test`
Expected: PASS (all existing tests green — this is the behavior-preservation proof). Note: trait files and repo mocks still exist at this point; they are deleted in Task 2.

- [ ] **Step 9: Commit**

```bash
git add src/repo/sqlite src/app.rs src/deploy/coordinator.rs tests/repo_contract.rs
git commit -m "refactor(repo): hold concrete Sqlite*Repo in AppState instead of Arc<dyn>"
```

---

### Task 2: Delete repo traits, repo mocks, and trait re-exports

Now that nothing implements or names the repo traits, remove the dead abstraction.

**Files:**
- Delete: `src/repo/{service,domain,registry,project,user,deployment,job,token,credential}_repo.rs`
- Modify: `src/repo/mod.rs`
- Modify: `src/repo/mock.rs`

- [ ] **Step 1: Delete the 9 trait files**

```bash
git rm src/repo/service_repo.rs src/repo/domain_repo.rs src/repo/registry_repo.rs \
       src/repo/project_repo.rs src/repo/user_repo.rs src/repo/deployment_repo.rs \
       src/repo/job_repo.rs src/repo/token_repo.rs src/repo/credential_repo.rs
```

- [ ] **Step 2: Trim `src/repo/mod.rs`**

Remove the 9 `pub mod *_repo;` declarations and the 9 `pub use *_repo::*Repo;` re-exports. Keep `pub mod error;`, `pub mod sqlite;`, the `mock` module gate, and `pub use error::RepoError;`. Update the file's doc comment to reflect that repos are concrete (`Sqlite*Repo` in `sqlite/`) and only `RepoError` and the kept infra fakes remain.

- [ ] **Step 3: Trim `src/repo/mock.rs` to just `StubDomainVerifier`**

Delete the 9 `InMemory*Repo` structs and their trait impls (lines for `InMemoryServiceRepo` through `InMemoryCredentialRepo`). Keep `StubDomainVerifier` + its `impl DomainVerifier`. Prune now-unused imports (drop the repo-trait `use` lines and any domain types only the deleted mocks used; keep `DomainVerifier`, `DomainVerifyError`). The file stays gated `#![cfg(any(test, feature = "test-support"))]`.

> Note: a `DomainVerifier` stub under `repo/` is slightly off-topic, but moving it to `src/verification/` would churn `tests/domain_verification.rs`'s import path. Keep it in place for this change; relocating is a separate cleanup.

- [ ] **Step 4: Build**

Run: `cargo build`
Expected: PASS. Errors here mean a deleted trait or mock is still referenced — grep the named symbol and remove the reference.

- [ ] **Step 5: Test**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/repo
git commit -m "refactor(repo): delete repo traits and in-memory repo mocks"
```

---

### Task 3: Verify, lint, format

**Files:** none (verification only).

- [ ] **Step 1: Confirm no orphaned `Arc<dyn ...Repo>` remain**

Run a symbol check for each repo trait name (`ServiceRepo`, `DomainRepo`, `RegistryRepo`, `ProjectRepo`, `UserRepo`, `DeploymentRepo`, `JobRepo`, `TokenRepo`, `CredentialRepo`).
Expected: zero hits in `src/` and `tests/` (matches only in `docs/`). Any `src/`/`tests/` hit is a missed reference.

- [ ] **Step 2: Format**

Run: `cargo fmt --all`

- [ ] **Step 3: Lint**

Run: `cargo clippy --all-targets --all-features`
Expected: no new warnings. Watch for `dead_code` on the kept `StubDomainVerifier` (only compiled under `test`/`test-support`) — acceptable.

- [ ] **Step 4: Full test pass**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 5: Detect changes scope (project requirement)**

Run: `gitnexus_detect_changes({scope: "all"})`
Expected: changes confined to `src/repo/**`, `src/app.rs`, `src/deploy/coordinator.rs`, `tests/repo_contract.rs`. Investigate anything outside that set.

- [ ] **Step 6: Commit any fmt/clippy fixups**

```bash
git add -A
git commit -m "refactor(repo): fmt + clippy after concrete-repo migration"
```

---

## Notes for the Implementer

- **`Arc` stays for infra:** Do not touch `runtime`, `health`, `command_runner`, `bridge_manager`, `domain_verifier`, `bridge_allocator`, or `routes` fields — they remain `Arc<...>`.
- **No behavior change:** every repo method body is copied verbatim; only the enclosing `impl ... for` header changes to inherent `impl`.
- **`Clone` is cheap:** `SqlitePool { inner: Arc<Mutex<Connection>> }` is `#[derive(Clone)]`, so cloning a `SqliteXRepo` clones an `Arc`, not a connection — `AppState` and `DeploymentRepos` stay cheaply `Clone`.
- **`SqliteStore` facade untouched:** the `impl SqliteStore { ... }` blocks in `src/repo/sqlite/*.rs` and `src/state.rs` are independent of this change; leave them.
