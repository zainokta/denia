## ADR-012: src/ Modularization and Per-Aggregate Repositories

- **Status**: Proposed
- **Date**: 2026-05-25

## Context

`src/` grew flat. Four files crossed the comprehension threshold:

- `app.rs` — 1014 lines: `AppState`, `build_router`, and every handler.
- `runtime.rs` — 1237 lines: `Runtime` trait + `LinuxRuntime` + `FakeRuntime` + validation + filesystem helpers + namespace-launcher resolution.
- `state.rs` — 1129 lines: a single `SqliteStore` owning persistence for services, projects, users, deployments, jobs, tokens, and credentials.
- `domain.rs` — 458 lines: every domain type in one file.

Symptoms against SOLID:

- **SRP**: `state.rs` is a god-object spanning every aggregate. `app.rs` couples routing, state, and handler logic. `runtime.rs` mixes the trait, the Linux implementation, validation, and filesystem helpers.
- **ISP**: handlers depend on the full `SqliteStore` surface even when they touch one method. Test doubles must satisfy the whole struct.
- **DIP**: handlers bind to concrete `SqliteStore`. Only `Runtime`, `CommandRunner`, and `HealthChecker` are trait-abstracted.
- **OCP**: replacing or wrapping persistence requires editing `app.rs` and every handler.

Folder-modules already exist for `artifacts/`, `oci/`, and `syscall/` — the pattern is established and the rest of `src/` should match.

## Decision

1. Convert every multi-concern flat module to a folder-module with one concern per file.
2. Introduce per-aggregate repository traits in `src/repo/`:
   `ServiceRepo`, `ProjectRepo`, `UserRepo`, `DeploymentRepo`, `JobRepo`,
   `TokenRepo`, `CredentialRepo`. Sqlite implementations live in `src/repo/sqlite/`.
3. Replace `AppState`'s concrete `SqliteStore` field with `Arc<dyn ...Repo>` per aggregate. Add an `AppStateBuilder` for test wiring.
4. Move axum handlers from `app.rs` into `src/api/<resource>.rs` modules, each exporting `pub fn router() -> Router<AppState>`. `app.rs` becomes a thin assembler.
5. Split `domain.rs` into `src/domain/{service,deployment,project,user,credential,job,error}.rs` with full `pub use` re-exports from `domain/mod.rs`.
6. Split `runtime.rs` into `src/runtime/{runtime_trait,linux,fake,plan,validation,fs_helpers,error}.rs`.
7. Group ingress (`traefik`, `bridge`, `socket_proxy`) under `src/ingress/`; observability (`metrics`, `node_metrics`, `access_log`, `logs`) under `src/observability/`.
8. Centralize `ApiError` plus `From<RepoError | DomainError | RuntimeError | DeployError>` conversions in `src/api/error.rs`.
9. Preserve every previously-public symbol via `pub use` in `mod.rs` files. Zero call-site churn outside `app.rs` and the `AppState` rewire commit.
10. The API surface (`/v1/*` paths and bodies), DB schema, SOPS layout, Traefik file-provider contract, and SPA embed (ADR-004) are unchanged.

## Out of Scope

- SOPS backend trait extraction.
- `BridgeAllocator` trait promotion.
- Scheduler refactor.
- Multi-node control plane.
- API surface changes.
- Frontend changes.

## Consequences

### Positive

- **SRP**: one concern per file. Largest file drops from ~1200 → ~400 lines.
- **ISP**: handlers depend only on the repos they use; `AppStateBuilder` exposes each independently.
- **DIP**: handler tests inject `InMemoryServiceRepo` etc.; no real SQLite required.
- **OCP**: a future PostgreSQL backend implements the same traits without touching handlers.
- Lower onboarding cost — folder layout maps to architecture.
- Independent unit testability of validation and filesystem helpers in `runtime/`.

### Negative

- `Arc<dyn>` per repo adds vtable indirection — negligible on the HTTP path.
- Re-export shims in `mod.rs` files require discipline to stay current.
- Steps 9–10 (replacing `SqliteStore` with per-aggregate repos in `AppState`) are a high-risk window during migration. Mitigated by paired commits with a temporary adapter, and per-step `cargo build && cargo test && cargo fmt && cargo clippy` gating.
- More files to navigate, fewer lines per file — net win in IDE/grep workflows after a one-time mental shift.

### Neutral

- No runtime behavior change. No DB migration. No API contract change. ADR-004 (SPA embed) and ADR-005 (runtime hardening) untouched.

## Alternatives Considered

- **Split files only, keep `SqliteStore` as a single struct**: rejected. Solves SRP at the file level but leaves ISP and DIP violations in `AppState`. Handler tests still require a real `SqliteStore`.
- **Per-aggregate concrete structs without traits**: rejected. Each struct still embeds `SqlitePool` directly; future backend swaps remain edit-heavy. ISP wins are partial — handlers still depend on concrete types.
- **Controller structs per resource (OO-style)**: rejected. Axum's free-function handler style is established in the codebase. Adding controller structs is boilerplate without payoff for a single-binary, single-process service.
- **Big-bang refactor in one commit**: rejected. Steps 8–11 are independently risky; per-step `cargo` gating gives precise revert points.

## References

- `docs/superpowers/specs/2026-05-25-src-modularization-design.md` (this refactor's design + 14-step migration order)
- ADR-001 Initial Backend Architecture
- ADR-003 Linux Runtime Process Runner
- ADR-004 Embed Web Console
- ADR-005 Runtime Security Hardening
