# Projects (Service Grouping) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a Project that owns multiple services and carries shared config (env + default limits) the services inherit, with per-project service names and versioned SQLite migrations.

**Architecture:** New `Project` domain type + `projects` table. `ServiceConfig` gains `project_id` and `env`. A `schema_version`-driven migration rebuilds `services` for `(project_id, name)` uniqueness and backfills a seeded `default` project. The runtime keys host paths off the globally-unique `service_id` instead of `service_name`. `/v1/projects` CRUD is added; service create requires an existing project.

**Tech Stack:** Rust 2024, axum 0.8, rusqlite (bundled SQLite), serde, uuid v7, thiserror. Spec: `docs/superpowers/specs/2026-05-25-projects-service-grouping.md`.

---

## File Structure

- `src/domain.rs` — add `Project`; extend `ServiceConfig` (`project_id`, `env`); env-merge + effective-limits helpers.
- `src/state.rs` — `schema_version` table + ordered migrations; `projects` table + rebuilt `services`; project CRUD; per-project service queries + count.
- `src/runtime.rs` + `src/domain.rs` — `RuntimeStartRequest.service_id`; `LinuxRuntime` paths from `service_id`.
- `src/deploy.rs` — pass `service_id` + merged env into `RuntimeStartRequest`.
- `src/app.rs` — `/v1/projects` routes + handlers; `ApiError::Conflict`; service `project_id` validation.
- `docs/adr/006-projects-and-migrations.md` + `docs/adr/README.md` — record the decision.
- Tests colocated as `#[cfg(test)]` in each module + `tests/backend_contract.rs` for API.

Commit after every task.

---

## Task 1: `Project` domain type

**Files:**
- Modify: `src/domain.rs`
- Test: `src/domain.rs` (`#[cfg(test)]`)

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn project_rejects_empty_name() {
    assert_eq!(Project::new("", None).unwrap_err(), DomainError::EmptyName);
}

#[test]
fn project_has_id_and_defaults() {
    let p = Project::new("default", Some("seed".into())).unwrap();
    assert_eq!(p.name, "default");
    assert!(p.shared_env.is_empty());
    assert!(p.default_resource_limits.is_none());
}
```

- [ ] **Step 2: Run, verify fail** — `cargo test domain::tests::project` → FAIL (no `Project`).

- [ ] **Step 3: Implement**

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub shared_env: Vec<(String, String)>,
    #[serde(default)]
    pub default_resource_limits: Option<ResourceLimits>,
    pub created_at: DateTime<Utc>,
}

impl Project {
    pub fn new(name: impl Into<String>, description: Option<String>) -> Result<Self, DomainError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(DomainError::EmptyName);
        }
        Ok(Self {
            id: Uuid::now_v7(),
            name,
            description,
            shared_env: Vec::new(),
            default_resource_limits: None,
            created_at: Utc::now(),
        })
    }
}
```

- [ ] **Step 4: Run, verify pass** — `cargo test domain::tests::project` → PASS.
- [ ] **Step 5: Commit** — `git commit -m "feat(domain): add Project type"`

---

## Task 2: `ServiceConfig` gains `project_id` + `env`, plus merge helpers

**Files:**
- Modify: `src/domain.rs` (`ServiceConfig`, `ServiceConfig::new`)
- Test: `src/domain.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn effective_env_merges_service_over_project() {
    let project = Project::new("p", None).map(|mut p| {
        p.shared_env = vec![("A".into(), "1".into()), ("B".into(), "p".into())];
        p
    }).unwrap();
    let svc = service_fixture(project.id, vec![("B".into(), "s".into()), ("C".into(), "3".into())]);
    let env = svc.effective_env(&project);
    assert_eq!(env.get("A"), Some(&"1".to_string()));
    assert_eq!(env.get("B"), Some(&"s".to_string())); // service wins
    assert_eq!(env.get("C"), Some(&"3".to_string()));
}

#[test]
fn effective_limits_fall_back_to_project_default() {
    let mut project = Project::new("p", None).unwrap();
    project.default_resource_limits = Some(ResourceLimits { cpu_millis: 250, memory_bytes: 1 });
    let svc = service_fixture(project.id, vec![]); // service uses None-equivalent
    assert_eq!(svc.effective_limits(&project).cpu_millis, /* service value or 250 */ 250);
}
```
(`service_fixture` is a small test helper building a `ServiceConfig` with the given project_id/env; add it in the test module.)

- [ ] **Step 2: Run, verify fail** — fields/methods missing.

- [ ] **Step 3: Implement**
- Add to `ServiceConfig`: `pub project_id: Uuid,` and `#[serde(default)] pub env: Vec<(String, String)>,`.
- Update `ServiceConfig::new` signature to take `project_id: Uuid` and `env: Vec<(String,String)>` (set before validation).
- Make `resource_limits` optional for fallback: keep `resource_limits: ResourceLimits` but add `effective_limits`. (Simplest: keep field; `effective_limits` returns the service value unless it equals `ResourceLimits::default()` is ambiguous — instead change field to `Option<ResourceLimits>`. Decide here and update callers.)
- Add helpers:

