# One-Time Admin Bootstrap Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a one-time `POST /v1/bootstrap` endpoint that creates the first super-admin (gated by the admin token, made permanent by a persistent flag), plus a `/setup` web page reached via a token-in-URL flow.

**Architecture:** A new `system_settings(key,value)` table (migration v7) stores `admin_initialized`. `SqliteUserRepo` gains `is_admin_initialized` + a transactional `bootstrap_admin`. A new `src/api/bootstrap.rs` handler is merged into the authed `/v1` router. `me()` gains an `admin_initialized` field (populated in both `me_handler` branches) so the SPA can route. The web console reads a `?token=…` query param, stores it, and a top-level gate routes to a new `/setup` page after `me()` resolves.

**Tech Stack:** Rust 2024, axum, rusqlite (single `Arc<Mutex<Connection>>`); TypeScript, TanStack Router/Query, Effect, Vitest.

**Spec:** `docs/superpowers/specs/2026-05-27-admin-bootstrap-design.md`

---

## File Structure

- `src/repo/sqlite/pool.rs` — add migration v7 (`system_settings` table).
- `src/repo/error.rs` — add `AdminAlreadyInitialized` variant.
- `src/repo/sqlite/users.rs` — add `is_admin_initialized` + `bootstrap_admin` to `SqliteUserRepo`.
- `src/api/error.rs` — map `RepoError::AdminAlreadyInitialized` → 409.
- `src/api/bootstrap.rs` — NEW: handler + router.
- `src/api/mod.rs`, `src/app.rs` — register the route.
- `src/domain/user.rs`, `src/api/auth.rs` — `admin_initialized` on `Me`, both branches.
- `tests/repo_contract.rs`, `tests/backend_contract.rs` — backend tests.
- `web/src/effect/schema.ts`, `web/src/effect/api-client.ts` — `admin_initialized` + `bootstrap()`.
- `web/src/effect/api-client.test.ts` — client test.
- `web/src/hooks/useAuth.ts`, `web/src/routes/__root.tsx`, `web/src/routes/setup.tsx` — token-in-URL + gate + page.

---

## Task 1: Migration v7 — `system_settings` table + `is_admin_initialized`

**Files:**
- Modify: `src/repo/sqlite/pool.rs` (after the `current < 6` block, before `Ok(())`)
- Modify: `src/repo/sqlite/users.rs` (add method to `impl SqliteUserRepo`)
- Test: `tests/repo_contract.rs`

- [ ] **Step 1: Write the failing test**

Add to `tests/repo_contract.rs`:

```rust
#[test]
fn admin_initialized_is_false_on_fresh_store() {
    let store = migrated_store();
    let users = SqliteUserRepo::new(store.pool());
    assert!(!users.is_admin_initialized().unwrap());
}
```

- [ ] **Step 2: Run test, verify it fails**

Run: `cargo test --test repo_contract admin_initialized_is_false_on_fresh_store`
Expected: FAIL — `no method named is_admin_initialized` (and/or `no such table`).

- [ ] **Step 3: Add migration v7**

In `src/repo/sqlite/pool.rs`, immediately after the `if current < 6 { … }` block:

```rust
    if current < 7 {
        connection.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS system_settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            "#,
        )?;
        connection.execute("DELETE FROM schema_version", [])?;
        connection.execute("INSERT INTO schema_version (version) VALUES (7)", [])?;
    }
```

- [ ] **Step 4: Add `is_admin_initialized` to `SqliteUserRepo`**

In `src/repo/sqlite/users.rs`, inside `impl SqliteUserRepo` (the second impl block, after `list_memberships_for_user`). `OptionalExtension` is already imported.

```rust
    pub fn is_admin_initialized(&self) -> Result<bool, RepoError> {
        let conn = self.pool.connection()?;
        let value: Option<String> = conn
            .query_row(
                "SELECT value FROM system_settings WHERE key = 'admin_initialized'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(value.as_deref() == Some("true"))
    }
```

