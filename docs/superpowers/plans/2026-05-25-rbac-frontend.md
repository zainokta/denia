# RBAC UI (Frontend) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add login/logout, a runtime auth-token store wired into `ApiClient`, a `/me` identity, user + project-member management, and role-gated UI to the console.

**Architecture:** Replace the captured static `VITE_DENIA_TOKEN` with a runtime session-token store that `ApiClient` reads per request, so login/logout does not require rebuilding the module `ManagedRuntime`. Session tokens live in `sessionStorage` (SSR-safe memory fallback); `VITE_DENIA_TOKEN` remains a read-only bootstrap/dev fallback. New auth `ApiClient` methods + Schema. A root guard redirects to `/login` without any bearer; a `useAuth()` hook exposes `Me` and the active-project role to gate actions.

**Tech Stack:** TanStack Start/Router/Query, React 19, Effect (`effect@beta`), `@effect/vitest`, `@testing-library/react`. Spec: `docs/superpowers/specs/2026-05-25-rbac-frontend.md`. Depends on the RBAC backend + sub-projects B (projects) and the operator-console companion.

---

## File Structure

- `web/src/effect/auth-store.ts` — get/set/clear/subscribe token in `sessionStorage` with SSR-safe memory fallback.
- `web/src/effect/config.ts` — `AppConfig` exposes `getAuthToken()`/`authHeader()` instead of a captured token value.
- `web/src/effect/schema.ts` — `Role`, `User`, `PrincipalView`, `Membership`, `Me`, `ApiToken`.
- `web/src/effect/api-client.ts` — `login`, `logout`, `me`, users/api-tokens/members.
- `web/src/hooks/useAuth.ts` — current `Me`, `roleForActiveProject`, login/logout.
- `web/src/routes/login.tsx`, `web/src/routes/__root.tsx` (guard).
- `web/src/routes/settings/users.tsx`, `web/src/routes/settings/tokens.tsx`.
- `web/src/components/Header.tsx` — identity chip + logout.
- Tests colocated.

Commit after each task.

---

## Task 1: Auth-token store + per-request AppConfig token access

**Files:**
- Create: `web/src/effect/auth-store.ts`
- Modify: `web/src/effect/config.ts`
- Test: `web/src/effect/api-client.test.ts`

- [ ] **Step 1: Write failing test** — after `setToken('abc')`, two calls through the existing `ManagedRuntime`/`ApiClient` include `Bearer abc`; after `setToken('def')`, the next call includes `Bearer def`; after `clearToken()`, no bearer is sent. This must pass without recreating the runtime.
- [ ] **Step 2: Run** `pnpm test` → FAIL.
- [ ] **Step 3: Implement** — `auth-store.ts`: `getToken()/setToken(t)/clearToken()/subscribe(listener)` over `sessionStorage` (SSR-safe: guard `window`, memory fallback). In `config.ts`, replace the `token` value with `getAuthToken(): string | undefined` or an Effect returning the current token, with `VITE_DENIA_TOKEN` as read-only fallback. Do not rely on `Layer.sync` for token freshness: the module-scope `ManagedRuntime` builds layers once, so the token must be read inside each API method.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** — `git commit -m "feat(web): runtime auth token store"`

---

## Task 2: Auth schema + ApiClient auth methods

**Files:**
- Modify: `web/src/effect/schema.ts`, `web/src/effect/api-client.ts`
- Test: `web/src/effect/api-client.test.ts`

- [ ] **Step 1: Write failing tests** — `login` decodes `{ token, expires_at }`; `me` decodes `{ principal: { kind: "user", user }, is_super_admin, memberships }`; `me` also decodes `{ principal: { kind: "bootstrap" }, is_super_admin: true, memberships: [] }`; bad login -> `ApiError` (401).
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** — Schema `Role = Literal('viewer','operator','admin')`, `User`, `PrincipalView = { kind: 'user', user } | { kind: 'bootstrap' }`, `Membership { project_id, role }`, `Me { principal, is_super_admin, memberships }`, `LoginResult`. ApiClient: `login(username, password)` (`POST /v1/auth/login`, no bearer), `logout` (sends current bearer before local clear), `me`, users CRUD, api-tokens CRUD, members CRUD. Map 401/403 to distinguishable `ApiError`.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** — `git commit -m "feat(web): auth schema + ApiClient methods"`

