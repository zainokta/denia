# Live Container Log Streaming Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `GET /v1/services/{id}/logs/stream` — a Server-Sent Events endpoint that emits a service's last ~200 log lines then streams new lines live, following through restarts until the client disconnects.

**Architecture:** A synchronous, axum-free `LogTailer` (in `src/observability/logs.rs`) owns the file offset + partial-line buffer and is fully unit-tested. The new async handler spawns a tailer task that emits backlog then polls the log file every 300ms, pushing each complete line into a bounded `mpsc` channel exposed to axum as an `Sse(ReceiverStream)`. Client disconnect drops the stream → channel send fails → task exits.

**Tech Stack:** Rust 2024, axum 0.8 (`response::sse`), `tokio` (time/interval), `tokio-stream` (`wrappers::ReceiverStream`).

**Spec:** `docs/superpowers/specs/2026-05-27-live-log-streaming-design.md`

---

### Task 1: Add `tokio-stream` dependency

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add the dependency**

In `Cargo.toml`, under `[dependencies]`, add (keep alphabetical-ish near `tokio`):

```toml
tokio-stream = "0.1"
```

- [ ] **Step 2: Verify it resolves and builds**

Run: `cargo build`
Expected: builds clean, `tokio-stream` downloaded/compiled.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "feat(deps): add tokio-stream for SSE log streaming"
```

---

### Task 2: `LogTailer` — backlog + incremental poll

A synchronous tailer that produces the initial backlog and then returns only newly-appended complete lines on each `poll()`. No axum, no async — pure file IO so it can be unit-tested directly.

**Files:**
- Modify: `src/observability/logs.rs` (append `LogTailer`, `split_complete`, tests)

- [ ] **Step 1: Write the failing tests**

Append to `src/observability/logs.rs`:

```rust
#[cfg(test)]
mod tailer_tests {
    use super::LogTailer;
    use std::fs;
    use std::io::Write;

    fn write(path: &std::path::Path, contents: &str) {
        let mut f = fs::File::create(path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
    }

    fn append(path: &std::path::Path, contents: &str) {
        let mut f = fs::OpenOptions::new().append(true).open(path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
    }

    #[test]
    fn backlog_returns_last_n_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("svc.log");
        write(&path, "l1\nl2\nl3\n");
        let mut tailer = LogTailer::new(&path);
        assert_eq!(tailer.backlog(2).unwrap(), vec!["l2", "l3"]);
    }

    #[test]
    fn backlog_buffers_trailing_partial() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("svc.log");
        write(&path, "a\nb");
        let mut tailer = LogTailer::new(&path);
        assert_eq!(tailer.backlog(10).unwrap(), vec!["a"]);
        // "b" is buffered, not yet a complete line; completed on next poll
        append(&path, "c\n");
        assert_eq!(tailer.poll().unwrap(), vec!["bc"]);
    }

    #[test]
    fn poll_returns_new_complete_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("svc.log");
        write(&path, "a\n");
        let mut tailer = LogTailer::new(&path);
        assert_eq!(tailer.backlog(10).unwrap(), vec!["a"]);
        append(&path, "b\nc\n");
        assert_eq!(tailer.poll().unwrap(), vec!["b", "c"]);
        assert_eq!(tailer.poll().unwrap(), Vec::<String>::new());
    }

    #[test]
    fn poll_buffers_partial_until_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("svc.log");
        write(&path, "a\n");
        let mut tailer = LogTailer::new(&path);
        let _ = tailer.backlog(10).unwrap();
        append(&path, "par");
        assert_eq!(tailer.poll().unwrap(), Vec::<String>::new());
        append(&path, "tial\n");
        assert_eq!(tailer.poll().unwrap(), vec!["partial"]);
    }

    #[test]
    fn poll_resets_on_truncation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("svc.log");
        write(&path, "a\nb\n");
        let mut tailer = LogTailer::new(&path);
        let _ = tailer.backlog(10).unwrap();
        // recreate smaller (len < offset) -> tailer re-reads from start
        write(&path, "x\n");
        assert_eq!(tailer.poll().unwrap(), vec!["x"]);
    }

    #[test]
    fn missing_file_yields_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.log");
        let mut tailer = LogTailer::new(&path);
        assert_eq!(tailer.backlog(10).unwrap(), Vec::<String>::new());
        assert_eq!(tailer.poll().unwrap(), Vec::<String>::new());
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib observability::logs::tailer_tests`
Expected: FAIL to compile — `LogTailer` does not exist.

- [ ] **Step 3: Implement `LogTailer` + `split_complete`**

Add to `src/observability/logs.rs`. Note the existing imports already cover `fs`, `OpenOptions`, `Read`, `Path`, `PathBuf`; add `Seek`/`SeekFrom` to the `std::io` import group.

Change the top import:

```rust
use std::{
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};
```

Append at end of file (before the `#[cfg(test)]` modules):