- [ ] **Step 5: Run test, verify it passes**

Run: `cargo test --test repo_contract admin_initialized_is_false_on_fresh_store`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add src/repo/sqlite/pool.rs src/repo/sqlite/users.rs tests/repo_contract.rs
git commit -m "feat(repo): add system_settings table and is_admin_initialized"
```

---

## Task 2: `bootstrap_admin` repo method

**Files:**
- Modify: `src/repo/error.rs` (add variant)
- Modify: `src/repo/sqlite/users.rs` (add method)
- Test: `tests/repo_contract.rs`

- [ ] **Step 1: Write the failing tests**

Add to `tests/repo_contract.rs`:

```rust
#[test]
fn bootstrap_admin_creates_super_admin_and_sets_flag() {
    let store = migrated_store();
    let users = SqliteUserRepo::new(store.pool());

    let user = users.bootstrap_admin("root", "hash").unwrap();
    assert!(user.is_super_admin);
    assert_eq!(user.username, "root");
    assert!(users.is_admin_initialized().unwrap());
}

#[test]
fn bootstrap_admin_is_rejected_once_initialized_even_after_user_deletion() {
    let store = migrated_store();
    let users = SqliteUserRepo::new(store.pool());

    let user = users.bootstrap_admin("root", "hash").unwrap();
    // `delete_user_q` guards against removing the LAST super-admin, so seed a
    // second one before deleting the bootstrapped user. This proves the flag —
    // not the user count — is what blocks a second bootstrap.
    users.create_user("ops", "hash2", true).unwrap();
    users.delete_user(user.id).unwrap();

    let err = users.bootstrap_admin("root2", "hash3").unwrap_err();
    assert!(matches!(err, denia::repo::RepoError::AdminAlreadyInitialized));
    assert!(users.is_admin_initialized().unwrap());
}
```

> `delete_user_q` (`src/repo/sqlite/users.rs`) returns `RepoError::LastSuperAdmin` when deleting the only super-admin — that's why the test seeds `ops` first. Do NOT try to delete every user (the guard makes that impossible via the repo API); deleting the bootstrapped user while another admin remains is enough to prove the flag survives user deletion.

- [ ] **Step 2: Run tests, verify they fail**

Run: `cargo test --test repo_contract bootstrap_admin`
Expected: FAIL — `no method named bootstrap_admin` / `no variant AdminAlreadyInitialized`.

- [ ] **Step 3: Add the error variant**

In `src/repo/error.rs`, add to `enum RepoError`:

```rust
    #[error("admin already initialized")]
    AdminAlreadyInitialized,
```

- [ ] **Step 4: Add `bootstrap_admin`**

In `src/repo/sqlite/users.rs`, inside `impl SqliteUserRepo`. `create_user_q` is in the same module:

```rust
    pub fn bootstrap_admin(
        &self,
        username: &str,
        password_hash: &str,
    ) -> Result<User, RepoError> {
        let mut conn = self.pool.connection()?;
        let tx = conn.transaction()?;
        let already: Option<String> = tx
            .query_row(
                "SELECT value FROM system_settings WHERE key = 'admin_initialized'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if already.as_deref() == Some("true") {
            return Err(RepoError::AdminAlreadyInitialized);
        }
        let user = create_user_q(&tx, username, password_hash, true)?;
        tx.execute(
            "INSERT INTO system_settings (key, value) VALUES ('admin_initialized', 'true')",
            [],
        )?;
        tx.commit()?;
        Ok(user)
    }
