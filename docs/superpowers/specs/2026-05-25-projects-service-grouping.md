# Spec: Projects (Service Grouping) — TODO #4

Status: Draft · Date: 2026-05-25 · Sub-project B of the TODO decomposition

## Problem

Denia has a flat list of services (`services` table, globally unique `name`,
`config_json` blob). There is no way to group related services the way Dokploy
groups them under a project. Grouping is also a prerequisite for later
sub-projects (RBAC scoping, per-project monitoring).

## Goal

Introduce a Project that owns multiple services and carries shared config that
services inherit. Keep the single-node control plane simple.

## Decisions

- **Project = grouping + shared config.** Fields: `id`, `name` (globally unique),
  `description`, `shared_env: Vec<(String, String)>`,
  `default_resource_limits: Option<ResourceLimits>`, `created_at`.
- **Service names are unique per project**, not globally. Because the runtime
  keys host cgroup/socket paths off the service identifier, those paths move from
  `service_name` to the globally-unique `service_id`.
- **Shared-config merge:** effective service env = `project.shared_env` merged
  with `service.env`, service key wins. Effective limits = `service.resource_limits`
  if set, else `project.default_resource_limits`.
- **Versioned migrations** replace the single idempotent `CREATE TABLE IF NOT
  EXISTS` batch (a `schema_version` table + ordered steps).
- **API:** `/v1/projects` CRUD; `ServiceConfig` gains a required `project_id`;
  existing service routes keep working.
- **Delete is blocked (409) while a project still has services.**

## Data model

- New `Project` domain type (fields above).
- `ServiceConfig` gains:
  - `project_id: Uuid` (required).
  - `env: Vec<(String, String)>` — **new**. Service-level env does not exist
    today (env currently comes only from the image `process.json`). This adds
    service env and project shared env, both threaded into the runtime.
- Uniqueness: `(project_id, name)` unique, replacing `UNIQUE(name)`.

## Runtime impact

- `RuntimeStartRequest` carries `service_id`; `LinuxRuntime` builds the cgroup
  path and socket path from `service_id` (globally unique) instead of
  `service_name`. The deploy coordinator and path builders update accordingly.
- Effective (merged) env is materialised into the runtime process spec so the
  workload receives project + service env.

## Migrations

- Introduce a `schema_version` table and an ordered list of migration steps run
  in `migrate()`.
- Step (baseline): the current tables.
- Step (projects): create `projects`; seed a `default` project; rebuild
  `services` to add `project_id NOT NULL` (backfilled to the default project) and
  `env`, and to swap `UNIQUE(name)` for `UNIQUE(project_id, name)` (SQLite table
  rebuild: create new, copy, drop, rename).

## API (`/v1`, bearer-protected)

- `GET /v1/projects` — list.
- `POST /v1/projects` — create (name, description, shared_env, default limits).
- `GET /v1/projects/{id}` — fetch.
- `DELETE /v1/projects/{id}` — 409 if it still owns services.
- `POST /v1/services` — payload requires `project_id`; 404 if the project is
  unknown; conflict if `(project_id, name)` already exists.

## Errors / edge cases

- Service references unknown `project_id` -> 404.
- Delete non-empty project -> 409.
- Duplicate service name within a project -> conflict (existing upsert-on-name
  behaviour becomes project-scoped).
- Backfill: all pre-existing services land in the seeded `default` project.

## Success criteria

- Projects can be created/listed/fetched/deleted; delete blocked when non-empty.
- A service belongs to exactly one project; same service name reusable across
  projects.
- A deployed workload receives merged project+service env and effective limits.
- Existing services continue to work, grouped under `default`.

## Testing

- Domain: env merge precedence (service over project); effective-limits fallback.
- State: migration backfill to `default`; `(project_id, name)` uniqueness;
  re-running `migrate()` is a no-op.
- API: project CRUD; 409 on non-empty delete; service create requires existing
  project; per-project name reuse.
- Runtime: cgroup/socket paths derive from `service_id`.

## Out of scope

Project domain-suffix inheritance, project-wide lifecycle ops (deploy/stop all),
RBAC scoping (sub-project C). The other TODO sub-projects.