```rust
/// Splits a buffer into complete lines (newline-terminated) and any trailing
/// partial line (text after the last `\n`).
fn split_complete(buf: &str) -> (Vec<String>, String) {
    match buf.rfind('\n') {
        Some(idx) => {
            let complete = buf[..idx].split('\n').map(str::to_string).collect();
            let partial = buf[idx + 1..].to_string();
            (complete, partial)
        }
        None => (Vec::new(), buf.to_string()),
    }
}

/// Follows a single log file: produces an initial backlog, then returns only
/// newly-appended complete lines on each `poll`. Synchronous; safe to call from
/// a spawned task that owns it.
#[derive(Debug)]
pub struct LogTailer {
    path: PathBuf,
    offset: u64,
    partial: String,
}

impl LogTailer {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            offset: 0,
            partial: String::new(),
        }
    }

    /// Returns the last `limit` complete lines and advances the read position to
    /// end-of-file. A trailing partial line (no final newline) is buffered, not
    /// returned. Missing file yields an empty backlog.
    pub fn backlog(&mut self, limit: usize) -> std::io::Result<Vec<String>> {
        let bytes = match fs::read(&self.path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                self.offset = 0;
                self.partial.clear();
                return Ok(Vec::new());
            }
            Err(e) => return Err(e),
        };
        self.offset = bytes.len() as u64;
        let text = String::from_utf8_lossy(&bytes);
        let (complete, partial) = split_complete(&text);
        self.partial = partial;
        let start = complete.len().saturating_sub(limit);
        Ok(complete[start..].to_vec())
    }

    /// Returns complete lines appended since the last call. Buffers any trailing
    /// partial line until it is newline-terminated. Resets to start of file if
    /// the file shrank (truncation/recreation). Missing file yields empty.
    pub fn poll(&mut self) -> std::io::Result<Vec<String>> {
        let mut file = match OpenOptions::new().read(true).open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let len = file.metadata()?.len();
        if len < self.offset {
            self.offset = 0;
            self.partial.clear();
        }
        if len == self.offset {
            return Ok(Vec::new());
        }
        file.seek(SeekFrom::Start(self.offset))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        self.offset = len;

        let mut combined = std::mem::take(&mut self.partial);
        combined.push_str(&String::from_utf8_lossy(&bytes));
        let (complete, partial) = split_complete(&combined);
        self.partial = partial;
        Ok(complete)
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test --lib observability::logs::tailer_tests`
Expected: PASS (6 tests).

- [ ] **Step 5: Commit**

```bash
git add src/observability/logs.rs
git commit -m "feat(observability): add LogTailer for incremental log following"
```

---

### Task 3: SSE streaming handler + route

Adds `service_logs_stream`, registers the route, and tests auth + content-type + backlog delivery. Because the live stream never closes, handler tests read a bounded number of body frames under a timeout and then drop the stream (which cancels the tailer task).

**Files:**
- Modify: `src/api/services.rs` (imports, route, handler, tests)

- [ ] **Step 1: Write the failing tests**

Add these tests inside the existing `#[cfg(test)] mod tests` block in `src/api/services.rs`. They reuse `test_state`, `ADMIN_TOKEN`, and the project/service setup pattern from `create_then_list_service_roundtrips`.

```rust
    #[tokio::test]
    async fn log_stream_unauthenticated_returns_401() {
        let resp = build_router(test_state())
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/services/{}/logs/stream", uuid::Uuid::now_v7()))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn log_stream_emits_backlog_then_live() {
        use crate::domain::{
            ExternalImageSource, HealthCheck, Project, ServiceConfig, ServiceSource,
        };
        use tokio_stream::StreamExt;

        let state = test_state();
        let log_dir = state.config.log_dir.clone();
        let project = Project::new("team-stream", None).unwrap();
        state.projects.put_project(project.clone()).unwrap();
        let svc = ServiceConfig::new(
            project.id,
            "streamsvc",
            vec!["stream.example.com".into()],
            ServiceSource::ExternalImage(ExternalImageSource {
                image: "nginx".into(),
                credential: None,
                registry_id: None,
                image_ref: None,
            }),
            80,
            HealthCheck::new("/health", 5),
            None,
            Vec::new(),
        )
        .unwrap();
        let service_id = svc.id;
        state.services.put_service(svc).unwrap();

        // Seed a backlog line in the service log file.
        std::fs::create_dir_all(&log_dir).unwrap();
        let log_path = log_dir.join("streamsvc.log");
        std::fs::write(&log_path, "backlog-line\n").unwrap();

        let resp = build_router(state)
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/services/{service_id}/logs/stream"))
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert!(ct.starts_with("text/event-stream"), "content-type was {ct}");

        // Read bounded frames under a timeout; the stream itself never ends.
        let mut stream = resp.into_body().into_data_stream();
        let mut seen = String::new();

        // Backlog frame.
        let frame = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
            .await
            .expect("backlog frame timed out")
            .expect("stream ended")
            .unwrap();
        seen.push_str(&String::from_utf8_lossy(&frame));

        // Append a live line; poll interval is 300ms.
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new().append(true).open(&log_path).unwrap();
            f.write_all(b"live-line\n").unwrap();
        }

        // Drain a few more frames (skipping keep-alive comments) for the live line.
        for _ in 0..10 {
            if seen.contains("live-line") {
                break;
            }
            if let Ok(Some(Ok(frame))) =
                tokio::time::timeout(std::time::Duration::from_secs(2), stream.next()).await
            {
                seen.push_str(&String::from_utf8_lossy(&frame));
            }
        }

        assert!(seen.contains("backlog-line"), "missing backlog in: {seen}");
        assert!(seen.contains("live-line"), "missing live line in: {seen}");
        // Dropping `stream` here cancels the tailer task.
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib api::services::tests::log_stream`
Expected: FAIL to compile — `service_logs_stream` route/handler does not exist.