```

> `conn.transaction()` needs `&mut` — that's why `conn` is `let mut`. `&tx` (`&Transaction`) deref-coerces to `&Connection` for `create_user_q`.

- [ ] **Step 5: Run tests, verify they pass**

Run: `cargo test --test repo_contract bootstrap_admin`
Expected: PASS (after resolving the `delete_user` note in Step 1 if needed).

- [ ] **Step 6: Commit**

```bash
git add src/repo/error.rs src/repo/sqlite/users.rs tests/repo_contract.rs
git commit -m "feat(repo): add transactional bootstrap_admin with one-time flag"
```

---

## Task 3: Map `AdminAlreadyInitialized` → 409

**Files:**
- Modify: `src/api/error.rs:99-110` (the `Self::Repo(error)` match arm)

- [ ] **Step 1: Add the mapping**

In the `Self::Repo(error) => match &error { … }` block in `src/api/error.rs`, add a branch alongside the others:

```rust
                RepoError::AdminAlreadyInitialized => (StatusCode::CONFLICT, error.to_string()),
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo build`
Expected: builds (no missing-match-arm error). Behavior is exercised by Task 4's HTTP tests.

- [ ] **Step 3: Commit**

```bash
git add src/api/error.rs
git commit -m "feat(api): map AdminAlreadyInitialized to 409 Conflict"
```

---

## Task 4: `POST /v1/bootstrap` endpoint

**Files:**
- Create: `src/api/bootstrap.rs`
- Modify: `src/api/mod.rs` (add `pub mod bootstrap;`)
- Modify: `src/app.rs:269-282` (add `.merge(api::bootstrap::router())`)
- Test: `tests/backend_contract.rs`

- [ ] **Step 1: Write the failing tests**

Add to `tests/backend_contract.rs` (mirror the existing `oneshot` style; `AppConfig::for_test("test-token")` makes `Bearer test-token` the admin token). Helper to read status + body is inline:

```rust
#[tokio::test]
async fn bootstrap_requires_admin_token() {
    let store = SqliteStore::open_in_memory().expect("open sqlite");
    store.migrate().expect("migrate");
    let app = build_router(AppState::new(AppConfig::for_test("test-token"), &store));

    let resp = app
        .oneshot(
            http::Request::builder()
                .method(http::Method::POST)
                .uri("/v1/bootstrap")
                .header(http::header::CONTENT_TYPE, "application/json")
                .body(axum::body::Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "username": "root", "password": "supersecret"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn bootstrap_creates_first_admin_then_conflicts() {
    let store = SqliteStore::open_in_memory().expect("open sqlite");
    store.migrate().expect("migrate");
    let app = build_router(AppState::new(AppConfig::for_test("test-token"), &store));

    let body = || {
        axum::body::Body::from(
            serde_json::to_vec(&serde_json::json!({
                "username": "root", "password": "supersecret"
            }))
            .unwrap(),
        )
    };
    let req = || {
        http::Request::builder()
            .method(http::Method::POST)
            .uri("/v1/bootstrap")
            .header(http::header::AUTHORIZATION, "Bearer test-token")
            .header(http::header::CONTENT_TYPE, "application/json")
            .body(body())
            .unwrap()
    };

    let first = app.clone().oneshot(req()).await.unwrap();
    assert_eq!(first.status(), http::StatusCode::CREATED);

    let second = app.oneshot(req()).await.unwrap();
    assert_eq!(second.status(), http::StatusCode::CONFLICT);
}

#[tokio::test]
async fn bootstrap_rejects_short_password() {
    let store = SqliteStore::open_in_memory().expect("open sqlite");
    store.migrate().expect("migrate");
    let app = build_router(AppState::new(AppConfig::for_test("test-token"), &store));

    let resp = app
        .oneshot(
            http::Request::builder()
                .method(http::Method::POST)
                .uri("/v1/bootstrap")
                .header(http::header::AUTHORIZATION, "Bearer test-token")
                .header(http::header::CONTENT_TYPE, "application/json")
                .body(axum::body::Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "username": "root", "password": "short"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), http::StatusCode::BAD_REQUEST);
}
```

- [ ] **Step 2: Run tests, verify they fail**

Run: `cargo test --test backend_contract bootstrap_`
Expected: FAIL — route returns 404 / module missing.

- [ ] **Step 3: Create the handler**

Create `src/api/bootstrap.rs` (model on `src/api/users.rs`):

```rust
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::post,
};
use serde::Deserialize;

use crate::api::ApiError;
use crate::app::AppState;
use crate::auth::Principal;

pub fn router() -> Router<AppState> {
    Router::new().route("/bootstrap", post(bootstrap_handler))
}

#[derive(Debug, Deserialize)]
struct BootstrapRequest {
    username: String,
    password: String,
}

async fn bootstrap_handler(
    State(state): State<AppState>,
    principal: Principal,
    Json(input): Json<BootstrapRequest>,
) -> Result<(StatusCode, Json<crate::domain::User>), ApiError> {
    if !principal.is_super_admin {
        return Err(ApiError::Forbidden("super admin required".to_string()));
    }
    if input.username.trim().is_empty() {
        return Err(ApiError::BadRequest("username required".to_string()));
    }
    if input.password.len() < 8 {
        return Err(ApiError::BadRequest(
            "password must be at least 8 characters".to_string(),
        ));
    }
    if state.users.is_admin_initialized()? {
        return Err(ApiError::Conflict("admin already initialized".to_string()));
    }
    let hash = crate::auth::hash_password(&input.password)?;
    let user = state.users.bootstrap_admin(&input.username, &hash)?;
    Ok((StatusCode::CREATED, Json(user)))
}
```

> `hash_password` lives at `crate::auth::hash_password` (`src/auth/credentials.rs`); it returns `Result<_, AuthError>` and `ApiError: From<AuthError>` exists. `User` has `#[serde(skip_serializing)]` on `password_hash`, so the response omits it.

- [ ] **Step 4: Register the module + route**

In `src/api/mod.rs`, add (keep alphabetical): `pub mod bootstrap;`

In `src/app.rs`, the `authed` chain starts `let authed = api::auth::router().merge(…)…`. Add a new `.merge` line anywhere in that chain (e.g. right after `api::users::router()`), before the closing `.route_layer(middleware::from_fn_with_state(state.clone(), require_auth))` — that `route_layer` is what enforces 401 on a missing/invalid token:

```rust
        .merge(api::bootstrap::router())
```

- [ ] **Step 5: Run tests, verify they pass**

Run: `cargo test --test backend_contract bootstrap_`
Expected: PASS (all three).

- [ ] **Step 6: Commit**

```bash
git add src/api/bootstrap.rs src/api/mod.rs src/app.rs tests/backend_contract.rs
git commit -m "feat(api): add one-time POST /v1/bootstrap endpoint"
```

---

## Task 5: `admin_initialized` on `me()`

**Files:**
- Modify: `src/domain/user.rs:70-75` (`Me` struct)
- Modify: `src/api/auth.rs:68-92` (`me_handler`, both branches)
- Test: `tests/backend_contract.rs`

- [ ] **Step 1: Write the failing test**

Add to `tests/backend_contract.rs`:

```rust
#[tokio::test]
async fn me_reports_admin_initialized_flag() {
    let store = SqliteStore::open_in_memory().expect("open sqlite");
    store.migrate().expect("migrate");
    let app = build_router(AppState::new(AppConfig::for_test("test-token"), &store));

    let me = |app: axum::Router| async move {
        let resp = app
            .oneshot(
                http::Request::builder()
                    .uri("/v1/me")
                    .header(http::header::AUTHORIZATION, "Bearer test-token")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()
    };

    let before = me(app.clone()).await;
    assert_eq!(before["admin_initialized"], serde_json::json!(false));

    app.clone()
        .oneshot(
            http::Request::builder()
                .method(http::Method::POST)
                .uri("/v1/bootstrap")
                .header(http::header::AUTHORIZATION, "Bearer test-token")
                .header(http::header::CONTENT_TYPE, "application/json")
                .body(axum::body::Body::from(
                    serde_json::to_vec(&serde_json::json!({
                        "username": "root", "password": "supersecret"
                    }))
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    let after = me(app).await;
    assert_eq!(after["admin_initialized"], serde_json::json!(true));
}
```

- [ ] **Step 2: Run test, verify it fails**

Run: `cargo test --test backend_contract me_reports_admin_initialized_flag`
Expected: FAIL — `admin_initialized` is `null` (field absent).

- [ ] **Step 3: Add the field to `Me`**

In `src/domain/user.rs`, the `Me` struct:

```rust
pub struct Me {
    pub principal: PrincipalView,
    pub is_super_admin: bool,
    pub admin_initialized: bool,
    pub memberships: Vec<ProjectMembership>,
}
```

- [ ] **Step 4: Populate it in both branches**

In `src/api/auth.rs` `me_handler`, compute once and set in both `Me { … }` literals:

```rust
async fn me_handler(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<Me>, ApiError> {
    let admin_initialized = state.users.is_admin_initialized()?;
    if principal.is_super_admin && !principal.is_authenticated() {
        return Ok(Json(Me {
            principal: PrincipalView::Bootstrap,
            is_super_admin: true,
            admin_initialized,
            memberships: vec![],
        }));
    }
    let user_id = principal
        .user_id
        .ok_or(ApiError::Conflict("no user".to_string()))?;
    let user = state
        .users
        .get_user(user_id)?
        .ok_or_else(|| ApiError::NotFound("user not found".to_string()))?;
    let memberships = state.users.list_memberships_for_user(user_id)?;
    Ok(Json(Me {
        principal: PrincipalView::User { user },
        is_super_admin: principal.is_super_admin,
        admin_initialized,
        memberships,
    }))
}
```

- [ ] **Step 5: Run test, verify it passes**

Run: `cargo test --test backend_contract me_reports_admin_initialized_flag`
Expected: PASS.

- [ ] **Step 6: Full backend gate + commit**

Run: `cargo test && cargo fmt --all && cargo clippy --all-targets --all-features`
Expected: all green.

```bash
git add src/domain/user.rs src/api/auth.rs tests/backend_contract.rs
git commit -m "feat(api): expose admin_initialized on /v1/me"
```

---

## Task 6: Frontend schema + API client `bootstrap()`

**Files:**
- Modify: `web/src/effect/schema.ts:46-50` (`Me` class)
- Modify: `web/src/effect/api-client.ts` (service type + impl + return object)
- Test: `web/src/effect/api-client.test.ts`

- [ ] **Step 1: Write the failing test**

In `web/src/effect/api-client.test.ts`, add a test that mocks a 201 `POST /v1/bootstrap` and asserts `bootstrap()` decodes the returned `User`. Follow the existing mocking pattern in that file (match how `login`/`createUser` are tested — read it first and mirror exactly). Assert `result.username === 'root'`.

- [ ] **Step 2: Run test, verify it fails**

Run: `cd web && pnpm test api-client`
Expected: FAIL — `bootstrap` not a property of `ApiClient`.

- [ ] **Step 3: Add `admin_initialized` to the `Me` schema**

In `web/src/effect/schema.ts`, the `Me` class:

```ts
export class Me extends Schema.Class<Me>('Me')({
  principal: PrincipalView,
  is_super_admin: Schema.Boolean,
  admin_initialized: Schema.Boolean,
  memberships: Schema.Array(Membership),
}) {}
```

- [ ] **Step 4: Add `bootstrap` to the `ApiClient` service**

In `web/src/effect/api-client.ts`: add to the service type (near `createUser`):

```ts
    readonly bootstrap: (
      username: string,
      password: string,
    ) => Effect.Effect<User, ApiError | DecodeError>
```

Add the implementation inside `ApiClientLive` (mirror `createUser`, which sends `authHeaders()`):

```ts
    const bootstrap = (username: string, password: string) =>
      Effect.gen(function* () {
        const response = yield* http
          .post(url('/v1/bootstrap'), {
            headers: {
              ...authHeaders(),
              'content-type': 'application/json',
            },
            body: jsonBody({ username, password }),
          })
          .pipe(Effect.mapError(httpError))
        return yield* parseResponse(response, User)
      })
```

Add `bootstrap,` to the returned object literal.

- [ ] **Step 5: Run test + typecheck, verify pass**

Run: `cd web && pnpm test api-client && pnpm typecheck`
Expected: PASS. (If `pnpm typecheck` flags the pre-existing `User.id: Schema.Number` vs UUID-string mismatch, that's out of scope — do not change it.)

- [ ] **Step 6: Commit**

```bash
git add web/src/effect/schema.ts web/src/effect/api-client.ts web/src/effect/api-client.test.ts
git commit -m "feat(web): add admin_initialized schema field and bootstrap client method"
```

---

## Task 7: Token-in-URL, routing gate, and `/setup` page

**Files:**
- Modify: `web/src/effect/auth-store.ts` (token-from-URL capture helper)
- Modify: `web/src/hooks/useAuth.ts` (expose `adminInitialized`)
- Modify: `web/src/routes/__root.tsx` (capture token before gate; allow `/setup`; mount gate; chrome-less `/setup`)
- Create: `web/src/routes/setup.tsx`

> This task is UI-heavy; verify in the browser (Step 5) since unit tests don't cover routing/redirects well.

- [ ] **Step 1: Capture the token from the URL**

In `web/src/effect/auth-store.ts`, add a one-shot capture that stores `?token=` and strips it from the address bar:

```ts
export function captureTokenFromUrl(): void {
  if (typeof window === 'undefined') return
  const params = new URLSearchParams(window.location.search)
  const token = params.get('token')
  if (token) {
    setToken(token)
    params.delete('token')
    const qs = params.toString()
    const url = window.location.pathname + (qs ? `?${qs}` : '') + window.location.hash
    window.history.replaceState({}, '', url)
  }
}
```

- [ ] **Step 2: Expose `adminInitialized` from `useAuth`**

In `web/src/hooks/useAuth.ts`, add to the returned object:

```ts
    adminInitialized: meQuery.data?.admin_initialized ?? false,
```

- [ ] **Step 3: Update `__root.tsx` — capture, gate, allow `/setup`**

In `web/src/routes/__root.tsx`:

1. Call `captureTokenFromUrl()` at module scope (top of file, after imports) so the token is in storage before `beforeLoad` runs.
2. In `beforeLoad`, treat `/setup` like `/login` (no auth redirect):

```ts
  beforeLoad: ({ location }) => {
    const isPublicRoute =
      location.pathname === '/login' || location.pathname === '/setup'
    if (!hasAuth() && !isPublicRoute) {
      throw redirect({ to: '/login' })
    }
    if (hasAuth() && location.pathname === '/login') {
      throw redirect({ to: '/' })
    }
  },
```

3. In `Chrome`, render `/setup` chrome-less like `/login`:

```ts
  if (pathname === '/login' || pathname === '/setup') {
    return <main id="main">{children}</main>
  }
```

4. Add a `BootstrapGate` component and render it inside `Chrome` (wrap `children`) so it runs after `me()` resolves. It only redirects; it never blocks `/login` or `/setup` rendering:

```tsx
import { useEffect } from 'react'
import { useRouter } from '@tanstack/react-router'
import { useAuth } from '../hooks/useAuth'

function BootstrapGate({ children }: { children: React.ReactNode }) {
  const router = useRouter()
  const pathname = useRouterState({ select: (s) => s.location.pathname })
  const { token, isLoading, isBootstrap, adminInitialized } = useAuth()

  useEffect(() => {
    if (!token || isLoading) return
    if (isBootstrap && !adminInitialized && pathname !== '/setup') {
      router.navigate({ to: '/setup' })
    } else if (isBootstrap && adminInitialized && pathname === '/setup') {
      router.navigate({ to: '/login' })
    }
  }, [token, isLoading, isBootstrap, adminInitialized, pathname, router])

  return <>{children}</>
}
```

Then wrap the `Chrome` body content with `<BootstrapGate>`.

> Adapt names/imports to the actual file; `useRouterState` is already imported. Keep the existing `hasAuth()` sync gate — it handles the no-token case; `BootstrapGate` handles the has-token bootstrap case.

- [ ] **Step 4: Create the `/setup` page**

Create `web/src/routes/setup.tsx` (file-based route, mirror `login.tsx` structure — read it first):

```tsx
import { createFileRoute, useRouter } from '@tanstack/react-router'
import { useState } from 'react'
import { useMutation } from '@tanstack/react-query'
import { Effect } from 'effect'
import { runQuery } from '../effect/runtime'
import { ApiClient } from '../effect/api-client'
import { clearToken } from '../effect/auth-store'

export const Route = createFileRoute('/setup')({
  component: SetupPage,
})

function SetupPage() {
  const router = useRouter()
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [confirm, setConfirm] = useState('')
  const [error, setError] = useState<string | null>(null)

  const mutation = useMutation({
    mutationFn: () =>
      runQuery(
        Effect.gen(function* () {
          const api = yield* ApiClient
          return yield* api.bootstrap(username, password)
        }),
      ),
    onSuccess: () => {
      clearToken()
      router.navigate({ to: '/login' })
    },
    onError: (e: unknown) => {
      const status = (e as { status?: number }).status
      if (status === 409) {
        clearToken()
        router.navigate({ to: '/login' })
      } else if (status === 401) {
        setError('Missing or invalid admin token. Reopen the console using the URL printed at launch.')
      } else {
        setError((e as { message?: string }).message ?? 'Setup failed')
      }
    },
  })

  const onSubmit = (ev: React.FormEvent) => {
    ev.preventDefault()
    setError(null)
    if (password !== confirm) {
      setError('Passwords do not match')
      return
    }
    mutation.mutate()
  }

  return (
    <form onSubmit={onSubmit}>
      <h1>Create the first admin</h1>
      {error && <p role="alert">{error}</p>}
      <input value={username} onChange={(e) => setUsername(e.target.value)} placeholder="username" autoComplete="username" />
      <input type="password" value={password} onChange={(e) => setPassword(e.target.value)} placeholder="password" autoComplete="new-password" />
      <input type="password" value={confirm} onChange={(e) => setConfirm(e.target.value)} placeholder="confirm password" autoComplete="new-password" />
      <button type="submit" disabled={mutation.isPending}>Create admin</button>
    </form>
  )
}
```

> The `ApiError` (`web/src/effect/errors.ts`) carries `status`; `runQuery` rejects with it, so `e.status` is readable in `onError`. Match the styling/markup conventions of `login.tsx`.

- [ ] **Step 5: Verify in the browser**

```bash
cd web && pnpm build && cd .. && cargo run
```

Then, with a fresh DB and the admin token from config:
- Open `http://127.0.0.1:7180/?token=<DENIA_ADMIN_TOKEN>` → URL token strips, redirects to `/setup`.
- Submit username + password (≥8, matching) → redirects to `/login`; `sessionStorage` no longer holds the token.
- Log in with the new credentials → reaches the app.
- Reopen `/?token=<token>` after setup → lands on `/login`, NOT `/setup`.

Confirm each behavior in the browser; note any that can't be verified.

- [ ] **Step 6: Frontend gate + commit**

Run: `cd web && pnpm typecheck && pnpm test`
Expected: green.

```bash
git add web/src/effect/auth-store.ts web/src/hooks/useAuth.ts web/src/routes/__root.tsx web/src/routes/setup.tsx
git commit -m "feat(web): add token-in-URL capture, bootstrap gate, and /setup page"
```

---

## Final Verification

- [ ] `cargo build`
- [ ] `cargo test`
- [ ] `cargo fmt --all`
- [ ] `cargo clippy --all-targets --all-features`
- [ ] `cd web && pnpm typecheck && pnpm test`
- [ ] Browser smoke test from Task 7 Step 5 passes.
</content>
