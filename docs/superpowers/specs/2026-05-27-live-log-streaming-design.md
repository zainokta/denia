# Live Container Log Streaming — Design

**Date:** 2026-05-27
**Status:** Approved (design), pending implementation

## Goal

Add a live (follow) log stream for a running service, alongside the existing one-shot
`GET /v1/services/{id}/logs` endpoint. Backend only.

## Background

Workload stdout/stderr is redirected via `dup2` in `src/syscall/ns.rs` directly to
`{log_dir}/{service_name}.log`. Denia does **not** pipe child output through its own
process. The existing `GET /v1/services/{id}/logs` handler (`src/api/services.rs:107`)
returns the last 200 lines one-shot via `LogStore::read_recent`. There is no way to
follow logs as they are produced.

Because the child writes the log file directly, the only way for Denia to observe new
output is to **tail the file**.

## Decisions

| Question | Decision | Rationale |
|----------|----------|-----------|
| Transport | **SSE** (`text/event-stream`) | Unidirectional server→client, fits log push, trivial axum integration. |
| New-line detection | **Poll + seek** (~300ms tokio interval) | No extra inotify deps, robust across filesystems, bounded latency. |
| On connect | **Backlog + live** | Send last ~200 lines, then stream appends. Matches `docker logs -f` UX. |
| Stream lifecycle | **Client-disconnect only** | Log file is append-mode and persists across restarts; true follow through redeploys. |
| Auth | **fetch + ReadableStream (bearer header)** | Browser `EventSource` cannot set `Authorization`; a `?token=` query param would leak into the ingress access log (forbidden by AGENTS.md). Console consumes the SSE body via `fetch()` with the existing bearer header. No token in URL. |

## Architecture

```
client GET /v1/services/{id}/logs/stream (Bearer)
   -> authz (Principal + ensure_role Operator)            [api/services.rs]
   -> spawn tailer task, return Sse(ReceiverStream)
        task:
          backlog: LogTailer::backlog(200) -> emit each line as SSE Event
          loop:    interval(300ms) -> LogTailer::poll() -> emit new complete lines
          exit:    tx.send fails (client dropped the stream) -> task returns
```

- **`LogTailer`** (new, in `src/observability/logs.rs`): synchronous, axum-free, unit-testable.
  - `backlog(limit)`: read whole file, return last `limit` complete lines, set `offset`
    to current EOF; if file does not end in `\n`, buffer the trailing partial (not emitted).
  - `poll()`: read `offset..EOF`, prepend buffered partial, split into complete lines +
    new trailing partial, advance `offset`. Truncation guard: if `len < offset`, reset
    `offset = 0` and clear partial. Missing file → `Ok(vec![])` (keep waiting).
- **Handler** `service_logs_stream` (new, `src/api/services.rs`): authz identical to
  `service_logs`, builds bounded `mpsc` channel (256), spawns tailer task,
  returns `Sse::new(ReceiverStream::new(rx)).keep_alive(15s)`.
  Event item type `Result<Event, Infallible>`. Each event `data:` = one log line.
- **Cancellation**: axum drops the response stream on client disconnect → `ReceiverStream`
  dropped → `tx.send` errors → tailer task returns. No leak.
- **Multi-consumer**: each client gets its own `LogTailer` + poll loop. No broadcast
  fan-out (YAGNI for admin-console scale).

## Data Flow / Edge Cases

- **Workload not started** (file missing): empty backlog, `offset = 0`, poll retries open
  each tick. Stream stays open.
- **Restart / redeploy**: append-mode file persists and only grows → `offset` stays valid.
- **External truncation/recreation**: `len < offset` → reset to 0, re-read from start.
- **Partial last line**: never emitted until terminated by `\n`.
- **Transient read error**: skip tick, retry; stream never errors to the client.

## Dependencies

- Add `tokio-stream = "0.1"` for `wrappers::ReceiverStream`. Dependency change →
  documented in **ADR-009 (observability)**.

## API / ADR

- New route: `GET /v1/services/{service_id}/logs/stream`.
- AGENTS.md requires an ADR for API + dependency changes. Extend **ADR-009** with the
  streaming endpoint and the SSE line contract (one log line per `data:` event) rather
  than adding a new ADR — same observability surface.

## Testing

- **Unit (`LogTailer`)**: backlog last-N + offset; backlog buffers trailing partial;
  poll returns new complete lines; poll buffers partial until newline; poll resets on
  truncation; missing file → empty.
- **Integration (handler)**: tower `oneshot`, append to log file, read SSE body chunks,
  assert backlog-then-live ordering. Unauthorized → 401; wrong role → 403 before stream.

## Out of Scope

Frontend console wiring, log rotation, multi-consumer broadcast, structured/JSON log events.
