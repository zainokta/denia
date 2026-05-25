# Spec: RBAC UI (Frontend) — companion to rbac

Status: Draft · Date: 2026-05-25 · Frontend companion to
[`2026-05-25-rbac.md`](2026-05-25-rbac.md)

## Problem

The backend gains users, password login, session/API tokens, and project-scoped
roles. The console currently has no auth: `ApiClient` reads a static
`VITE_DENIA_TOKEN` from the build env. There is no login, no identity, and no way
to gate actions by role.

## Goal

Add login/logout, a runtime auth-token store wired into `ApiClient`, an identity
view (`/me`), user management (super-admin) and project member management, and
role-gated UI so viewers cannot see operator/admin actions. Effect + Query layer,
DESIGN.md system.

## Backend surface consumed

- `POST /v1/auth/login` -> `{ token, expires_at }`, `POST /v1/auth/logout`
- `GET /v1/me` -> `{ principal, is_super_admin, memberships }`; `principal` is
  `{ kind: "user", user }` for a real user or `{ kind: "bootstrap" }` for the
  env bootstrap bearer.
- `GET/POST /v1/users`, `DELETE /v1/users/{id}`
- `GET/POST /v1/api-tokens`, `DELETE /v1/api-tokens/{id}`
- `GET/POST /v1/projects/{id}/members`, `DELETE .../members/{user_id}`

## Decisions

- **Runtime token, not captured build env.** Replace the static captured
  `VITE_DENIA_TOKEN` with a client token store. Session tokens go in
  `sessionStorage` (memory fallback when `window` is absent), not `localStorage`;
  `VITE_DENIA_TOKEN` remains a read-only bootstrap/dev fallback. `AppConfig`
  exposes a `getAuthToken()`/`authHeader()` function (or Effect), not a captured
  `token` value, so each `ApiClient` method reads the current bearer at request
  time. Login writes the session token; logout posts best-effort, then clears it.
- **Auth gate.** A root `beforeLoad`/guard redirects to `/login` when there is no
  runtime token or bootstrap fallback; `/me` failure (401) clears the runtime
  token, clears auth Query cache, and bounces to `/login`.
- **Role gating from `/me`.** The active project (`?project=` from sub-project B)
  + the user's membership role decide which actions render. Viewers see read-only
  views; operator actions (deploy/stop) and admin actions (members) are hidden or
  disabled with a reason. Role comparisons use an explicit rank map
  (`viewer=0`, `operator=1`, `admin=2`), never string/enum ordering.
  Super-admin sees user management. Bootstrap principals see user/project
  management but not self API-token minting.
- **Design:** login is a single calm `.panel` (mono, dark); identity/role shown
  as a `.kicker` + `.signal` chip in the header. No new visual primitives.

## Components / data flow

- Auth store: `web/src/effect/auth-store.ts` (get/set/clear/subscribe token in
  `sessionStorage`, SSR-safe memory fallback); `AppConfig` exposes current-token
  accessors.
- `ApiClient` methods: `login`, `logout`, `me`, users CRUD, api-tokens CRUD,
  members CRUD; `Schema` `Role`, `User`, `PrincipalView`, `Membership`,
  `ApiToken`, `Me`.
- Routes: `/login`; `/settings/users` (super-admin); `/settings/tokens` (self);
  project members editor on the project detail view (sub-project B route).
- `useAuth()` hook: current `Me` (Query `['me']`), `roleForActiveProject()`,
  token subscription, `login`/`logout` mutations.
- Header: identity chip + logout.

## Errors / edge cases

- Wrong credentials -> inline 401 on the login panel (no enumeration hint).
- Token expired mid-session -> next 401 clears token + redirect to `/login`.
- Last super-admin demote/delete -> 409 surfaced inline.
- API token shown once on creation (copy), never re-fetchable.
- Bootstrap principal cannot mint self API tokens -> hide `/settings/tokens` or
  show a prompt to create/login as a real super-admin.
- No active project -> role-gated actions hidden until one is selected.

## Success criteria

- Operator logs in, sees only what their role allows, and is blocked (hidden +
  403-safe) from disallowed actions.
- Super-admin manages users; project admins manage members.
- Logout clears the token; protected routes redirect to `/login`.

## Testing

- `@effect/vitest`: auth `ApiClient` methods + Schema; `AppConfig`/`ApiClient`
  reads the updated token without rebuilding the module runtime; 401 mapping.
- `@testing-library/react`: login form success/failure; guard redirect when no
  token; role gating hides operator/admin actions for a viewer; users page
  visible only to super-admin; bootstrap principal hides self-token minting.

## Out of scope

SSO/OAuth/MFA UI, password reset flows, audit-log view. Backend behaviour (its
own spec). Builds on the operator-console + projects companions for the gated
actions and the member editor.