- [ ] **Step 3: Add imports**

At the top of `src/api/services.rs`, extend the axum import and add the streaming imports:

```rust
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
    routing::{get, post},
};
use std::convert::Infallible;
use std::time::Duration;
use tokio_stream::wrappers::ReceiverStream;

use crate::logs::{LogStore, LogTailer};
```

(Replace the existing `use crate::logs::LogStore;` line with the combined import above.)

- [ ] **Step 4: Register the route**

In `router()`, add the stream route (place it before the catch-all `{action}` route so it is not shadowed):

```rust
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/services", get(list_services).post(put_service))
        .route("/services/{service_id}/logs", get(service_logs))
        .route("/services/{service_id}/logs/stream", get(service_logs_stream))
        .route("/services/{service_id}/metrics", get(service_metrics))
        .route("/services/{service_id}/{action}", post(lifecycle_command))
}
```

- [ ] **Step 5: Implement the handler**

Add after `service_logs` in `src/api/services.rs`:

```rust
async fn service_logs_stream(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(service_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Sse<ReceiverStream<Result<Event, Infallible>>>, ApiError> {
    let Some(service) = state.services.get_service(service_id)? else {
        return Err(ApiError::NotFound("service not found".to_string()));
    };
    ensure_role(&state, &principal, service.project_id, Role::Operator)?;

    let log_path = std::path::Path::new(&state.config.log_dir)
        .join(format!("{}.log", service.name));

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(256);

    tokio::spawn(async move {
        let mut tailer = LogTailer::new(&log_path);

        if let Ok(lines) = tailer.backlog(200) {
            for line in lines {
                if tx.send(Ok(Event::default().data(line))).await.is_err() {
                    return;
                }
            }
        }

        let mut interval = tokio::time::interval(Duration::from_millis(300));
        loop {
            interval.tick().await;
            match tailer.poll() {
                Ok(lines) => {
                    for line in lines {
                        if tx.send(Ok(Event::default().data(line))).await.is_err() {
                            return;
                        }
                    }
                }
                Err(_) => continue, // transient read error; retry next tick
            }
        }
    });

    Ok(Sse::new(ReceiverStream::new(rx))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}
```

- [ ] **Step 6: Run tests to verify they pass**

Run: `cargo test --lib api::services::tests::log_stream`
Expected: PASS (2 tests). The live test should observe both `backlog-line` and `live-line`.

- [ ] **Step 7: Full build + format + clippy**

Run: `cargo build && cargo fmt --all && cargo clippy --all-targets --all-features`
Expected: clean build, no clippy warnings on new code.

- [ ] **Step 8: Commit**

```bash
git add src/api/services.rs
git commit -m "feat(api): add SSE live log stream endpoint for services"
```

---

### Task 4: Document in ADR-009

**Files:**
- Modify: `docs/adr/009-observability.md`

- [ ] **Step 1: Append the streaming decision**

Add a section to `docs/adr/009-observability.md` recording:
- New endpoint `GET /v1/services/{service_id}/logs/stream` (SSE, `text/event-stream`).
- SSE contract: each event's `data:` is exactly one log line; no event `id`/`event` fields.
- Mechanism: per-client `LogTailer` tails `{log_dir}/{service_name}.log` (poll + seek, 300ms), backlog of last 200 lines then live; stream closes only on client disconnect; 15s keep-alive.
- Auth: same bearer + `Operator` role as the one-shot logs endpoint; clients consume via `fetch` + `ReadableStream` (browser `EventSource` cannot send `Authorization`; a query-param token is rejected to avoid leaking into the access log).
- Dependency: added `tokio-stream` for `ReceiverStream`.

- [ ] **Step 2: Commit**

```bash
git add docs/adr/009-observability.md
git commit -m "docs(adr): record SSE log streaming in ADR-009"
```

---

## Final Verification

- [ ] `cargo build`
- [ ] `cargo test`
- [ ] `cargo fmt --all`
- [ ] `cargo clippy --all-targets --all-features`
- [ ] `gitnexus_detect_changes()` confirms only `Cargo.toml`, `Cargo.lock`, `src/observability/logs.rs`, `src/api/services.rs`, `docs/adr/009-observability.md`, and the spec/plan docs changed.
