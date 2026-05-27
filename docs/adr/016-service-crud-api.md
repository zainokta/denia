# ADR-016: Service CRUD API

- **Status**: Proposed
- **Date**: 2026-05-27

## Context

The `/v1/services` API today exposes only two operations:

- `GET /v1/services` — list services.
- `POST /v1/services` (`put_service`) — an upsert that takes a full
  `ServiceConfig`, including its `id`.

There is no way to read a single service by id, and no way to delete one. The
web console therefore cannot offer create, read, or delete. The services page is
a dead end: an empty state with no create path and no way to inspect or remove
an existing service.

Separately, Denia mandates UUIDv7 for every persisted id (see `CLAUDE.md`): IDs
are generated server-side with `Uuid::now_v7()` so they stay time-ordered and
preserve SQLite B-tree index locality. A web client cannot honor this invariant
— `crypto.randomUUID` produces a UUIDv4 — so clients must not generate service
IDs at all. The current `put_service` contract, which requires the caller to
supply `ServiceConfig.id`, conflicts with that rule for the create case.

## Decision

- Add `GET /v1/services/{service_id}`, authorized for the **Viewer** role on the
  service's project. It returns the `ServiceConfig`, or `404` if no service with
  that id exists.

- Add `DELETE /v1/services/{service_id}`, authorized for the **Operator** role.
  It removes the service.

- Make `ServiceConfig.id` default to nil on deserialization, and resolve the id
  server-side inside `put_service`:
  - If the incoming `id` is nil, reuse the existing service's id for the same
    `(project_id, name)` when one exists; otherwise mint a fresh `Uuid::now_v7()`.
  - If the incoming `id` is non-nil, keep it (the update path).

  The persistence layer upserts on `ON CONFLICT(project_id, name)` and never
  updates the primary-key `id`. Minting a fresh id for a row that already exists
  would leave `config_json.id` pointing at a different value than the row's PK,
  so the create path must reuse the existing id rather than generate a new one.

  This lets the web client create a service by POSTing a body with no `id`, while
  updates keep their id stable.

## Consequences

- The web console gains full CRUD: create (POST with no `id`), read single
  (`GET /v1/services/{service_id}`), update (POST with `id`), delete
  (`DELETE /v1/services/{service_id}`), and the existing list.
- A single POST endpoint serves both create and update; the client never
  generates IDs, preserving the UUIDv7 invariant and SQLite B-tree locality.
- `put_service` carries slightly more logic: a lookup by `(project_id, name)`
  when the incoming id is nil.
- `GET` and `DELETE` follow the project-scoped RBAC split already used elsewhere
  (Viewer can read, Operator can mutate).

## Alternatives Considered

- **Client-generated UUID on create**: rejected. The browser cannot guarantee
  UUIDv7 (`crypto.randomUUID` is v4), which violates the persisted-id invariant
  and breaks index locality and deterministic ordering.
- **A separate `ServiceInput` type without `id` plus a distinct create
  endpoint**: rejected as heavier than defaulting `id` and reusing the existing
  upsert. It would duplicate validation and split the create/update paths for no
  gain over a nil-id default on the single POST.

## References

- ADR-006 (Projects And Versioned Migrations).
- ADR-008 (Project-Scoped RBAC).
- ADR-012 (src/ Modularization and Per-Aggregate Repositories).
