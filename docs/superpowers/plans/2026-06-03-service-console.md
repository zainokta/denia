# Service Console Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build an interactive service console for Denia that opens `/bin/sh` inside a selected live service replica from both the web UI and the cross-platform `denia console` CLI.

**Architecture:** The backend mints short-lived one-time console tickets through the normal bearer-auth API, then upgrades a websocket using the ticket. `LinuxRuntime` opens a PTY-backed console process by joining the tracked replica's namespaces and cgroup through a new syscall path, leaving the existing service/job launcher unchanged. Web and CLI clients both use the same ticket + websocket protocol.

**Tech Stack:** Rust 2024, Axum websocket support, Tokio, rustix/libc namespace + PTY syscalls, reqwest plus a websocket client for the CLI, React/TanStack Router, Effect schemas/API client, `@xterm/xterm`.

---

## File Structure

- Create `docs/adr/033-service-console.md`: accepted ADR for runtime/API/dependency decisions.
- Modify `docs/adr/README.md`: add ADR-033 to the index.
- Modify `Cargo.toml`: add server websocket support and client terminal/websocket dependencies.
- Create `src/domain/console.rs`: serializable API/domain types shared by handlers, runtime, tests, and client.
- Modify `src/domain/mod.rs`: export console types.
- Create `src/api/console.rs`: replica listing, ticket minting, websocket upgrade, session limits, audit metadata.
- Modify `src/api/mod.rs` and `src/app.rs`: register the console API and CSP websocket connect source.
- Modify `src/runtime/runtime_trait.rs`: add the console runtime method with a default unsupported implementation.
- Modify `src/runtime/mod.rs`: export console runtime module.
- Create `src/runtime/console.rs`: runtime console session stream types and helpers.
- Modify `src/runtime/linux.rs`: implement live console open against tracked replica state.
- Modify `src/runtime/fake.rs`: implement deterministic fake console support for API tests.
- Create `src/syscall/pty.rs`: PTY open, resize, raw fd ownership helpers.
- Modify `src/syscall/mod.rs`: export `pty`.
- Create `src/syscall/console.rs`: `setns` + PTY fork path for the console shell.
- Do not modify `src/syscall/ns.rs` or `spawn_namespaced_process`; console support uses new syscall modules.
- Create `src/cli/client/console.rs`: `denia console` command implementation.
- Modify `src/cli/client/http.rs`: add console replica/ticket API calls and websocket URL helper.
- Modify `src/cli/client/mod.rs` and `src/cli/mod.rs`: expose top-level client command in server and client-only builds.
- Modify `web/package.json` and `web/pnpm-lock.yaml`: add `@xterm/xterm`.
- Modify `web/src/effect/schema.ts` and `web/src/effect/api-client.ts`: add console schemas and methods.
- Create `web/src/components/ServiceConsole.tsx`: browser terminal component.
- Modify `web/src/routes/services/$serviceId.tsx`: add operator-only `console` tab.
- Modify `web/src/styles.css`: add terminal layout styles.
- Add or update tests:
  - `src/api/console.rs` unit tests.
  - `src/runtime/linux.rs` console planning/unit tests.
  - `src/syscall/pty.rs` unit tests.
  - `tests/client_console.rs` CLI tests.
  - `tests/linux_runtime_privileged.rs` ignored live console test.
  - `web/src/components/ServiceConsole.test.tsx`.
  - `web/src/routes/services/-detail.test.tsx`.

## Safety Notes

- GitNexus impact already checked:
  - `Runtime` trait: LOW; direct implementors are `LinuxRuntime` and `FakeRuntime`.
  - `spawn_namespaced_process`: CRITICAL; do not edit this function for console support.
  - `AppState`: CRITICAL; keep ticket/session stores module-local in `src/api/console.rs` instead of adding state fields.
- Run `gitnexus_impact` before editing every existing function or method named in a task below.
- Run `gitnexus_detect_changes({scope: "all"})` before committing implementation work.
- UUIDs must be UUIDv7. Use `uuid::Uuid::now_v7()` for console session ids.
- The console must never persist terminal input or output. Persist or log metadata only.

## Protocol Contract

