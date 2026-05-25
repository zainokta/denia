# ADR-008: RBAC

- **Status**: Proposed
- **Date**: 2026-05-25

## Context

Until now, `/v1` was protected by a single bootstrap bearer token shared by all
operators. Multi-user operation needs per-user identity (with password login
and revocable API tokens) and project-scoped roles so an operator on Project A
cannot read or mutate Project B.

## Decision

- Domain types: `User`, `ProjectMembership { user_id, project_id, role }`,
  `ApiToken`, `Session`. `Role` is `Viewer < Operator < Admin`. A `super_admin`
  flag on `User` bypasses project membership checks.
- Identity resolution lives in `src/auth.rs`. `resolve_auth` accepts either
  the bootstrap admin token (→ `Principal::super_admin`), a session token, or
  an API token. The bootstrap admin token continues to grant super-admin so
  existing automation keeps working.
- All `/v1` routes except `POST /auth/login` go through `require_auth`, which
  populates a `Principal` request extension. `Principal` is then extracted by
  handlers via `FromRequestParts`.
- `require_project_role(principal, project_role, min)` is the single
  enforcement point. Resource handlers look up the target project (from the
  request body, the service's `project_id`, or the job's `project_id`) and
  call `ensure_role(&state, &principal, project_id, min)` which short-circuits
  for super-admins.
- Required roles by endpoint:
  - Viewer: `GET /services`, `GET /services/{id}/deployments`,
    `GET /services/{id}/metrics`, `GET /projects`, `GET /projects/{id}`,
    `GET /jobs`, `GET /jobs/{id}`, `GET /jobs/{id}/runs`.
  - Operator: `POST /services`, `POST /deployments`,
    `POST /services/{id}/{action}`, `GET /services/{id}/logs`,
    `POST /jobs`, `DELETE /jobs/{id}`, `POST /jobs/{id}/run`.
  - Admin: `DELETE /projects/{id}`.
  - Super-admin: `POST /projects`, `POST /credentials/*`,
    `GET /ingress/routes`, `GET /ingress/config`, `GET /users`,
    `POST /users`, `DELETE /users/{id}`.
- List endpoints filter by membership for non-super-admins (`GET /services`
  and `GET /projects` only return rows whose `project_id` matches a row in
  `project_members`).

## Consequences

- A single helper (`ensure_role`) handles per-handler enforcement; this keeps
  middleware focused on resolving identity and leaves authorization to the
  handlers that already know the target project.
- Super-admin still bypasses role checks deliberately — operators bootstrap
  with the admin token, then create users and grant memberships.
- Logs read is Operator-gated because workload logs can contain secrets;
  metrics is Viewer because it exposes only cgroup counters.

## Alternatives Considered

- **Middleware-driven per-route role config**: rejected; the project is not
  always in the URL path (it can be on the body or derived from a service),
  so handlers are a better authorization site than middleware.
- **Drop the bootstrap admin token**: rejected for now; super-admin is needed
  to seed the first user.

## References

- `docs/superpowers/plans/2026-05-25-rbac.md`
- `docs/superpowers/specs/2026-05-25-rbac.md`
- ADR-006 (projects)
