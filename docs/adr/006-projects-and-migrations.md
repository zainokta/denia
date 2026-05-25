# ADR-006: Projects And Versioned Migrations

**Status:** Proposed
**Date:** 2026-05-25

## Context

Services currently live in a flat global namespace with no grouping mechanism. As the platform grows, operators need to organize services into projects for access control (RBAC), shared configuration (environment variables, default resource limits), and namespace isolation (per-project unique service names).

## Decision

### Project Domain Type

A `Project` owns multiple services and carries shared configuration:

- `shared_env`: key-value pairs inherited by all project services (service-level env overrides)
- `default_resource_limits`: fallback CPU/memory limits when a service does not specify its own

`ServiceConfig` gains `project_id` (UUID reference) and `env` (service-level overrides). `effective_env()` and `effective_limits()` merge project defaults with service overrides.

### Per-Project Service Names

Service names are unique within a project (`UNIQUE(project_id, name)`). The `services` table is rebuilt during migration to include `project_id`. A seeded `default` project backfills all existing services.

### Versioned Migrations

A `schema_version` table tracks the applied migration version, enabling ordered, idempotent schema evolution. Each migration step runs only if `current_version < step_version`, inside a single transaction.

### Runtime Keyed By Service ID

`RuntimeStartRequest` gains `service_id: Uuid`. Host paths (cgroup, socket, deployment dirs) are derived from the globally-unique `service_id` instead of `service_name`, preventing collisions across projects.

### Delete Guard

`delete_project` returns `409 Conflict` when the project still has services. No cascade delete.

## Alternatives Considered

- **Global service names only:** Rejected because it prevents projects from having services with the same name.
- **project_id only in JSON:** Rejected because querying/joining on a column is faster and SQL cannot index into JSON fields consistently.
- **Cascade delete:** Rejected because deleting a project should be a deliberate act after removing all services.

## Consequences

- All service creation must now reference a valid `project_id`. The API returns 404 for unknown projects.
- Runtime host paths remain stable across renames because they use `service_id`.
- The migration system (version 1 = baseline, version 2 = projects) is idempotent and safe to run on existing databases.
- RBAC (ADR-008) and Jobs (ADR-009) depend on this project model.
