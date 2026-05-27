# ADR-009: Observability

- **Status**: Proposed
- **Date**: 2026-05-25

## Context

Operators need a node-level dashboard (CPU/mem/disk/load), a roll-up of
running workloads, and per-service request logs without standing up a metrics
side-car or a separate log pipeline. The control plane already reads cgroup
v2 counters; this ADR extends the read path to procfs/statvfs and to the
loopback bridge.

## Decision

- `src/node_metrics.rs` exposes `NodeMetricsReader` returning a `NodeSnapshot`
  with cumulative CPU jiffies (parsed from `/proc/stat`), memory total +
  available (from `/proc/meminfo`), 1/5/15-min load average (from
  `/proc/loadavg`), and disk total + available via `libc::statvfs` against
  `DENIA_NODE_DISK_PATH` (defaults to the data dir).
- `src/access_log.rs` exposes an in-process `AccessLogStore` capped at 200
  entries per service with `parse_request_line` / `parse_status_line` helpers
  for HTTP/1.x lines. The store is held on `AppState` so future bridge tee
  logic can append entries; today the API surface is wired but the bridge
  byte-copy path still uses `tokio::io::copy_bidirectional` and does not yet
  produce entries.
- New endpoints:
  - `GET /v1/metrics/node` — super-admin gated, returns `NodeSnapshot`.
  - `GET /v1/workloads` — returns one `WorkloadView` per service (filtered by
    membership for non-super-admins), joining the service config with the
    promoted deployment and a best-effort cgroup snapshot.
  - `GET /v1/services/{id}/requests` — Operator-gated, returns the per-service
    `AccessEntry` ring buffer newest-first.

## Consequences

- CPU% is reported as cumulative jiffies; the frontend computes deltas
  against a sample ring.
- The access log is in-process and lost on restart. That is acceptable for an
  operator console (recent traffic only); persistence is a later concern.
- Bridge-level access-log capture is deferred. The `AccessLogStore` API is in
  place so the bridge can begin appending once the byte-copy proxy is
  upgraded to a per-direction reader/tee.

## Alternatives Considered

- **External Prometheus + Loki**: rejected; control plane should expose
  honest read endpoints, not require a separate stack on the same node.
- **SQLite-backed access log**: rejected; per-request writes against the
  control-plane DB would compete with deploy/state operations.

## SSE Live Log Streaming (2026-05-27)

### New endpoint

`GET /v1/services/{service_id}/logs/stream` — Server-Sent Events, `Content-Type: text/event-stream`.

### SSE contract

Each SSE event carries exactly one log line in its `data:` field. No `id` or `event` fields are set. The stream never closes server-side; it ends only when the client disconnects.

### Mechanism

A per-client `LogTailer` (in `src/observability/logs.rs`) tails `{log_dir}/{service_name}.log` using a seek+read poll at 300ms intervals. On connect it emits the last 200 complete lines as backlog, then streams newly-appended lines live. File truncation/recreation is detected by comparing the current file length against the stored offset; on shrink the tailer resets to offset 0. A 15-second SSE keep-alive comment is sent to prevent proxy timeouts.

The handler spawns a `tokio::task` that owns the `LogTailer` and pushes `Result<Event, Infallible>` values into a bounded `mpsc` channel (capacity 256). The channel's `ReceiverStream` is handed directly to `axum::response::sse::Sse`. When the client disconnects, axum drops the stream, the channel send returns `Err`, and the tailer task exits cleanly.

### Auth

Same bearer token + `Operator` role check as the existing one-shot `GET /v1/services/{id}/logs` endpoint. Browser clients must use `fetch` + `ReadableStream` rather than `EventSource` because `EventSource` cannot send an `Authorization` header. Query-parameter tokens are deliberately not supported to avoid credentials appearing in access logs.

### Dependency

Added `tokio-stream = "0.1"` for `tokio_stream::wrappers::ReceiverStream`.

## References

- `docs/superpowers/plans/2026-05-25-observability.md`
- `docs/superpowers/specs/2026-05-25-observability.md`
- `docs/superpowers/plans/2026-05-27-live-log-streaming.md`
- `docs/superpowers/specs/2026-05-27-live-log-streaming-design.md`