---

## Task 3: useAuth hook

**Files:**
- Create: `web/src/hooks/useAuth.ts`
- Test: `web/src/hooks/useAuth.test.ts`

- [ ] **Step 1: Write failing test** — with a mocked `['me']` Query, `roleForActiveProject('p1')` returns the membership role; `can('operator', role)` uses an explicit rank map (`viewer=0`, `operator=1`, `admin=2`); `login` mutation stores the token and invalidates/refetches `['me']`; `logout` posts best-effort, clears the token, and clears auth Query cache.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** — `useAuth()`: token state from `auth-store.subscribe` (for example via `useSyncExternalStore`); `me` Query `['me']` (enabled when a runtime token or bootstrap fallback exists); `login`/`logout` mutations (`setToken`/`clearToken`, invalidate/remove `['me']`); `roleForActiveProject(projectId)` reads memberships + `is_super_admin`; bootstrap principal is treated as super-admin for user/project management but has no self token page.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** — `git commit -m "feat(web): useAuth hook"`

---

## Task 4: Login route + root guard

**Files:**
- Create: `web/src/routes/login.tsx`
- Modify: `web/src/routes/__root.tsx`
- Test: `web/src/routes/login.test.tsx`

- [ ] **Step 1: Write failing tests** — submitting valid creds calls `login` and navigates away; invalid -> inline 401 message; visiting a protected route with no token redirects to `/login`.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** — `/login` single `.panel` form (username/password, `.btn-primary`); on success store the token, refetch `['me']`, then navigate to `/services`. In `__root` (or a layout route) add a guard: if no runtime token/bootstrap fallback and route is not `/login`, redirect; on `['me']` 401, `clearToken()`, remove auth queries, and redirect.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** — `git commit -m "feat(web): login + auth guard"`

---

## Task 5: Header identity + logout; role gating

**Files:**
- Modify: `web/src/components/Header.tsx`, `web/src/routes/services/*`
- Test: `web/src/components/Header.test.tsx`, route tests

- [ ] **Step 1: Write failing tests** — header shows username + role chip + logout; a viewer role hides the deploy/stop buttons on the services views.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** — header identity `.kicker` + `.signal` chip + logout button. In the console views, gate operator actions behind `can('operator', roleForActiveProject(projectId))` (hide or disable with a reason); never compare role strings or enum variants directly. Super-admin-only nav for `/settings/users`; hide `/settings/tokens` for bootstrap principals.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** — `git commit -m "feat(web): identity, logout, role-gated actions"`

---

## Task 6: User + token + member management

**Files:**
- Create: `web/src/routes/settings/users.tsx`, `web/src/routes/settings/tokens.tsx`
- Modify: project detail route (sub-project B) for the member editor
- Test: route tests

- [ ] **Step 1: Write failing tests** — users page lists/creates/deletes (visible only to super-admin); tokens page mints a token shown once for real users and is hidden/blocked for bootstrap principal; member editor sets a user's role on a project.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** — `/settings/users` (super-admin guard); `/settings/tokens` (real-user self only) with one-time token reveal + copy; member editor on the project detail view (add user + role select, remove). Mutations invalidate the relevant queries; last-super-admin 409 surfaced inline.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** — `git commit -m "feat(web): users, tokens, members management"`

---

## Final Verification

- [ ] `cd web && pnpm typecheck` (no `any`/`as`).
- [ ] `cd web && pnpm test` — store, auth schema/ApiClient, useAuth, login/guard, gating, management green.
- [ ] `cd web && pnpm build`.
- [ ] Manual (backend running): login as an operator -> only allowed actions
  show; expired/invalid token -> bounced to `/login`; super-admin manages users;
  project admin assigns roles; logout clears the token.

## Notes

- The runtime token store replaces the captured build-env token; `ApiClient` must
  read it per request (not capture once in `AppConfigLive`/`ManagedRuntime`).
- Role gating is UX only; the backend still enforces 403 — never trust the client.
- Builds on the operator-console + projects companions; sequence after them.