```rust
impl ServiceConfig {
    pub fn effective_env(&self, project: &Project) -> std::collections::BTreeMap<String, String> {
        let mut map: std::collections::BTreeMap<String, String> =
            project.shared_env.iter().cloned().collect();
        map.extend(self.env.iter().cloned()); // service wins
        map
    }

    pub fn effective_limits(&self, project: &Project) -> ResourceLimits {
        self.resource_limits
            .clone()
            .or_else(|| project.default_resource_limits.clone())
            .unwrap_or_default()
    }
}
```
(If `resource_limits` becomes `Option<ResourceLimits>`, update `new` and all construction sites + existing tests accordingly.)

- [ ] **Step 4: Run, verify pass** — `cargo test domain` → PASS.
- [ ] **Step 5: Commit** — `git commit -m "feat(domain): service project_id, env, and shared-config merge"`

---

## Task 3: Versioned migration infrastructure

**Files:**
- Modify: `src/state.rs` (`migrate`)
- Test: `src/state.rs`

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn migrate_is_idempotent_and_records_version() {
    let store = SqliteStore::open_in_memory().unwrap();
    store.migrate().unwrap();
    store.migrate().unwrap(); // no-op second time
    let v = store.schema_version().unwrap();
    assert!(v >= 2);
}
```

- [ ] **Step 2: Run, verify fail** — `schema_version` missing.

- [ ] **Step 3: Implement**
- Add table `schema_version (version INTEGER NOT NULL)` (single row) created first.
- Define migrations as an ordered `&[(&str /*sql*/,)]` or `&[fn(&Connection)->rusqlite::Result<()>]`; apply each step whose index > current version inside a transaction, then bump version.
- Migration 1 = the current baseline (`credentials`, `services`, `deployments`, `artifacts`, `promoted_deployments`) as `CREATE TABLE IF NOT EXISTS` (so existing DBs converge).
- Add `pub fn schema_version(&self) -> Result<i64, StateError>`.

- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** — `git commit -m "feat(state): versioned schema migrations"`

---

## Task 4: Projects migration (table, seed default, rebuild services)

**Files:**
- Modify: `src/state.rs` (migration 2)
- Test: `src/state.rs`

- [ ] **Step 1: Write failing test**

```rust
#[test]
fn migration_seeds_default_project_and_backfills_services() {
    // Build a store at migration-1 shape with one legacy service row, then migrate to 2.
    let store = SqliteStore::open_in_memory().unwrap();
    store.migrate().unwrap();
    let default_id = store.default_project_id().unwrap();
    let projects = store.list_projects().unwrap();
    assert!(projects.iter().any(|p| p.id == default_id && p.name == "default"));
}
```

- [ ] **Step 2: Run, verify fail.**

- [ ] **Step 3: Implement migration 2**
- `CREATE TABLE projects (id TEXT PRIMARY KEY, name TEXT NOT NULL UNIQUE, config_json TEXT NOT NULL)` (store the full `Project` as JSON like services).
- Insert a seeded `default` project (stable known id or look up by name).
- Rebuild `services`:
  ```sql
  CREATE TABLE services_new (
      id TEXT PRIMARY KEY,
      project_id TEXT NOT NULL,
      name TEXT NOT NULL,
      config_json TEXT NOT NULL,
      UNIQUE(project_id, name)
  );
  INSERT INTO services_new (id, project_id, name, config_json)
      SELECT id, '<default-id>', name, config_json FROM services;
  DROP TABLE services;
  ALTER TABLE services_new RENAME TO services;
  ```
- Backfill each migrated `config_json` to include `project_id` (default) and empty `env` — do this in Rust by deserializing into the new `ServiceConfig`, setting `project_id`, reserializing; or store project_id only in the column and let row read overwrite the JSON field on next write. Prefer rewriting JSON during migration for consistency.
- Add `pub fn default_project_id(&self) -> Result<Uuid, StateError>`.

- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** — `git commit -m "feat(state): projects table, default seed, services backfill"`

---

## Task 5: Project CRUD + project-scoped service queries

**Files:**
- Modify: `src/state.rs`
- Test: `src/state.rs`

- [ ] **Step 1: Write failing tests** — `put_project`/`get_project`/`list_projects`/`delete_project`; `count_services_in_project`; `put_service` enforces `(project_id, name)`.

```rust
#[test]
fn delete_project_blocked_when_non_empty() {
    let store = SqliteStore::open_in_memory().unwrap();
    store.migrate().unwrap();
    let p = store.put_project(Project::new("web", None).unwrap()).unwrap();
    store.put_service(service_fixture(p.id, vec![])).unwrap();
    assert!(matches!(store.delete_project(p.id), Err(StateError::ProjectNotEmpty)));
}
```

- [ ] **Step 2: Run, verify fail.**

- [ ] **Step 3: Implement**
- `put_project` (upsert on name), `get_project`, `list_projects`, `count_services_in_project`, `delete_project` (return `StateError::ProjectNotEmpty` if count > 0).
- Update `put_service` INSERT to include `project_id` column and `ON CONFLICT(project_id, name)`.
- Add `StateError::ProjectNotEmpty` and `StateError::UnknownProject` variants.

- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** — `git commit -m "feat(state): project CRUD and project-scoped services"`

---

## Task 6: Runtime keyed by `service_id`

**Files:**
- Modify: `src/domain.rs` (`RuntimeStartRequest`), `src/runtime.rs`
- Test: `src/runtime.rs`, `tests/linux_runtime_privileged.rs`

- [ ] **Step 1: Write failing test** — `plan()` builds cgroup/socket-relevant paths from `service_id`.

```rust
#[test]
fn plan_uses_service_id_for_cgroup_path() {
    // construct runtime + request with known service_id; assert cgroup_path ends with service_id.
}
```

- [ ] **Step 2: Run, verify fail.**

- [ ] **Step 3: Implement**
- Add `pub service_id: Uuid` to `RuntimeStartRequest`.
- In `LinuxRuntime::plan`, build `service_dir`/`cgroup_path` from `request.service_id.to_string()` (keep `validate_service_name` for the human name if still carried, or drop name from path logic).
- Update all `RuntimeStartRequest { .. }` construction sites (deploy, tests) to pass `service_id`.

- [ ] **Step 4: Run, verify pass** (`cargo test`; privileged test still compiles).
- [ ] **Step 5: Commit** — `git commit -m "refactor(runtime): key host paths off service_id"`

---

## Task 7: Deploy coordinator threads `service_id` + merged env

**Files:**
- Modify: `src/deploy.rs`
- Test: `src/deploy.rs` / `tests/deploy_orchestration.rs`

- [ ] **Step 1: Write failing test** — deploying a service in a project with shared_env produces a `RuntimeStartRequest`/process spec containing the merged env and the service_id.
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement** — coordinator fetches the owning `Project`, computes `effective_env`/`effective_limits`, and passes them + `service_id` into the runtime start path.
- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** — `git commit -m "feat(deploy): apply project shared env and id to runtime start"`

---

## Task 8: `/v1/projects` API + service validation

**Files:**
- Modify: `src/app.rs`
- Test: `tests/backend_contract.rs`

- [ ] **Step 1: Write failing tests**
- `POST /v1/projects` then `GET /v1/projects` returns it.
- `DELETE /v1/projects/{id}` with a service → 409.
- `POST /v1/services` with unknown `project_id` → 404; with valid project → 200; duplicate `(project_id, name)` handled.

- [ ] **Step 2: Run, verify fail.**

- [ ] **Step 3: Implement**
- Add `ApiError::Conflict(String)` → 409 and map `StateError::ProjectNotEmpty` → 409, `StateError::UnknownProject` → 404.
- Routes in `build_router` `protected`:
  ```rust
  .route("/projects", get(list_projects).post(create_project))
  .route("/projects/{project_id}", get(get_project).delete(delete_project))
  ```
- Handlers: `create_project`, `list_projects`, `get_project`, `delete_project`.
- In `put_service`/`create_deployment`, validate the referenced project exists (404 if not).

- [ ] **Step 4: Run, verify pass** — `cargo test --test backend_contract`.
- [ ] **Step 5: Commit** — `git commit -m "feat(api): /v1/projects CRUD and service project validation"`

---

## Task 9: ADR + docs

**Files:**
- Create: `docs/adr/006-projects-and-migrations.md`
- Modify: `docs/adr/README.md` (index row), `AGENTS.md` (note projects + versioned migrations)

- [ ] **Step 1:** Write ADR-006 (Status: Proposed): project = grouping + shared config; per-project names; runtime keyed by service_id; versioned migrations; delete blocked when non-empty. Alternatives: global names, project_id-in-JSON only, cascade delete.
- [ ] **Step 2:** Add the index row and the AGENTS.md note.
- [ ] **Step 3: Commit** — `git commit -m "docs: ADR-006 projects and versioned migrations"`

---

## Final Verification

- [ ] `cargo build`
- [ ] `cargo fmt --all`
- [ ] `cargo clippy --all-targets --all-features`
- [ ] `cargo test` — domain merge, migration backfill/idempotency, state CRUD + 409, runtime path-by-id, deploy env, API contract all green.
- [ ] Manual: create a project, create a service in it with `project_id`, deploy, confirm merged env reaches the workload and host paths use the service id.

## Notes

- Frontend (`web/`) is out of scope per `AGENTS.md` unless explicitly requested; the SPA can add a project switcher later.
- `tests/backend_contract.rs` and `tests/deploy_orchestration.rs` had in-progress local changes when this plan was written; reconcile before editing.
- This is sub-project B only. RBAC (C) will scope to `project_id`; keep that join key clean.