Backend JSON:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsoleReplicaView {
    pub service_id: uuid::Uuid,
    pub service_name: String,
    pub deployment_id: uuid::Uuid,
    pub replica_index: u32,
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateConsoleTicketRequest {
    pub replica_index: u32,
    #[serde(default = "default_terminal_cols")]
    pub cols: u16,
    #[serde(default = "default_terminal_rows")]
    pub rows: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsoleTicketResponse {
    pub ticket: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub ws_path: String,
}
```

Websocket text frames:

```json
{ "type": "ready", "session_id": "0197...", "replica_index": 0, "cols": 120, "rows": 32 }
{ "type": "resize", "cols": 100, "rows": 30 }
{ "type": "exit", "code": 0 }
{ "type": "error", "message": "console shell exited before ready" }
{ "type": "close" }
```

Websocket binary frames:

- Client to server: raw terminal input bytes.
- Server to client: raw PTY output bytes.

CLI surface:

```bash
denia console [SERVICE] --project <PROJECT> --replica <INDEX>
denia console
```

Resolution rules:

- `denia console` with no service reads `.denia`, resolves `.project` and `.service`, then uses the active profile.
- `SERVICE` may be a service id or a service name.
- Service names require a project context from `--project` or `.denia`.
- If exactly one running replica exists, `--replica` defaults to it.
- If multiple running replicas exist and `--replica` is absent, print the replica list and exit non-zero.

## Task 1: ADR And Dependency Contract

**Files:**
- Create: `docs/adr/033-service-console.md`
- Modify: `docs/adr/README.md`
- Modify: `Cargo.toml`
- Modify: `web/package.json`

- [ ] **Step 1: Write ADR-033**

Create `docs/adr/033-service-console.md` with this content:

```markdown
# ADR-033: Service Console

- Status: Accepted
- Date: 2026-06-03
- Related: ADR-003, ADR-005, ADR-019, ADR-024, ADR-030

## Context

Operators need an interactive way to inspect a deployed service from inside Denia's Linux runtime isolation. The console must behave like an exec session into a running service replica, not a Docker shell, SSH session, or diagnostic clone. Denia owns service runtime isolation and must preserve per-service namespace, cgroup, filesystem, and auth boundaries.

## Decision

Denia will add a live service console exposed through the management API, web console, and cross-platform `denia console` client command.

- The console attaches to a selected live replica of the service's promoted deployment.
- The runtime launches `/bin/sh` through a new PTY-backed `setns` path that joins the target replica's namespaces and cgroup.
- The existing `spawn_namespaced_process` service/job launcher remains unchanged.
- The browser and CLI first create a short-lived single-use console ticket through bearer-authenticated HTTP, then open a websocket using that ticket.
- Websocket binary frames carry terminal input/output bytes. Text JSON frames carry readiness, resize, exit, close, and error control messages.
- Console sessions are limited process-wide and per service.
- Denia records metadata-only audit events: user/principal, service id, deployment id, replica index, session id, start/end time, and exit reason.
- Denia does not persist terminal input or output.
- `/bin/sh` is the only v1 shell. Images without `/bin/sh` return a clear console error.

## Consequences

- Operators can inspect environment, files, process state, and runtime behavior from the service's actual sandbox.
- Browser websocket auth does not expose bearer tokens in URLs.
- CLI users get a `kubectl exec`-style workflow while retaining the ADR-030 client/server split.
- Runtime code gains a new privileged syscall surface for PTY and namespace joining, requiring normal unit tests plus gated privileged tests.
- Distroless images without `/bin/sh` cannot use v1 console until Denia adds an explicit command mode.

## Alternatives Considered

- Diagnostic clone console: rejected because it is not the live service instance.
- Token in websocket query string: rejected because bearer tokens can leak through URLs.
- Full transcript persistence: rejected because console output can include secrets.
- Docker/containerd/runc exec: rejected by Denia's runtime architecture.

## References

- ADR-003: Linux Runtime Process Runner
- ADR-005: Runtime Security Hardening
- ADR-019: Per-Replica Runtime Filesystem Isolation
- ADR-030: Cross-Platform Client CLI
```

- [ ] **Step 2: Update ADR index**

Add this row to `docs/adr/README.md` after ADR-032:

```markdown
| [033](033-service-console.md) | Service Console | Accepted | 2026-06-03 |
```

- [ ] **Step 3: Add backend dependencies**

Modify `Cargo.toml`:

```toml
axum = { version = "0.8", features = ["ws"], optional = true }
crossterm = { version = "0.29", optional = true }
futures-util = { version = "0.3", optional = true }
tokio-tungstenite = { version = "0.28", optional = true, default-features = false, features = ["rustls-tls-webpki-roots"] }
```

Add the new optional deps to the feature sets:

```toml
client = [
    "dep:crossterm",
    "dep:futures-util",
    "dep:tokio-tungstenite",
]
server = [
    # existing entries stay in place
    "dep:crossterm",
    "dep:futures-util",
    "dep:tokio-tungstenite",
]
```

Keep `uuid = { version = "1", features = ["serde", "v7"], optional = true }` unchanged.

- [ ] **Step 4: Add web dependency**

Run from `web/`:

```bash
pnpm add @xterm/xterm
```

Expected: `web/package.json` and `web/pnpm-lock.yaml` update.

- [ ] **Step 5: Verify dependency metadata**

Run:

```bash
cargo check --no-default-features --features client
cargo check
cd web && pnpm typecheck
```

Expected:

- client-only Rust build succeeds on the new client dependency set.
- server Rust build succeeds.
- web typecheck may still fail if the package install changed lock metadata but no imports exist yet; if it fails only because no source imports reference `@xterm/xterm`, continue to Task 7.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml web/package.json web/pnpm-lock.yaml docs/adr/README.md docs/adr/033-service-console.md
git commit -m "docs: add service console adr"
```

## Task 2: Domain Types And Runtime Trait

**Files:**
- Create: `src/domain/console.rs`
- Modify: `src/domain/mod.rs`
- Create: `src/runtime/console.rs`
- Modify: `src/runtime/mod.rs`
- Modify: `src/runtime/runtime_trait.rs`
- Modify: `src/runtime/fake.rs`

- [ ] **Step 1: Run impact checks**

Run:

```bash
gitnexus_impact({target: "Runtime", direction: "upstream", repo: "denia"})
gitnexus_impact({target: "FakeRuntime", direction: "upstream", repo: "denia"})
```

Expected:

- `Runtime` impact LOW with direct implementors `LinuxRuntime` and `FakeRuntime`.
- No HIGH or CRITICAL warning for `FakeRuntime`.

- [ ] **Step 2: Add console domain types**

Create `src/domain/console.rs`:

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const DEFAULT_TERMINAL_COLS: u16 = 120;
pub const DEFAULT_TERMINAL_ROWS: u16 = 32;
pub const MAX_TERMINAL_COLS: u16 = 300;
pub const MAX_TERMINAL_ROWS: u16 = 120;

pub fn default_terminal_cols() -> u16 {
    DEFAULT_TERMINAL_COLS
}

pub fn default_terminal_rows() -> u16 {
    DEFAULT_TERMINAL_ROWS
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsoleReplicaView {
    pub service_id: Uuid,
    pub service_name: String,
    pub deployment_id: Uuid,
    pub replica_index: u32,
    pub state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateConsoleTicketRequest {
    pub replica_index: u32,
    #[serde(default = "default_terminal_cols")]
    pub cols: u16,
    #[serde(default = "default_terminal_rows")]
    pub rows: u16,
}

impl CreateConsoleTicketRequest {
    pub fn normalized(mut self) -> Self {
        self.cols = self.cols.clamp(1, MAX_TERMINAL_COLS);
        self.rows = self.rows.clamp(1, MAX_TERMINAL_ROWS);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsoleTicketResponse {
    pub ticket: String,
    pub expires_at: DateTime<Utc>,
    pub ws_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ConsoleControlFrame {
    Ready {
        session_id: Uuid,
        replica_index: u32,
        cols: u16,
        rows: u16,
    },
    Resize {
        cols: u16,
        rows: u16,
    },
    Exit {
        code: Option<i32>,
    },
    Error {
        message: String,
    },
    Close,
}
```

- [ ] **Step 3: Export domain module**

In `src/domain/mod.rs`, add:

```rust
pub mod console;
pub use console::{
    ConsoleControlFrame, ConsoleReplicaView, ConsoleTicketResponse,
    CreateConsoleTicketRequest, DEFAULT_TERMINAL_COLS, DEFAULT_TERMINAL_ROWS,
    MAX_TERMINAL_COLS, MAX_TERMINAL_ROWS,
};
```

- [ ] **Step 4: Add runtime console session types**

Create `src/runtime/console.rs`:

```rust
use std::path::PathBuf;
use tokio::io::{AsyncRead, AsyncWrite};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct RuntimeConsoleRequest {
    pub session_id: Uuid,
    pub service_id: Uuid,
    pub service_name: String,
    pub deployment_id: Uuid,
    pub replica_index: u32,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug)]
pub struct RuntimeConsoleSession {
    pub session_id: Uuid,
    pub replica_index: u32,
    pub child_pid: u32,
    pub cgroup_path: PathBuf,
    pub pty: Box<dyn ConsolePty>,
}

pub trait ConsolePty: AsyncRead + AsyncWrite + Unpin + Send {
    fn resize(&self, cols: u16, rows: u16) -> std::io::Result<()>;
}

impl<T> ConsolePty for T
where
    T: AsyncRead + AsyncWrite + Unpin + Send,
{
    fn resize(&self, _cols: u16, _rows: u16) -> std::io::Result<()> {
        Ok(())
    }
}
```

- [ ] **Step 5: Export runtime console module**

In `src/runtime/mod.rs`, add:

```rust
pub mod console;
pub use console::{RuntimeConsoleRequest, RuntimeConsoleSession};
```

- [ ] **Step 6: Extend Runtime trait**

Modify `src/runtime/runtime_trait.rs` imports:

```rust
use crate::runtime::console::{RuntimeConsoleRequest, RuntimeConsoleSession};
```

Add this default method to `Runtime`:

```rust
async fn open_console(
    &self,
    _request: RuntimeConsoleRequest,
) -> Result<RuntimeConsoleSession, RuntimeError> {
    Err(RuntimeError::InvalidServiceName {
        name: "open_console not implemented".to_string(),
    })
}
```

Add the forwarding implementation to `impl<T> Runtime for Arc<T>`:

```rust
async fn open_console(
    &self,
    request: RuntimeConsoleRequest,
) -> Result<RuntimeConsoleSession, RuntimeError> {
    (**self).open_console(request).await
}
```

- [ ] **Step 7: Implement fake runtime console**

Modify `src/runtime/fake.rs`:

```rust
use tokio::io::DuplexStream;

use crate::runtime::console::{RuntimeConsoleRequest, RuntimeConsoleSession};

async fn open_console(
    &self,
    request: RuntimeConsoleRequest,
) -> Result<RuntimeConsoleSession, RuntimeError> {
    let (_client, server): (DuplexStream, DuplexStream) = tokio::io::duplex(4096);
    Ok(RuntimeConsoleSession {
        session_id: request.session_id,
        replica_index: request.replica_index,
        child_pid: 4321,
        cgroup_path: "/sys/fs/cgroup/denia/fake".into(),
        pty: Box::new(server),
    })
}
```

Place the method inside `impl Runtime for FakeRuntime`.

- [ ] **Step 8: Run tests**

Run:

```bash
cargo test runtime::fake
cargo test runtime::runtime_trait
```

Expected:

- Existing fake runtime tests pass.
- If no runtime trait tests exist, cargo reports zero filtered tests without compile errors.

- [ ] **Step 9: Commit**

```bash
git add src/domain/console.rs src/domain/mod.rs src/runtime/console.rs src/runtime/mod.rs src/runtime/runtime_trait.rs src/runtime/fake.rs
git commit -m "feat(runtime): add console runtime contract"
```

## Task 3: Console API Tickets And Replica Discovery

**Files:**
- Create: `src/api/console.rs`
- Modify: `src/api/mod.rs`
- Modify: `src/app.rs`
- Test: `src/api/console.rs`

- [ ] **Step 1: Run impact checks**

Run:

```bash
gitnexus_impact({target: "build_router", direction: "upstream", repo: "denia"})
gitnexus_impact({target: "security_headers", direction: "upstream", repo: "denia"})
```

Expected: no HIGH/CRITICAL warning. If GitNexus reports HIGH/CRITICAL, stop and report it before editing.

- [ ] **Step 2: Create API module skeleton**

Create `src/api/console.rs` with:

```rust
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use axum::{
    Json, Router,
    extract::{Path, State, WebSocketUpgrade, ws::{Message, WebSocket}},
    response::IntoResponse,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use rand::distr::{Alphanumeric, SampleString};
use serde::Serialize;
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::api::ApiError;
use crate::app::AppState;
use crate::auth::{Principal, ensure_role};
use crate::domain::{
    ConsoleControlFrame, ConsoleReplicaView, ConsoleTicketResponse,
    CreateConsoleTicketRequest, Role,
};
use crate::runtime::RuntimeConsoleRequest;

const TICKET_TTL: Duration = Duration::from_secs(30);
const MAX_CONSOLE_SESSIONS: usize = 16;
const MAX_CONSOLE_SESSIONS_PER_SERVICE: usize = 2;

static TICKETS: LazyLock<Arc<Mutex<HashMap<String, ConsoleTicket>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(HashMap::new())));
static CONSOLE_LIMIT: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(MAX_CONSOLE_SESSIONS)));
static SERVICE_LIMITS: LazyLock<Arc<Mutex<HashMap<Uuid, Arc<Semaphore>>>>> =
    LazyLock::new(|| Arc::new(Mutex::new(HashMap::new())));

#[derive(Debug, Clone)]
struct ConsoleTicket {
    service_id: Uuid,
    service_name: String,
    deployment_id: Uuid,
    replica_index: u32,
    principal_label: String,
    cols: u16,
    rows: u16,
    expires_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
struct ConsoleError {
    error: String,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/services/{service_id}/console/replicas", get(list_console_replicas))
        .route("/services/{service_id}/console/tickets", post(create_console_ticket))
        .route("/services/{service_id}/console/ws", get(console_ws))
}
```

- [ ] **Step 3: Add replica listing handler**

Append to `src/api/console.rs`:

```rust
async fn list_console_replicas(
    State(state): State<AppState>,
    principal: Principal,
    Path(service_id): Path<Uuid>,
) -> Result<Json<Vec<ConsoleReplicaView>>, ApiError> {
    let service = state
        .services
        .get_service(service_id)?
        .ok_or_else(|| ApiError::NotFound("service not found".to_string()))?;
    ensure_role(&state, &principal, service.project_id, Role::Operator)?;

    let promoted = state
        .deployments
        .promoted_deployment(service.id)?
        .ok_or_else(|| ApiError::Conflict("service has no promoted deployment".to_string()))?;

    let running = state.runtime.list_running().await?;
    let replicas = running
        .into_iter()
        .filter(|status| status.service_id == service.id && status.deployment_id == promoted)
        .map(|status| ConsoleReplicaView {
            service_id: status.service_id,
            service_name: status.service_name,
            deployment_id: status.deployment_id,
            replica_index: status.replica_index,
            state: status.state,
        })
        .collect();
    Ok(Json(replicas))
}
```

- [ ] **Step 4: Add ticket creation handler**

Append:

```rust
async fn create_console_ticket(
    State(state): State<AppState>,
    principal: Principal,
    Path(service_id): Path<Uuid>,
    Json(input): Json<CreateConsoleTicketRequest>,
) -> Result<Json<ConsoleTicketResponse>, ApiError> {
    let input = input.normalized();
    let service = state
        .services
        .get_service(service_id)?
        .ok_or_else(|| ApiError::NotFound("service not found".to_string()))?;
    ensure_role(&state, &principal, service.project_id, Role::Operator)?;

    let promoted = state
        .deployments
        .promoted_deployment(service.id)?
        .ok_or_else(|| ApiError::Conflict("service has no promoted deployment".to_string()))?;

    let running = state.runtime.list_running().await?;
    let target = running
        .into_iter()
        .find(|status| {
            status.service_id == service.id
                && status.deployment_id == promoted
                && status.replica_index == input.replica_index
        })
        .ok_or_else(|| ApiError::Conflict("selected replica is not running".to_string()))?;

    let ticket = Alphanumeric.sample_string(&mut rand::rng(), 48);
    let expires_at = Utc::now() + chrono::TimeDelta::seconds(TICKET_TTL.as_secs() as i64);
    let principal_label = match principal.user_id {
        Some(user_id) => user_id.to_string(),
        None => "super-admin-token".to_string(),
    };
    let value = ConsoleTicket {
        service_id: service.id,
        service_name: service.name,
        deployment_id: target.deployment_id,
        replica_index: target.replica_index,
        principal_label,
        cols: input.cols,
        rows: input.rows,
        expires_at,
    };

    TICKETS
        .lock()
        .map_err(|_| ApiError::Conflict("console ticket store unavailable".to_string()))?
        .insert(ticket.clone(), value);

    Ok(Json(ConsoleTicketResponse {
        ws_path: format!("/v1/services/{service_id}/console/ws?ticket={ticket}"),
        ticket,
        expires_at,
    }))
}
```

- [ ] **Step 5: Add ticket consume helpers**

Append:

```rust
fn consume_ticket(service_id: Uuid, ticket: &str) -> Result<ConsoleTicket, ApiError> {
    let mut tickets = TICKETS
        .lock()
        .map_err(|_| ApiError::Conflict("console ticket store unavailable".to_string()))?;
    let ticket_value = tickets
        .remove(ticket)
        .ok_or_else(|| ApiError::Unauthorized("invalid console ticket".to_string()))?;
    if ticket_value.service_id != service_id {
        return Err(ApiError::Unauthorized("console ticket service mismatch".to_string()));
    }
    if ticket_value.expires_at <= Utc::now() {
        return Err(ApiError::Unauthorized("console ticket expired".to_string()));
    }
    Ok(ticket_value)
}

fn service_limit(service_id: Uuid) -> Result<Arc<Semaphore>, ApiError> {
    let mut limits = SERVICE_LIMITS
        .lock()
        .map_err(|_| ApiError::Conflict("console service limit unavailable".to_string()))?;
    Ok(limits
        .entry(service_id)
        .or_insert_with(|| Arc::new(Semaphore::new(MAX_CONSOLE_SESSIONS_PER_SERVICE)))
        .clone())
}
```

- [ ] **Step 6: Add websocket handler**

Append:

```rust
async fn console_ws(
    State(state): State<AppState>,
    Path(service_id): Path<Uuid>,
    axum::extract::Query(params): axum::extract::Query<HashMap<String, String>>,
    ws: WebSocketUpgrade,
) -> Result<impl IntoResponse, ApiError> {
    let ticket = params
        .get("ticket")
        .ok_or_else(|| ApiError::Unauthorized("missing console ticket".to_string()))?;
    let ticket = consume_ticket(service_id, ticket)?;
    let global_permit = CONSOLE_LIMIT
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::TooManyRequests("too many console sessions".to_string()))?;
    let service_permit = service_limit(service_id)?
        .try_acquire_owned()
        .map_err(|_| ApiError::TooManyRequests("too many console sessions for service".to_string()))?;

    Ok(ws.on_upgrade(move |socket| async move {
        let _global_permit = global_permit;
        let _service_permit = service_permit;
        handle_console_socket(state, socket, ticket).await;
    }))
}

async fn handle_console_socket(state: AppState, mut socket: WebSocket, ticket: ConsoleTicket) {
    let session_id = Uuid::now_v7();
    tracing::info!(
        %session_id,
        service_id = %ticket.service_id,
        deployment_id = %ticket.deployment_id,
        replica_index = ticket.replica_index,
        principal = %ticket.principal_label,
        "console session start"
    );

    let request = RuntimeConsoleRequest {
        session_id,
        service_id: ticket.service_id,
        service_name: ticket.service_name.clone(),
        deployment_id: ticket.deployment_id,
        replica_index: ticket.replica_index,
        cols: ticket.cols,
        rows: ticket.rows,
    };

    let session = match state.runtime.open_console(request).await {
        Ok(session) => session,
        Err(error) => {
            let _ = socket
                .send(Message::Text(
                    serde_json::to_string(&ConsoleControlFrame::Error {
                        message: error.to_string(),
                    })
                    .unwrap_or_else(|_| "{\"type\":\"error\",\"message\":\"console failed\"}".to_string())
                    .into(),
                ))
                .await;
            return;
        }
    };

    let _ = socket
        .send(Message::Text(
            serde_json::to_string(&ConsoleControlFrame::Ready {
                session_id,
                replica_index: ticket.replica_index,
                cols: ticket.cols,
                rows: ticket.rows,
            })
            .unwrap_or_else(|_| "{\"type\":\"ready\"}".to_string())
            .into(),
        ))
        .await;

    bridge_console_socket(socket, session).await;

    tracing::info!(
        %session_id,
        service_id = %ticket.service_id,
        deployment_id = %ticket.deployment_id,
        replica_index = ticket.replica_index,
        principal = %ticket.principal_label,
        "console session end"
    );
}

async fn bridge_console_socket(
    _socket: WebSocket,
    _session: crate::runtime::RuntimeConsoleSession,
) {
    // Task 5 replaces this body with the bidirectional PTY bridge after the
    // runtime session object has a real PTY implementation.
}
```

- [ ] **Step 7: Register API module**

In `src/api/mod.rs`, add:

```rust
pub mod console;
```

In `src/app.rs`, add to the authenticated router chain:

```rust
.merge(api::console::router())
```

Update CSP in `security_headers`:

```rust
"default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self' ws: wss:; frame-ancestors 'none'; base-uri 'self'; form-action 'self'"
```

- [ ] **Step 8: Add API tests**

In `src/api/console.rs`, add:

```rust
#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use crate::app::{AppState, build_router};
    use crate::artifacts::{ArtifactKind, ArtifactRecord};
    use crate::config::AppConfig;
    use crate::domain::{
        DeploymentRequest, HealthCheck, Role, RuntimeStartRequest, ServiceConfig,
        ServiceSource, ExternalImageSource,
    };

    const ADMIN_TOKEN: &str = "test-admin-token-0123456789abcdef";

    fn test_state() -> AppState {
        AppState::builder(AppConfig::for_test(ADMIN_TOKEN)).build()
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn service_body(project_id: uuid::Uuid) -> ServiceConfig {
        ServiceConfig::new(
            project_id,
            "web",
            Vec::new(),
            ServiceSource::ExternalImage(ExternalImageSource {
                image: "busybox".to_string(),
                credential: None,
                registry_id: None,
                image_ref: None,
            }),
            8080,
            HealthCheck::new("/", 5),
            None,
            Vec::new(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn operator_can_create_console_ticket_for_running_replica() {
        let state = test_state();
        let project_id = state.projects.default_project_id().unwrap();
        let service = state.services.put_service(service_body(project_id)).unwrap();
        let deployment = state
            .deployments
            .create_deployment(DeploymentRequest::external_image(service.id, "busybox"))
            .unwrap();
        state.deployments.promote_deployment(service.id, deployment.id).unwrap();
        let artifact = ArtifactRecord::new("sha256:test".to_string(), ArtifactKind::RootfsBundle).unwrap();
        let _ = state
            .runtime
            .start(RuntimeStartRequest {
                service_name: service.name.clone(),
                service_id: service.id,
                deployment_id: deployment.id,
                artifact,
                internal_port: 8080,
                socket_path: "/tmp/test.sock".into(),
                cpu_millis: 100,
                memory_bytes: 64 * 1024 * 1024,
                env: Vec::new(),
                pids_max: None,
                memory_swap_max: None,
                io_weight: None,
                replica_index: 0,
            })
            .await
            .unwrap();

        let resp = build_router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/services/{}/console/tickets", service.id))
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"replica_index":0}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert!(body["ticket"].as_str().unwrap().len() >= 32);
        assert_eq!(
            body["ws_path"].as_str().unwrap(),
            format!("/v1/services/{}/console/ws?ticket={}", service.id, body["ticket"].as_str().unwrap())
        );
    }

    #[tokio::test]
    async fn viewer_cannot_create_console_ticket() {
        let state = test_state();
        let project_id = state.projects.default_project_id().unwrap();
        let viewer = state.users.create_user("viewer", "hash", false).unwrap();
        state.users.set_membership(viewer.id, project_id, Role::Viewer).unwrap();
        let viewer_token = state.tokens.create_api_token(viewer.id, "viewer").unwrap().token;
        let service = state.services.put_service(service_body(project_id)).unwrap();

        let resp = build_router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/services/{}/console/tickets", service.id))
                    .header("Authorization", format!("Bearer {viewer_token}"))
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"replica_index":0}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn stopped_service_returns_conflict_for_console_ticket() {
        let state = test_state();
        let project_id = state.projects.default_project_id().unwrap();
        let service = state.services.put_service(service_body(project_id)).unwrap();

        let resp = build_router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/services/{}/console/tickets", service.id))
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"replica_index":0}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }
}
```

- [ ] **Step 9: Run tests**

Run:

```bash
cargo test api::console
cargo test api::services::service_logs_stream_returns_sse
```

Expected:

- Console API tests pass.
- Existing service log stream test still passes.

- [ ] **Step 10: Commit**

```bash
git add src/api/console.rs src/api/mod.rs src/app.rs
git commit -m "feat(api): add service console tickets"
```

## Task 4: PTY And Live Replica Console Runtime

**Files:**
- Create: `src/syscall/pty.rs`
- Create: `src/syscall/console.rs`
- Modify: `src/syscall/mod.rs`
- Modify: `src/runtime/linux.rs`
- Modify: `src/runtime/console.rs`
- Test: `src/syscall/pty.rs`, `src/syscall/console.rs`, `src/runtime/linux.rs`, `tests/linux_runtime_privileged.rs`

- [ ] **Step 1: Run impact checks**

Run:

```bash
gitnexus_impact({target: "LinuxRuntime", direction: "upstream", repo: "denia"})
gitnexus_impact({target: "TrackedChild", direction: "upstream", repo: "denia"})
gitnexus_impact({target: "spawn_namespaced_process", direction: "upstream", repo: "denia"})
```

Expected:

- `spawn_namespaced_process` reports CRITICAL. Do not modify it.
- Any existing function you edit in `src/runtime/linux.rs` has its own impact checked before editing.

- [ ] **Step 2: Add PTY helper**

Create `src/syscall/pty.rs`:

```rust
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

use tokio::io::unix::AsyncFd;

#[derive(Debug)]
pub struct PtyMaster {
    inner: AsyncFd<OwnedFd>,
}

impl PtyMaster {
    pub fn new(fd: OwnedFd) -> io::Result<Self> {
        set_nonblocking(fd.as_raw_fd())?;
        Ok(Self {
            inner: AsyncFd::new(fd)?,
        })
    }

    pub fn raw_fd(&self) -> RawFd {
        self.inner.get_ref().as_raw_fd()
    }

    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        let winsize = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let rc = unsafe { libc::ioctl(self.raw_fd(), libc::TIOCSWINSZ, &winsize) };
        if rc == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl tokio::io::AsyncRead for PtyMaster {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        loop {
            let mut guard = match self.inner.poll_read_ready(cx) {
                std::task::Poll::Ready(result) => result?,
                std::task::Poll::Pending => return std::task::Poll::Pending,
            };
            let dst = buf.initialize_unfilled();
            match guard.try_io(|inner| {
                let n = unsafe {
                    libc::read(
                        inner.get_ref().as_raw_fd(),
                        dst.as_mut_ptr().cast(),
                        dst.len(),
                    )
                };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            }) {
                Ok(Ok(n)) => {
                    buf.advance(n);
                    return std::task::Poll::Ready(Ok(()));
                }
                Ok(Err(error)) => return std::task::Poll::Ready(Err(error)),
                Err(_would_block) => continue,
            }
        }
    }
}

impl tokio::io::AsyncWrite for PtyMaster {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<io::Result<usize>> {
        loop {
            let mut guard = match self.inner.poll_write_ready(cx) {
                std::task::Poll::Ready(result) => result?,
                std::task::Poll::Pending => return std::task::Poll::Pending,
            };
            match guard.try_io(|inner| {
                let n = unsafe {
                    libc::write(
                        inner.get_ref().as_raw_fd(),
                        bytes.as_ptr().cast(),
                        bytes.len(),
                    )
                };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            }) {
                Ok(result) => return std::task::Poll::Ready(result),
                Err(_would_block) => continue,
            }
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

pub fn open_pty(cols: u16, rows: u16) -> io::Result<(PtyMaster, OwnedFd)> {
    let master = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) };
    if master < 0 {
        return Err(io::Error::last_os_error());
    }
    let master = unsafe { OwnedFd::from_raw_fd(master) };
    if unsafe { libc::grantpt(master.as_raw_fd()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::unlockpt(master.as_raw_fd()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    let slave_name = pts_name(master.as_raw_fd())?;
    let slave = unsafe {
        libc::open(
            slave_name.as_ptr(),
            libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
        )
    };
    if slave < 0 {
        return Err(io::Error::last_os_error());
    }
    let pty = PtyMaster::new(master)?;
    pty.resize(cols, rows)?;
    Ok((pty, unsafe { OwnedFd::from_raw_fd(slave) }))
}

fn pts_name(fd: RawFd) -> io::Result<std::ffi::CString> {
    let mut buf = vec![0_i8; 128];
    let rc = unsafe { libc::ptsname_r(fd, buf.as_mut_ptr(), buf.len()) };
    if rc != 0 {
        return Err(io::Error::from_raw_os_error(rc));
    }
    let len = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    let bytes = buf[..len].iter().map(|b| *b as u8).collect::<Vec<_>>();
    std::ffi::CString::new(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "pty name contained nul"))
}

fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
```

- [ ] **Step 3: Add PTY tests**

Append to `src/syscall/pty.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_pty_creates_master_and_slave() {
        let (master, slave) = open_pty(80, 24).unwrap();
        assert!(master.raw_fd() >= 0);
        assert!(slave.as_raw_fd() >= 0);
    }

    #[test]
    fn resize_accepts_valid_dimensions() {
        let (master, _slave) = open_pty(80, 24).unwrap();
        master.resize(120, 32).unwrap();
    }
}
```

- [ ] **Step 4: Add syscall console launcher**

Create `src/syscall/console.rs`:

```rust
use std::ffi::CString;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::path::{Path, PathBuf};

use crate::syscall::SyscallError;

#[derive(Debug, Clone)]
pub struct ConsoleLaunchConfig {
    pub target_pid: u32,
    pub cgroup_path: PathBuf,
    pub rootfs: PathBuf,
    pub workdir: String,
    pub env: Vec<(String, String)>,
    pub shell: String,
}

pub fn spawn_console_process(
    config: &ConsoleLaunchConfig,
    slave: OwnedFd,
) -> Result<u32, SyscallError> {
    validate_console_config(config)?;
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(SyscallError::Io(std::io::Error::last_os_error()));
    }
    if pid == 0 {
        unsafe {
            child_exec_console(config, slave.as_raw_fd());
        }
    }
    drop(slave);
    Ok(pid as u32)
}

fn validate_console_config(config: &ConsoleLaunchConfig) -> Result<(), SyscallError> {
    if config.target_pid == 0 {
        return Err(SyscallError::Capability("target pid must be non-zero".to_string()));
    }
    if !config.rootfs.is_absolute() {
        return Err(SyscallError::Capability("rootfs must be absolute".to_string()));
    }
    if !config.cgroup_path.is_absolute() {
        return Err(SyscallError::Capability("cgroup path must be absolute".to_string()));
    }
    if config.shell != "/bin/sh" {
        return Err(SyscallError::Capability("console shell must be /bin/sh".to_string()));
    }
    Ok(())
}

unsafe fn child_exec_console(config: &ConsoleLaunchConfig, slave_fd: RawFd) -> ! {
    if let Err(_error) = child_exec_console_inner(config, slave_fd) {
        unsafe { libc::_exit(127) };
    }
    unsafe { libc::_exit(127) };
}

fn child_exec_console_inner(config: &ConsoleLaunchConfig, slave_fd: RawFd) -> Result<(), SyscallError> {
    join_namespace(config.target_pid, "user")?;
    join_namespace(config.target_pid, "mnt")?;
    join_namespace(config.target_pid, "net")?;
    join_namespace(config.target_pid, "uts")?;
    join_namespace(config.target_pid, "ipc")?;

    attach_self_to_cgroup(&config.cgroup_path.join("cgroup.procs"))?;
    make_controlling_terminal(slave_fd)?;
    chroot_into(&config.rootfs, &config.workdir)?;

    let shell = CString::new(config.shell.as_bytes())
        .map_err(|_| SyscallError::Capability("shell contains nul".to_string()))?;
    let arg0 = shell.clone();
    let argv = [arg0.as_ptr(), std::ptr::null()];
    let env = config
        .env
        .iter()
        .map(|(key, value)| CString::new(format!("{key}={value}")).map_err(|_| {
            SyscallError::Capability("environment entry contains nul".to_string())
        }))
        .collect::<Result<Vec<_>, _>>()?;
    let mut env_ptrs = env.iter().map(|value| value.as_ptr()).collect::<Vec<_>>();
    env_ptrs.push(std::ptr::null());
    unsafe {
        libc::execve(shell.as_ptr(), argv.as_ptr(), env_ptrs.as_ptr());
    }
    Err(SyscallError::Io(std::io::Error::last_os_error()))
}

fn join_namespace(pid: u32, name: &str) -> Result<(), SyscallError> {
    let path = format!("/proc/{pid}/ns/{name}");
    let file = std::fs::File::open(&path).map_err(SyscallError::Io)?;
    let rc = unsafe { libc::setns(file.as_raw_fd(), 0) };
    if rc == -1 {
        return Err(SyscallError::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

fn attach_self_to_cgroup(path: &Path) -> Result<(), SyscallError> {
    std::fs::write(path, format!("{}\n", std::process::id())).map_err(SyscallError::Io)
}

fn make_controlling_terminal(slave_fd: RawFd) -> Result<(), SyscallError> {
    unsafe {
        libc::setsid();
        if libc::ioctl(slave_fd, libc::TIOCSCTTY, 0) == -1 {
            return Err(SyscallError::Io(std::io::Error::last_os_error()));
        }
        for fd in [libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO] {
            if libc::dup2(slave_fd, fd) == -1 {
                return Err(SyscallError::Io(std::io::Error::last_os_error()));
            }
        }
    }
    Ok(())
}

fn chroot_into(rootfs: &Path, workdir: &str) -> Result<(), SyscallError> {
    let root = CString::new(rootfs.as_os_str().as_encoded_bytes())
        .map_err(|_| SyscallError::Capability("rootfs contains nul".to_string()))?;
    let workdir = CString::new(workdir.as_bytes())
        .map_err(|_| SyscallError::Capability("workdir contains nul".to_string()))?;
    unsafe {
        if libc::chroot(root.as_ptr()) == -1 {
            return Err(SyscallError::Io(std::io::Error::last_os_error()));
        }
        if libc::chdir(workdir.as_ptr()) == -1 {
            return Err(SyscallError::Io(std::io::Error::last_os_error()));
        }
    }
    Ok(())
}
```

- [ ] **Step 5: Export syscall modules**

In `src/syscall/mod.rs`, add:

```rust
pub mod console;
pub mod pty;
```

- [ ] **Step 6: Wire `PtyMaster` into `ConsolePty`**

Modify `src/runtime/console.rs`:

```rust
impl ConsolePty for crate::syscall::pty::PtyMaster {
    fn resize(&self, cols: u16, rows: u16) -> std::io::Result<()> {
        crate::syscall::pty::PtyMaster::resize(self, cols, rows)
    }
}
```

Remove the blanket `impl<T> ConsolePty for T` if Rust reports conflicting implementations. Replace the fake runtime PTY with a local wrapper type in `src/runtime/fake.rs` when needed:

```rust
struct FakeConsolePty(tokio::io::DuplexStream);

impl tokio::io::AsyncRead for FakeConsolePty {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl tokio::io::AsyncWrite for FakeConsolePty {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.0).poll_write(cx, bytes)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

impl crate::runtime::console::ConsolePty for FakeConsolePty {
    fn resize(&self, _cols: u16, _rows: u16) -> std::io::Result<()> {
        Ok(())
    }
}
```

- [ ] **Step 7: Implement `LinuxRuntime::open_console`**

In `src/runtime/linux.rs`, import:

```rust
use crate::runtime::console::{RuntimeConsoleRequest, RuntimeConsoleSession};
use crate::syscall::console::{ConsoleLaunchConfig, spawn_console_process};
use crate::syscall::pty::open_pty;
```

Add method inside `impl Runtime for LinuxRuntime`:

```rust
async fn open_console(
    &self,
    request: RuntimeConsoleRequest,
) -> Result<RuntimeConsoleSession, RuntimeError> {
    self.reap_exited_children()?;
    let instance = crate::domain::RuntimeInstanceId {
        service_id: request.service_id,
        service_name: request.service_name.clone(),
        replica_index: request.replica_index,
    };
    let tracked = {
        let children = self.children.lock().map_err(|_| RuntimeError::LockPoisoned)?;
        children
            .get(&instance)
            .ok_or_else(|| RuntimeError::InvalidServiceName {
                name: "selected replica is not running".to_string(),
            })?
            .clone_for_console()
    };
    let TrackedProcess::NativePid(target_pid) = tracked.process;
    if target_pid == 0 {
        return Err(RuntimeError::InvalidServiceName {
            name: "selected replica has exited".to_string(),
        });
    }

    let (pty, slave) = open_pty(request.cols, request.rows).map_err(RuntimeError::Io)?;
    let config = ConsoleLaunchConfig {
        target_pid,
        cgroup_path: tracked.plan.cgroup_path.clone(),
        rootfs: tracked.plan.merged.clone(),
        workdir: tracked.plan.namespace.workdir.clone(),
        env: tracked.plan.namespace.env.clone(),
        shell: "/bin/sh".to_string(),
    };
    let child_pid = tokio::task::spawn_blocking(move || spawn_console_process(&config, slave))
        .await?
        .map_err(RuntimeError::Syscall)?;

    Ok(RuntimeConsoleSession {
        session_id: request.session_id,
        replica_index: request.replica_index,
        child_pid,
        cgroup_path: tracked.plan.cgroup_path,
        pty: Box::new(pty),
    })
}
```

Add this helper to `TrackedChild` in `src/runtime/plan.rs`:

```rust
impl TrackedChild {
    pub(crate) fn clone_for_console(&self) -> Self {
        Self {
            process: self.process.clone(),
            plan: self.plan.clone(),
        }
    }
}
```

Add `Clone` to `TrackedProcess`:

```rust
#[derive(Debug, Clone)]
pub(crate) enum TrackedProcess {
    NativePid(u32),
}
```

- [ ] **Step 8: Add unit tests**

Add to `src/runtime/linux.rs` tests:

```rust
#[tokio::test]
async fn open_console_rejects_missing_replica() {
    let runtime = LinuxRuntime::new_with_paths(
        tempfile::tempdir().unwrap().path().join("runtime"),
        tempfile::tempdir().unwrap().path().join("artifacts"),
        tempfile::tempdir().unwrap().path().join("cgroup"),
    );
    let err = runtime
        .open_console(RuntimeConsoleRequest {
            session_id: uuid::Uuid::now_v7(),
            service_id: uuid::Uuid::now_v7(),
            service_name: "web".to_string(),
            deployment_id: uuid::Uuid::now_v7(),
            replica_index: 0,
            cols: 120,
            rows: 32,
        })
        .await
        .unwrap_err();
    assert!(err.to_string().contains("selected replica"));
}
```

- [ ] **Step 9: Add privileged test**

Append to `tests/linux_runtime_privileged.rs`:

```rust
#[tokio::test]
#[ignore = "requires root, namespaces, cgroups, and busybox"]
async fn console_exec_reads_service_environment() {
    if std::env::var("DENIA_RUN_PRIVILEGED_TESTS").ok().as_deref() != Some("1") {
        eprintln!("set DENIA_RUN_PRIVILEGED_TESTS=1 to run");
        return;
    }
    let runtime_dir = tempfile::tempdir().expect("runtime dir");
    let artifact_dir = tempfile::tempdir().expect("artifact dir");
    let helper_dir = tempfile::tempdir().expect("helper dir");
    let cgroup_root = CgroupTestRoot::new();
    let socket_proxy = socket_proxy_helper(helper_dir.path());
    let runtime =
        LinuxRuntime::new_with_paths(runtime_dir.path(), artifact_dir.path(), cgroup_root.path())
            .with_socket_proxy(socket_proxy);
    let artifact = ArtifactRecord::new(
        "sha256:console",
        ArtifactKind::RootfsBundle,
        ArtifactSource::ExternalRegistry {
            image: "local/rootfs:console".to_string(),
        },
    )
    .expect("artifact");
    let bundle_dir = artifact_dir.path().join("sha256-console");
    let rootfs = bundle_dir.join("rootfs");
    write_busybox_rootfs(&rootfs);
    std::fs::write(
        bundle_dir.join("process.json"),
        serde_json::to_vec(&LinuxRuntimeProcessSpec {
            argv: vec!["/bin/sleep".to_string(), "300".to_string()],
            env: vec![("DENIA_CONSOLE_TEST".to_string(), "inside".to_string())],
            workdir: "/".to_string(),
        })
        .expect("manifest json"),
    )
    .expect("manifest");

    let service_id = uuid::Uuid::now_v7();
    let deployment_id = uuid::Uuid::now_v7();
    let status = runtime
        .start(RuntimeStartRequest {
            service_name: "console-service".to_string(),
            service_id,
            deployment_id,
            artifact,
            internal_port: 3000,
            socket_path: runtime_dir.path().join("console-service/current.sock"),
            cpu_millis: 100,
            memory_bytes: 67108864,
            env: Vec::new(),
            pids_max: None,
            memory_swap_max: None,
            io_weight: None,
            replica_index: 0,
        })
        .await
        .expect("runtime start");
    wait_for_path(&status.socket_path);

    let mut session = runtime
        .open_console(denia::runtime::RuntimeConsoleRequest {
            session_id: uuid::Uuid::now_v7(),
            service_id,
            service_name: "console-service".to_string(),
            deployment_id,
            replica_index: 0,
            cols: 120,
            rows: 32,
        })
        .await
        .expect("open console");
    tokio::io::AsyncWriteExt::write_all(
        &mut session.pty,
        b"echo $DENIA_CONSOLE_TEST; exit\n",
    )
    .await
    .expect("write console command");
    let mut output = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::io::AsyncReadExt::read_to_end(&mut session.pty, &mut output),
    )
    .await
    .expect("console output timeout")
    .expect("read console output");
    assert!(
        String::from_utf8_lossy(&output).contains("inside"),
        "console output should contain service env, got {:?}",
        String::from_utf8_lossy(&output)
    );
    runtime
        .stop(&RuntimeInstanceId {
            service_id,
            service_name: "console-service".to_string(),
            replica_index: 0,
        })
        .await
        .expect("stop service");
}
```

- [ ] **Step 10: Run tests**

Run:

```bash
cargo test syscall::pty
cargo test runtime::linux::open_console_rejects_missing_replica
```

Expected: tests pass without privileged mode.

- [ ] **Step 11: Commit**

```bash
git add src/syscall/pty.rs src/syscall/console.rs src/syscall/mod.rs src/runtime/console.rs src/runtime/linux.rs src/runtime/plan.rs tests/linux_runtime_privileged.rs
git commit -m "feat(runtime): add live service console launcher"
```

## Task 5: Websocket PTY Bridge

**Files:**
- Modify: `src/api/console.rs`
- Test: `src/api/console.rs`

- [ ] **Step 1: Run impact check**

Run:

```bash
gitnexus_impact({target: "handle_console_socket", direction: "upstream", repo: "denia"})
```

Expected: LOW or no indexed impact because the symbol is new. Continue if GitNexus reports it is not indexed yet.

- [ ] **Step 2: Replace bridge body**

In `src/api/console.rs`, replace `bridge_console_socket` with:

```rust
async fn bridge_console_socket(
    mut socket: WebSocket,
    mut session: crate::runtime::RuntimeConsoleSession,
) {
    use axum::extract::ws::CloseFrame;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut buf = vec![0_u8; 8192];
    loop {
        tokio::select! {
            read = session.pty.read(&mut buf) => {
                match read {
                    Ok(0) => break,
                    Ok(n) => {
                        if socket.send(Message::Binary(buf[..n].to_vec().into())).await.is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = send_control(
                            &mut socket,
                            ConsoleControlFrame::Error { message: error.to_string() },
                        ).await;
                        break;
                    }
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Binary(bytes))) => {
                        if session.pty.write_all(&bytes).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<ConsoleControlFrame>(&text) {
                            Ok(ConsoleControlFrame::Resize { cols, rows }) => {
                                let _ = session.pty.resize(cols, rows);
                            }
                            Ok(ConsoleControlFrame::Close) => break,
                            _ => {}
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
        }
    }

    let _ = crate::syscall::signal::kill(session.child_pid, rustix::process::Signal::TERM);
    let _ = send_control(&mut socket, ConsoleControlFrame::Exit { code: None }).await;
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code: axum::extract::ws::close_code::NORMAL,
            reason: "console closed".into(),
        })))
        .await;
}

async fn send_control(socket: &mut WebSocket, frame: ConsoleControlFrame) -> Result<(), axum::Error> {
    socket
        .send(Message::Text(
            serde_json::to_string(&frame)
                .unwrap_or_else(|_| "{\"type\":\"error\",\"message\":\"control encode failed\"}".to_string())
                .into(),
        ))
        .await
}
```

- [ ] **Step 3: Add bridge tests**

Add tests for the pure helpers:

```rust
#[test]
fn consume_ticket_rejects_wrong_service() {
    let service_id = uuid::Uuid::now_v7();
    let other_id = uuid::Uuid::now_v7();
    let ticket = "unit-test-ticket".to_string();
    TICKETS.lock().unwrap().insert(ticket.clone(), ConsoleTicket {
        service_id,
        service_name: "web".to_string(),
        deployment_id: uuid::Uuid::now_v7(),
        replica_index: 0,
        principal_label: "tester".to_string(),
        cols: 120,
        rows: 32,
        expires_at: chrono::Utc::now() + chrono::TimeDelta::seconds(30),
    });
    let err = consume_ticket(other_id, &ticket).unwrap_err();
    assert!(err.to_string().contains("mismatch"));
}

#[test]
fn consume_ticket_is_single_use() {
    let service_id = uuid::Uuid::now_v7();
    let ticket = "unit-test-single-use".to_string();
    TICKETS.lock().unwrap().insert(ticket.clone(), ConsoleTicket {
        service_id,
        service_name: "web".to_string(),
        deployment_id: uuid::Uuid::now_v7(),
        replica_index: 0,
        principal_label: "tester".to_string(),
        cols: 120,
        rows: 32,
        expires_at: chrono::Utc::now() + chrono::TimeDelta::seconds(30),
    });
    assert!(consume_ticket(service_id, &ticket).is_ok());
    assert!(consume_ticket(service_id, &ticket).is_err());
}
```

- [ ] **Step 4: Run tests**

Run:

```bash
cargo test api::console
```

Expected: all console API tests pass.

- [ ] **Step 5: Commit**

```bash
git add src/api/console.rs
git commit -m "feat(api): bridge console websocket to pty"
```

## Task 6: Cross-Platform CLI Console

**Files:**
- Create: `src/cli/client/console.rs`
- Modify: `src/cli/client/mod.rs`
- Modify: `src/cli/client/http.rs`
- Modify: `src/cli/mod.rs`
- Test: `tests/client_console.rs`, `tests/cli_help.rs`

- [ ] **Step 1: Run impact checks**

Run:

```bash
gitnexus_impact({target: "ClientApi", direction: "upstream", repo: "denia"})
gitnexus_impact({target: "dispatch", direction: "upstream", repo: "denia"})
```

Expected: no HIGH/CRITICAL warnings. If `dispatch` is ambiguous, run it with `file_path: "src/cli/mod.rs"`.

- [ ] **Step 2: Extend client HTTP types**

Modify `src/cli/client/http.rs` with:

```rust
#[derive(Debug, Deserialize)]
pub struct ConsoleReplicaView {
    pub service_id: String,
    pub service_name: String,
    pub deployment_id: String,
    pub replica_index: u32,
    pub state: String,
}

#[derive(Debug, Deserialize)]
pub struct ConsoleTicketView {
    pub ticket: String,
    pub expires_at: String,
    pub ws_path: String,
}

#[derive(Debug, Serialize)]
struct ConsoleTicketRequest {
    replica_index: u32,
    cols: u16,
    rows: u16,
}
```

Add methods:

```rust
pub async fn list_console_replicas(
    &self,
    bearer: &str,
    service_id: &str,
) -> Result<Vec<ConsoleReplicaView>, ClientApiError> {
    self.get_json(&format!("/v1/services/{service_id}/console/replicas"), bearer).await
}

pub async fn create_console_ticket(
    &self,
    bearer: &str,
    service_id: &str,
    replica_index: u32,
    cols: u16,
    rows: u16,
) -> Result<ConsoleTicketView, ClientApiError> {
    self.post_json(
        &format!("/v1/services/{service_id}/console/tickets"),
        bearer,
        &ConsoleTicketRequest { replica_index, cols, rows },
    )
    .await
}

pub fn websocket_url(&self, ws_path: &str) -> Result<String, ClientApiError> {
    let mut url = reqwest::Url::parse(&self.base_url)
        .map_err(|error| ClientApiError::Api {
            status: StatusCode::BAD_REQUEST,
            body: error.to_string(),
        })?;
    match url.scheme() {
        "http" => url.set_scheme("ws").map_err(|_| ClientApiError::Api {
            status: StatusCode::BAD_REQUEST,
            body: "could not set websocket scheme".to_string(),
        })?,
        "https" => url.set_scheme("wss").map_err(|_| ClientApiError::Api {
            status: StatusCode::BAD_REQUEST,
            body: "could not set websocket scheme".to_string(),
        })?,
        _ => return Err(ClientApiError::Api {
            status: StatusCode::BAD_REQUEST,
            body: "profile url must use http or https".to_string(),
        }),
    }
    url.set_path(ws_path.split('?').next().unwrap_or(ws_path));
    url.set_query(ws_path.split_once('?').map(|(_, query)| query));
    Ok(url.to_string())
}
```

- [ ] **Step 3: Create console command**

Create `src/cli/client/console.rs`:

```rust
use std::io::{Read, Write};
use std::path::PathBuf;

use clap::Args;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

use super::http::{ClientApi, ServiceView};
use super::manifest::DeniaManifest;
use super::profile::{ClientConfig, config_path};

#[derive(Args, Debug)]
pub struct ConsoleArgs {
    /// Service id or service name. Omit to read service/project from `.denia`.
    pub service: Option<String>,
    /// Project name for service-name resolution.
    #[arg(long)]
    pub project: Option<String>,
    /// Replica index to attach to.
    #[arg(long)]
    pub replica: Option<u32>,
    /// Project directory containing `.denia` when SERVICE is omitted.
    #[arg(long, default_value = ".")]
    pub path: PathBuf,
}

pub async fn run(args: ConsoleArgs) -> anyhow::Result<()> {
    let cfg = ClientConfig::load_from(&config_path()?)?;
    let profile = cfg.active_profile()?;
    let api = ClientApi::new(&profile.url);
    let token = &profile.token;

    let manifest = read_manifest_if_present(&args.path)?;
    let project_name = args
        .project
        .clone()
        .or_else(|| manifest.as_ref().map(|m| m.project.clone()));
    let service_name = args
        .service
        .clone()
        .or_else(|| manifest.as_ref().map(|m| m.service.clone()))
        .ok_or_else(|| anyhow::anyhow!("service is required when .denia is not present"))?;

    let service = resolve_service(&api, token, project_name.as_deref(), &service_name).await?;
    let replicas = api.list_console_replicas(token, &service.id).await?;
    if replicas.is_empty() {
        anyhow::bail!("service '{}' has no running replicas", service.name);
    }
    let replica_index = match args.replica {
        Some(index) => index,
        None if replicas.len() == 1 => replicas[0].replica_index,
        None => {
            eprintln!("service '{}' has multiple running replicas:", service.name);
            for replica in &replicas {
                eprintln!(
                    "  replica={} deployment={} state={}",
                    replica.replica_index, replica.deployment_id, replica.state
                );
            }
            anyhow::bail!("choose a replica with --replica <INDEX>");
        }
    };

    let (cols, rows) = terminal_size();
    let ticket = api
        .create_console_ticket(token, &service.id, replica_index, cols, rows)
        .await?;
    let ws_url = api.websocket_url(&ticket.ws_path)?;
    run_terminal(ws_url).await
}

fn read_manifest_if_present(path: &std::path::Path) -> anyhow::Result<Option<DeniaManifest>> {
    let manifest_path = path.join(".denia");
    if !manifest_path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&manifest_path)?;
    Ok(Some(DeniaManifest::parse(&raw)?))
}

async fn resolve_service(
    api: &ClientApi,
    token: &str,
    project_name: Option<&str>,
    service: &str,
) -> anyhow::Result<ServiceView> {
    let services = api.list_services(token).await?;
    if uuid::Uuid::parse_str(service).is_ok() {
        return services
            .into_iter()
            .find(|s| s.id == service)
            .ok_or_else(|| anyhow::anyhow!("service id '{}' not found", service));
    }
    let project_id = match project_name {
        Some(name) => {
            let projects = api.list_projects(token).await?;
            Some(
                projects
                    .into_iter()
                    .find(|p| p.name == name)
                    .ok_or_else(|| anyhow::anyhow!("project '{}' not found", name))?
                    .id,
            )
        }
        None => None,
    };
    let matches = services
        .into_iter()
        .filter(|s| s.name == service)
        .filter(|s| project_id.as_ref().is_none_or(|id| &s.project_id == id))
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [one] => Ok(one.clone()),
        [] => anyhow::bail!("service '{}' not found", service),
        _ => anyhow::bail!("service name '{}' is ambiguous; pass --project", service),
    }
}

fn terminal_size() -> (u16, u16) {
    crossterm::terminal::size().unwrap_or((120, 32))
}

async fn run_terminal(ws_url: String) -> anyhow::Result<()> {
    let (stream, _) = tokio_tungstenite::connect_async(&ws_url).await?;
    let (mut write, mut read) = stream.split();
    let _raw = RawModeGuard::enter()?;

    let stdin_task = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<Vec<u8>>> {
        let mut stdin = std::io::stdin();
        let mut chunks = Vec::new();
        let mut buf = [0_u8; 1024];
        loop {
            let n = stdin.read(&mut buf)?;
            if n == 0 {
                break;
            }
            chunks.push(buf[..n].to_vec());
        }
        Ok(chunks)
    });

    let output_task = tokio::spawn(async move {
        while let Some(message) = read.next().await {
            match message? {
                Message::Binary(bytes) => {
                    std::io::stdout().write_all(&bytes)?;
                    std::io::stdout().flush()?;
                }
                Message::Text(text) => {
                    if text.contains("\"type\":\"exit\"") {
                        break;
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
        anyhow::Ok(())
    });

    for chunk in stdin_task.await?? {
        write.send(Message::Binary(chunk.into())).await?;
    }
    let _ = write.close().await;
    output_task.await??;
    Ok(())
}

struct RawModeGuard;

impl RawModeGuard {
    fn enter() -> anyhow::Result<Self> {
        crossterm::terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
    }
}
```

- [ ] **Step 4: Export CLI module**

In `src/cli/client/mod.rs`, add:

```rust
pub mod console;
```

- [ ] **Step 5: Add top-level command**

In both `Commands` enums in `src/cli/mod.rs`, add:

```rust
/// Open an interactive shell inside a running service replica.
Console(client::console::ConsoleArgs),
```

In both `dispatch` functions, add:

```rust
Some(Commands::Console(args)) => block_on(crate::cli::client::console::run(args)),
```

- [ ] **Step 6: Add CLI tests**

Create `tests/client_console.rs`:

```rust
use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn console_help_is_available() {
    let mut cmd = Command::cargo_bin("denia").unwrap();
    cmd.arg("console")
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("--replica"))
        .stdout(predicate::str::contains("--project"));
}

#[test]
fn console_without_profile_errors_cleanly() {
    let temp = tempfile::tempdir().unwrap();
    let mut cmd = Command::cargo_bin("denia").unwrap();
    cmd.env("DENIA_CLIENT_CONFIG", temp.path().join("missing.toml"))
        .arg("console")
        .arg("web")
        .assert()
        .failure()
        .stderr(predicate::str::contains("profile"));
}
```

Update `tests/cli_help.rs`:

```rust
.stdout(predicates::str::contains("console"))
```

- [ ] **Step 7: Run tests**

Run:

```bash
cargo test --test client_console
cargo test --test cli_help
cargo check --no-default-features --features client
```

Expected: CLI tests and client-only build pass.

- [ ] **Step 8: Commit**

```bash
git add src/cli/client/console.rs src/cli/client/mod.rs src/cli/client/http.rs src/cli/mod.rs tests/client_console.rs tests/cli_help.rs
git commit -m "feat(cli): add service console command"
```

## Task 7: Web Console UI

**Files:**
- Modify: `web/src/effect/schema.ts`
- Modify: `web/src/effect/api-client.ts`
- Create: `web/src/components/ServiceConsole.tsx`
- Modify: `web/src/routes/services/$serviceId.tsx`
- Modify: `web/src/styles.css`
- Test: `web/src/components/ServiceConsole.test.tsx`, `web/src/routes/services/-detail.test.tsx`, `web/src/effect/api-client.test.ts`

- [ ] **Step 1: Add Effect schemas**

In `web/src/effect/schema.ts`, add:

```ts
export class ConsoleReplica extends Schema.Class<ConsoleReplica>('ConsoleReplica')({
  service_id: Schema.String,
  service_name: Schema.String,
  deployment_id: Schema.String,
  replica_index: Schema.Number,
  state: Schema.String,
}) {}

export const ConsoleReplicas = Schema.Array(ConsoleReplica)

export class ConsoleTicket extends Schema.Class<ConsoleTicket>('ConsoleTicket')({
  ticket: Schema.String,
  expires_at: Schema.String,
  ws_path: Schema.String,
}) {}
```

- [ ] **Step 2: Add API client methods**

In `web/src/effect/api-client.ts`, import the new schemas and extend `ApiClient`:

```ts
readonly listConsoleReplicas: (
  serviceId: string,
) => Effect.Effect<ReadonlyArray<ConsoleReplica>, ApiError | DecodeError>
readonly createConsoleTicket: (
  serviceId: string,
  replicaIndex: number,
  cols: number,
  rows: number,
) => Effect.Effect<ConsoleTicket, ApiError | DecodeError>
```

Inside `ApiClientLive`, add:

```ts
const listConsoleReplicas = (serviceId: string) =>
  Effect.gen(function* () {
    const response = yield* http
      .get(url(`/v1/services/${serviceId}/console/replicas`), {
        headers: authHeaders(),
      })
      .pipe(Effect.mapError(httpError))
    return yield* parseResponse(response, ConsoleReplicas)
  })

const createConsoleTicket = (
  serviceId: string,
  replicaIndex: number,
  cols: number,
  rows: number,
) =>
  Effect.gen(function* () {
    const response = yield* http
      .post(url(`/v1/services/${serviceId}/console/tickets`), {
        headers: {
          ...authHeaders(),
          'content-type': 'application/json',
        },
        body: jsonBody({ replica_index: replicaIndex, cols, rows }),
      })
      .pipe(Effect.mapError(httpError))
    return yield* parseResponse(response, ConsoleTicket)
  })
```

Return these methods from the service object.

- [ ] **Step 3: Create terminal component**

Create `web/src/components/ServiceConsole.tsx`:

```tsx
import '@xterm/xterm/css/xterm.css'
import { useEffect, useRef, useState } from 'react'
import { Terminal } from '@xterm/xterm'
import { useMutation, useQuery } from '@tanstack/react-query'
import { Effect } from 'effect'
import { ApiClient } from '#/effect/api-client'
import { getApiBaseUrl } from '#/effect/config'
import { runQuery } from '#/effect/runtime'
import { errorMessage } from './ErrorPanel'

function listReplicas(serviceId: string) {
  return Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.listConsoleReplicas(serviceId)
  })
}

function createTicket(serviceId: string, replicaIndex: number, cols: number, rows: number) {
  return Effect.gen(function* () {
    const api = yield* ApiClient
    return yield* api.createConsoleTicket(serviceId, replicaIndex, cols, rows)
  })
}

function wsUrl(path: string): string {
  const base = getApiBaseUrl()
  const origin = base || window.location.origin
  const url = new URL(path, origin)
  url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:'
  return url.toString()
}

export function ServiceConsole({ serviceId }: { readonly serviceId: string }) {
  const hostRef = useRef<HTMLDivElement | null>(null)
  const termRef = useRef<Terminal | null>(null)
  const socketRef = useRef<WebSocket | null>(null)
  const [selectedReplica, setSelectedReplica] = useState<number | null>(null)
  const [status, setStatus] = useState('disconnected')
  const [error, setError] = useState('')

  const { data: replicas = [], isLoading } = useQuery({
    queryKey: ['services', serviceId, 'console', 'replicas'],
    queryFn: () => runQuery(listReplicas(serviceId)),
    refetchInterval: 5000,
  })

  useEffect(() => {
    if (selectedReplica !== null || replicas.length !== 1) return
    setSelectedReplica(replicas[0].replica_index)
  }, [replicas, selectedReplica])

  useEffect(() => {
    const terminal = new Terminal({
      cols: 120,
      rows: 32,
      cursorBlink: true,
      convertEol: true,
      fontFamily: 'JetBrains Mono, ui-monospace, SFMono-Regular, monospace',
      fontSize: 13,
      theme: {
        background: '#121115',
        foreground: '#f4eff7',
        cursor: '#ff4fa3',
      },
    })
    termRef.current = terminal
    if (hostRef.current) terminal.open(hostRef.current)
    return () => {
      socketRef.current?.close()
      terminal.dispose()
    }
  }, [])

  const connect = useMutation({
    mutationFn: async () => {
      const replica = selectedReplica
      const terminal = termRef.current
      if (replica === null || terminal === null) throw new Error('select a replica')
      const ticket = await runQuery(createTicket(serviceId, replica, terminal.cols, terminal.rows))
      return ticket.ws_path
    },
    onSuccess: (path) => {
      const terminal = termRef.current
      if (!terminal) return
      terminal.clear()
      const socket = new WebSocket(wsUrl(path))
      socket.binaryType = 'arraybuffer'
      socketRef.current = socket
      setError('')
      setStatus('connecting')
      socket.onopen = () => {
        setStatus('connected')
        terminal.focus()
      }
      socket.onmessage = (event) => {
        if (event.data instanceof ArrayBuffer) {
          terminal.write(new Uint8Array(event.data))
          return
        }
        if (typeof event.data === 'string' && event.data.includes('"type":"error"')) {
          setError(event.data)
          setStatus('error')
        }
      }
      socket.onclose = () => setStatus('disconnected')
      socket.onerror = () => {
        setStatus('error')
        setError('console websocket failed')
      }
      terminal.onData((data) => {
        if (socket.readyState === WebSocket.OPEN) {
          socket.send(new TextEncoder().encode(data))
        }
      })
    },
    onError: (err: unknown) => {
      setError(errorMessage(err))
      setStatus('error')
    },
  })

  return (
    <div className="stack">
      <div className="panel-head">
        <div className="cluster">
          <label className="kicker" htmlFor="console-replica">replica</label>
          <select
            id="console-replica"
            className="field"
            value={selectedReplica ?? ''}
            onChange={(event) => setSelectedReplica(Number(event.target.value))}
            disabled={isLoading || replicas.length === 0 || status === 'connected'}
          >
            <option value="" disabled>select</option>
            {replicas.map((replica) => (
              <option key={replica.replica_index} value={replica.replica_index}>
                {replica.replica_index} · {replica.state}
              </option>
            ))}
          </select>
          <span className="badge">{status}</span>
        </div>
        <div className="cluster">
          <button
            type="button"
            className="btn btn-primary"
            onClick={() => connect.mutate()}
            disabled={selectedReplica === null || connect.isPending || status === 'connected'}
          >
            connect
          </button>
          <button
            type="button"
            className="btn"
            onClick={() => socketRef.current?.close()}
            disabled={status !== 'connected'}
          >
            disconnect
          </button>
        </div>
      </div>
      {error ? <p className="field-error">{error}</p> : null}
      <div className="terminal-panel" ref={hostRef} aria-label="service console terminal" />
    </div>
  )
}
```

- [ ] **Step 4: Add service tab**

In `web/src/routes/services/$serviceId.tsx`, import:

```ts
import { ServiceConsole } from '#/components/ServiceConsole'
```

Add operator-only tab:

```ts
...(canOperate ? [{ id: 'console', label: 'console' }] : []),
```

Add tab branch before metrics:

```tsx
if (active === 'console') {
  return <ServiceConsole serviceId={id} />
}
```

- [ ] **Step 5: Add styles**

In `web/src/styles.css`, add:

```css
.terminal-panel {
  min-height: 34rem;
  height: min(68vh, 42rem);
  overflow: hidden;
  border: 1px solid var(--line);
  background: #121115;
  padding: 0.75rem;
}

.terminal-panel .xterm {
  height: 100%;
}

.terminal-panel .xterm-viewport {
  scrollbar-color: var(--fg-faint) transparent;
}
```

- [ ] **Step 6: Add web tests**

Create `web/src/components/ServiceConsole.test.tsx`:

```tsx
import { describe, expect, it, vi } from 'vitest'
import { render, screen } from '@testing-library/react'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { ServiceConsole } from './ServiceConsole'

vi.mock('#/effect/runtime', () => ({
  runQuery: vi.fn(async () => [
    {
      service_id: 'svc',
      service_name: 'web',
      deployment_id: 'dep',
      replica_index: 0,
      state: 'running',
    },
  ]),
}))

vi.mock('@xterm/xterm', () => ({
  Terminal: class {
    cols = 120
    rows = 32
    open() {}
    dispose() {}
    clear() {}
    focus() {}
    onData() {}
  },
}))

describe('ServiceConsole', () => {
  it('renders replica selector and connect controls', async () => {
    const client = new QueryClient()
    render(
      <QueryClientProvider client={client}>
        <ServiceConsole serviceId="svc" />
      </QueryClientProvider>,
    )
    expect(await screen.findByLabelText('replica')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'connect' })).toBeInTheDocument()
  })
})
```

Update service detail tests to assert viewer role does not see `console` and operator role does.

- [ ] **Step 7: Run web tests**

Run:

```bash
cd web && pnpm test -- ServiceConsole
cd web && pnpm test -- services/-detail
cd web && pnpm typecheck
```

Expected: tests and typecheck pass.

- [ ] **Step 8: Commit**

```bash
git add web/src/effect/schema.ts web/src/effect/api-client.ts web/src/components/ServiceConsole.tsx web/src/routes/services/\$serviceId.tsx web/src/styles.css web/src/components/ServiceConsole.test.tsx web/src/routes/services/-detail.test.tsx web/src/effect/api-client.test.ts
git commit -m "feat(web): add service console terminal"
```

## Task 8: End-To-End Verification And Cleanup

**Files:**
- Potentially modify only files changed in previous tasks if verification exposes compile errors.

- [ ] **Step 1: Run full backend verification**

Run:

```bash
cargo fmt --all -- --check
cargo test
cargo clippy --all-targets --all-features
```

Expected:

- Formatting check passes.
- Test suite passes.
- Clippy passes without warnings that require code changes.

- [ ] **Step 2: Run client-only verification**

Run:

```bash
cargo check --no-default-features --features client
```

Expected: client-only build compiles without server dependencies.

- [ ] **Step 3: Run web verification**

Run:

```bash
cd web && pnpm test
cd web && pnpm typecheck
cd web && pnpm build
```

Expected: web tests, typecheck, and static SPA build pass.

- [ ] **Step 4: Run privileged verification when available**

Run on a host with root, namespace, cgroup v2, and busybox fixture support:

```bash
DENIA_RUN_PRIVILEGED_TESTS=1 cargo test --test linux_runtime_privileged -- --ignored
```

Expected:

- Existing privileged runtime tests pass.
- The new console test opens a shell in the live replica and reads the expected service env value.

- [ ] **Step 5: Run GitNexus change detection**

Run:

```bash
gitnexus_detect_changes({scope: "all", repo: "denia"})
```

Expected:

- Changed symbols and processes match the planned API/runtime/web/CLI scope.
- No unexpected unrelated modules appear.

- [ ] **Step 6: Final commit**

If previous tasks were committed separately, this step may be empty. If verification fixes were needed:

```bash
git add <fixed-files>
git commit -m "fix: stabilize service console verification"
```

## Self-Review

- Spec coverage: ADR, backend API, ticket auth, websocket bridge, live runtime launcher, web UI, CLI command, audit metadata, and tests are covered by Tasks 1-8.
- Placeholder scan: no placeholder markers or unspecified implementation branches remain.
- Type consistency: `ConsoleReplicaView`, `CreateConsoleTicketRequest`, `ConsoleTicketResponse`, `ConsoleControlFrame`, `RuntimeConsoleRequest`, and `RuntimeConsoleSession` use the same field names across backend, web, and CLI tasks.
- Risk controls: the plan avoids modifying `spawn_namespaced_process`, avoids `AppState` field additions, requires impact checks before edits, and requires GitNexus change detection before completion.
