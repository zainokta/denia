# One-Time Admin Bootstrap — Design

Date: 2026-05-27
Status: Approved (design), pending implementation plan

## Problem

A freshly launched Denia node has no user accounts. The only credential is the
host-local `DENIA_ADMIN_TOKEN`. We need a first-run flow that lets the operator
create the initial super-admin user exactly once, after which normal
authenticated user management takes over. The operation must be irreversible:
once the first admin is created, the bootstrap path is permanently closed even
if every user is later deleted.

## Goals

- A dedicated `POST /v1/bootstrap` endpoint that creates the first super-admin.
- Gated by the admin token (existing `Principal::super_admin()` / Bootstrap principal).
- Truly one-time, enforced by a **persistent flag** (survives user deletion).
- A `/setup` web page reached via a token-in-URL flow, asking only for username + password.
- Leave `POST /v1/users` unchanged for later super-admin-managed user creation.

## Non-Goals

- Reopening user creation after bootstrap via any special path (use `/v1/users`).
- Multi-node bootstrap, invite flows, password reset, email verification.
- Rate limiting beyond what already exists.

## Decisions

1. **Separate endpoint, not an overload of `/v1/users`.** Keeps the one-time
   guard isolated and leaves normal user management semantics intact.
2. **Persistent flag over user-count check.** A count check would reopen
   bootstrap if all admins were deleted; a flag set once never reopens. This is
   the security-relevant choice.
3. **Admin-token gated.** Only the holder of the host secret can register the
   first admin — no land-grab race on a freshly exposed node.
4. **Token-in-URL on the frontend.** The launch output surfaces a URL carrying
   the admin token (`?token=…`). The SPA stores it and `me()` returns the
   existing `Bootstrap` view, so no separate public status endpoint is needed.

## Backend Design

### Migration (schema version 7)

Current max version is 6 (`src/repo/sqlite/pool.rs`). Add a `current < 7` branch
creating a generic key/value settings table:

```sql
CREATE TABLE IF NOT EXISTS system_settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
```

Follow the existing migration pattern: `DELETE FROM schema_version; INSERT INTO
schema_version (version) VALUES (7);` at the end of the branch.

The bootstrap flag is stored as the row `('admin_initialized', 'true')`. A
generic kv table is chosen over a single-purpose column so future control-plane
settings have a home without another migration.

### Repo layer (`src/repo/sqlite/`, on `SqliteStore`)

- `is_admin_initialized(&self) -> Result<bool, StateError>` — `SELECT` the
  `admin_initialized` row, return whether it exists/equals `"true"`.
- `bootstrap_admin(&self, username, password_hash) -> Result<User, StateError>` —
  performs the whole operation inside a single SQLite transaction:
  1. Re-check the flag inside the transaction; if set, return a typed
     "already initialized" error.
  2. Create the user with `is_super_admin = true` (UUIDv7, via the existing
     `create_user_q` path).
  3. Insert `('admin_initialized', 'true')` into `system_settings`.
  4. Commit.

  `SqlitePool` is a single `Connection` behind one `Arc<Mutex>`
  (`src/repo/sqlite/pool.rs`), so all repo calls already serialize — overlapping
  bootstrap calls cannot truly race. The transaction's role is an **atomic
  check-and-set within the one locked connection**: it guarantees the flag check
  and the user+flag writes commit together (no partial state if an insert fails),
  and the in-transaction re-check makes a second call after commit deterministic.

Password hashing stays in the API layer via `crate::auth::hash_password`
(matching `create_user_handler`); the repo receives an already-hashed value.

### API layer (`src/api/`)

New module `src/api/bootstrap.rs` exposing:

```
POST /v1/bootstrap   body: { username, password }
```

- Added to the **authed** router via `.merge(bootstrap::router())` alongside the
  other authed routers in `src/app.rs` (same axum verb as `users::router()` etc.,
  not `.nest`), so the existing auth middleware requires a valid token.
  Missing/invalid token → 401 (unchanged middleware behavior).
- Handler requires `principal.is_super_admin` (the Bootstrap principal already
  satisfies this); otherwise 403, mirroring `users.rs`.
- Validate input: non-empty username (trimmed) and password length ≥ 8;
  otherwise 400 (`ApiError::BadRequest`). This is the only password policy for
  now — anything stronger is out of scope.
- If `is_admin_initialized()` is true → **409 Conflict** (`ApiError::Conflict`,
  already defined in `src/api/error.rs`).
- Else hash the password with `crate::auth::hash_password`, call
  `bootstrap_admin(...)`, and return `201 Created` with the created `User`
  (password hash is `skip_serializing`).

`/v1/users` and its handlers are untouched.

### `me()` / `PrincipalView`

Add `admin_initialized: bool` to the **`Me` struct itself** in
`src/domain/user.rs` (not to the `Bootstrap` variant). `me_handler`
(`src/api/auth.rs`) currently returns early with `PrincipalView::Bootstrap` in
one branch and `PrincipalView::User` in another — populate `admin_initialized`
from `state.users.is_admin_initialized()` in **both** branches so the field is
always present regardless of principal kind. (After the operator logs in as the
new user, `me()` returns the `User` variant, and the frontend still needs the
flag to route correctly.)

