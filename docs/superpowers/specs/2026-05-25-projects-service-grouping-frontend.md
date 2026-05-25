# Spec: Projects UI (Frontend) — companion to projects-service-grouping

Status: Draft · Date: 2026-05-25 · Frontend companion to
[`2026-05-25-projects-service-grouping.md`](2026-05-25-projects-service-grouping.md)

## Problem

The backend gains `/v1/projects` and project-scoped services. The web console
(`web/`, TanStack Start SPA) has no project UI; it only ships the scaffold demo
route. Operators need to manage projects and see services grouped by project.

## Goal

A project management surface in the console: list/create/view/delete projects,
edit shared config, and a project switcher that scopes the service views. Same
origin as `/v1`, bearer-authed, built on the existing Effect + Query layer and
the DESIGN.md system.

## Decisions

- **Effect first.** All `/v1/projects` calls are `ApiClient` methods returning
  typed Effects; React calls them through `runQuery`/Query `queryFn`/`mutationFn`.
  Schema-decode responses; typed `ApiError`/`DecodeError`.
- **Routes** (file-based, `web/src/routes/`): `/projects` (list + create),
  `/projects/$projectId` (detail: shared config + member services).
- **Project switcher** in `Header`, persisted in the URL search param
  `?project=` (shareable, back-button friendly).
- **Delete UX:** 409 (non-empty) surfaces as an inline fault, not a crash; the
  detail view lists member services so the operator can empty it.
- **Design:** flat `.panel`s, `.kicker` labels, `.signal` dots, `.btn`/
  `.btn-primary`, tabular numerics. No new visual primitives. Pink = primary
  action, violet = destructive/fault.

## Components / data flow

- `ApiClient` methods: `listProjects`, `getProject(id)`, `createProject(input)`,
  `deleteProject(id)`; `Schema` types `Project`, `ProjectInput`.
- `web/src/routes/projects/index.tsx` — list (Query `['projects']`) + create form.
- `web/src/routes/projects/$projectId.tsx` — detail; shared-env key/value editor,
  default-limits fields, member-services list (`['projects', id, 'services']`).
- `ProjectSwitcher` in `Header`; reads/writes `?project=`.
- Mutations invalidate `['projects']` and the detail query.

## Errors / edge cases

- Duplicate name -> conflict surfaced inline.
- Delete non-empty -> 409 -> inline message + link to member services.
- Fresh install has seeded `default` -> switcher shows it; empty-state copy on list.
- 401 -> single auth-needed banner (full login is sub-project C).

## Success criteria

- Create/list/open/edit/delete projects; delete blocked when non-empty with a
  clear message.
- The switcher scopes which services the console shows.
- All network state flows through Effect + Query; no raw fetch in components.

## Testing

- `@effect/vitest`: project `ApiClient` methods + Schema decode with stub
  `HttpClient` (success + 404/409 -> typed errors).
- `@testing-library/react`: list renders from Query; create calls mutation;
  delete shows 409 message.

## Out of scope

RBAC/login UI (C), metrics/logs (console companion), project-wide lifecycle
buttons. Backend behaviour (its own spec).