Rationale: with the token-in-URL flow, `me()` already succeeds for the admin
token. Surfacing `admin_initialized` lets the frontend distinguish "needs setup"
from "already set up, go log in" even when the stale token URL is reused.

## Frontend Design (`web/`)

### Schema (`web/src/effect/schema.ts`)

Add `admin_initialized: Schema.Boolean` to the `Me` schema (matching the backend
field). Add a `Bootstrap` request shape if needed for the new client method.

### API client (`web/src/effect/api-client.ts`)

Add `bootstrap(username, password)` to the `ApiClient` service: `POST
/v1/bootstrap` with `authHeaders()` + JSON body, parsed as `User` via
`parseResponse`. Wire it into the returned object, mirroring `createUser`.

### Token-in-URL handling

On app load, read `token` from the URL query string. If present, store it via
the existing `auth-store` (`setToken`) and strip it from the URL
(`history.replaceState`) so the secret isn't left in the address bar / history.
Residual risk: the token may still appear in server access logs / referrer for
the initial request — acceptable for a host-local first-run flow, noted here so
it isn't a surprise.

### Routing — sync gate vs async `me()`

This is the subtle part. `beforeLoad` in `__root.tsx` is **synchronous** and only
has access to `getToken()` (localStorage); it cannot read `me()`, which is an
async React Query. So the bootstrap decision cannot live in `beforeLoad`. Split
the responsibilities:

1. **`beforeLoad` (sync, unchanged in spirit):** token-presence redirects only.
   Allow `/login` and `/setup` without a token; redirect everything else to
   `/login` when `!hasAuth()`. (Token-in-URL must be stored *before* this runs —
   do it at module load / router creation so `getToken()` sees it.)
2. **A top-level gate component** (rendered inside `Chrome`/root, using
   `useAuth()`): once `meQuery` resolves, branch on the data:
   - while `meQuery.isLoading` → render a loading state (no redirect).
   - `isBootstrap && !admin_initialized` → `navigate('/setup')`.
   - `admin_initialized` but Bootstrap principal (stale token, already set up) →
     `navigate('/login')`.
   - authenticated `User` → render the app.
   Use `useEffect` + `router.navigate` for the redirects so they run after the
   query settles, not during render.

- New route `web/src/routes/setup.tsx` rendering the setup form.
- `/setup`, like `/login`, renders without the app chrome (`Chrome` in
  `__root.tsx` should treat `/setup` the same as `/login`).

### `/setup` page behavior

- Form fields: username, password, confirm password (client-side match check).
- Submit → `ApiClient.bootstrap(username, password)` (sends stored admin token).
- On success:
  1. **Clear the admin token from browser storage** (`clearToken`) — the host
     secret must not persist in the browser.
  2. Invalidate the `me` query.
  3. Redirect to `/login`; the operator logs in with the new username/password
     to obtain a normal session token.
- Error handling branches on `ApiError.status` in the mutation's error handler
  (`parseResponse` only special-cases 401/403; 409/400 surface as a generic
  `ApiError` carrying `status`):
  - `409` (already initialized) → redirect to `/login`.
  - `401` → show "missing/invalid admin token".
  - `400` → show the validation message (username/password rules).

## Error Handling Summary

| Condition | Backend response | Frontend handling |
|-----------|------------------|-------------------|
| Empty username / password < 8 | 400 | Show validation message |
| No/invalid token | 401 | Error message on `/setup` |
| Not super_admin | 403 | Error message |
| Already initialized | 409 Conflict | Redirect to `/login` |
| Success | 201 + `User` | Clear token, redirect to `/login` |

## Testing

### Backend (Rust)

- Bootstrap with admin token, fresh DB → creates super_admin user, sets flag,
  returns the user; `is_super_admin == true`.
- Second bootstrap call → 409.
- Bootstrap without a token → 401.
- Bootstrap as a non-super-admin principal → 403.
- Flag persistence: after bootstrap, delete all users, call bootstrap again →
  still 409 (the core reason for the flag).
- Migration: a v6 DB migrates to v7 and the `system_settings` table exists.
- `me()` returns `admin_initialized` reflecting the flag in both states.

### Frontend

- Token-in-URL is stored and stripped from the address bar.
- Routing selects `/setup` only in `Bootstrap && !admin_initialized`.
- Successful bootstrap clears the token and redirects to `/login`.
- Stale token URL when already initialized routes to `/login`, not `/setup`.

## Verification Commands

- `cargo build`
- `cargo test`
- `cargo fmt --all`
- `cargo clippy --all-targets --all-features`
- `cd web && pnpm typecheck && pnpm test`

## Files Touched (anticipated)

- `src/repo/sqlite/pool.rs` — migration v7.
- `src/repo/sqlite/users.rs` (or a settings module) — `is_admin_initialized`, `bootstrap_admin`.
- `src/api/bootstrap.rs` — new handler + router.
- `src/api/mod.rs`, `src/app.rs` — wire the route under authed.
- `src/domain/user.rs`, `src/api/auth.rs` — `admin_initialized` on `me()`.
- `web/src/effect/schema.ts`, `web/src/effect/api-client.ts` — `admin_initialized`, `bootstrap()`.
- `web/src/routes/__root.tsx`, `web/src/routes/setup.tsx`, `web/src/hooks/useAuth.ts` — routing + setup page + token-in-URL.
</content>
</invoke>
