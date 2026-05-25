# Spec: Observability — node metrics, running workloads, request logs

Status: Draft · Date: 2026-05-25 · Sub-project D of the TODO decomposition.

## Problem

Denia exposes per-service cgroup metrics (`/v1/services/{id}/metrics`) and
stdout/stderr logs (`/v1/services/{id}/logs`), but the operator has no
**node-level** view (host CPU/mem/disk/load), no single **running-workloads**
roll-up (which services are live + their live usage), and no **request** (access)
visibility — the loopback bridge is a raw byte proxy that records nothing about
the HTTP traffic it carries.

## Goal

Three read-only observability surfaces, same `/v1` bearer-protected API, no new
runtime privileges:

1. **Node metrics** — host CPU jiffies, memory, load average, disk usage.
2. **Running workloads** — one row per service: promoted deployment + status +
   latest cgroup snapshot.
3. **Per-service request logs** — method, path, status, bytes, duration captured
   at the Denia bridge.

## Decisions

- **Node metrics via procfs/statvfs, no new crate.** A `NodeMetricsReader` reads
  `/proc/stat` (aggregate `cpu` line), `/proc/meminfo` (`MemTotal`/`MemAvailable`),
  `/proc/loadavg` (1/5/15), and `libc::statvfs` on a configured disk path. Matches
  the existing `CgroupMetricsReader` fs-read pattern. `libc` is already a
  dependency.
- **Cumulative counters, point-in-time.** CPU is returned as cumulative jiffies
  (`total`, `idle`) exactly like `MetricSnapshot.cpu_usage_usec` is cumulative
  µs; the client computes the delta between polls for a percentage. No server-side
  sampler, no time-series table, no retention/pruning.
- **Running workloads = store join + live cgroup read.** `/v1/workloads` lists
  each service with its promoted deployment id, `DeploymentStatus`, and the
  current `MetricSnapshot` (when a promoted deployment exists). The deployment id
  comes from `promoted_deployment(service_id)`; its status is looked up via
  `list_deployments(service_id)`. `running := status == Healthy`. Services with no
  promotion report `running: false`, `status/deployment_id/snapshot: None`.
- **Request logs by tapping the bridge.** `LoopbackBridge` parses the first
  HTTP/1.1 request line (method + target) on the inbound stream and the response
  status line + `Content-Length` on the outbound stream, times the exchange, and
  appends one `AccessEntry` to a per-service access log via `AccessLogStore`
  (sibling of `LogStore`, file `{service}.access.log`, JSON-lines). Denia-owned;
  no Traefik log coupling.
- **One entry per connection.** The bridge logs the first request/response pair
  per accepted connection; it does not parse pipelined/keep-alive subsequent
  requests on the same connection. Documented limitation, acceptable for a
  single-node operator console.
- **Bytes from `Content-Length` only.** A response with no `Content-Length`
  (chunked/streamed) records `bytes: 0` — the bridge does not count streamed body
  bytes. Documented limitation.
- **Design:** request bodies are never read or stored — only the request line,
  status, byte count, and duration. No header capture (avoids leaking
  `Authorization`/cookies).

## Backend surface added

- `GET /v1/metrics/node` -> `NodeSnapshot { cpu_total_jiffies, cpu_idle_jiffies,
  mem_total_bytes, mem_available_bytes, load1, load5, load15, disk_total_bytes,
  disk_available_bytes }`.
- `GET /v1/workloads` -> `Vec<WorkloadView { service_id, service_name,
  project_id, running, deployment_id, status, snapshot }>`.
- `GET /v1/services/{id}/requests` -> `Vec<AccessEntry { ts, method, path,
  status, bytes, duration_ms }>` (newest-first, capped).

## Components / data flow

- `src/node_metrics.rs` (new): pure parsers `parse_proc_stat_cpu`,
  `parse_meminfo`, `parse_loadavg`; `NodeMetricsReader::read()` composes them +
  `statvfs`. `NodeSnapshot` (serde).
- `src/access_log.rs` (new): `AccessLogStore { dir }` with `append(service,
  &AccessEntry)` and `read_recent(service, limit)`; `AccessEntry` (serde,
  JSON-line). Parser helpers `parse_request_line`, `parse_status_line`.
- `src/bridge.rs`: `LoopbackBridge` gains an `AccessLogStore` + service name;
  `serve_one` replaces `copy_bidirectional` with a tee that captures the first
  request line / response status, records timing, then streams the rest. The
  byte-for-byte proxy behaviour is unchanged for the client.
- `src/app.rs`: three handlers + routes (bearer-protected; RBAC `viewer` read
  when present). `WorkloadView` joins `list_services` + `promoted_deployment` +
  `CgroupMetricsReader`.
- `src/config.rs`: `DENIA_NODE_DISK_PATH` (default = `runtime_dir`) for the
  `statvfs` target; access logs reuse `log_dir`.

## Errors / edge cases

- Missing/unreadable procfs file -> `NodeMetricsError`; `/v1/metrics/node` maps
  to 500 with a typed message (never a panic).
- Service with no promoted deployment -> `WorkloadView { running: false,
  snapshot: None }` (200, not 404).
- No access log yet for a service -> empty list (mirror `service_logs`
  NotFound -> `[]`).
- Malformed request/status line at the bridge -> skip the access entry, never
  break the proxy stream (observability must not degrade traffic).
- Bridge must never log header values (no `Authorization`/`Cookie` capture).

## Success criteria

- Operator sees live host CPU%/mem/disk/load (client delta over polls).
- `/v1/workloads` shows which services are running with current CPU/mem.
- Hitting a deployed service produces request-log rows (method/path/status/
  duration) visible per service; bridge throughput is unaffected.

## Testing

- Unit: procfs/meminfo/loadavg parsers against fixture strings; `parse_request_line`
  / `parse_status_line` happy + malformed; `AccessLogStore` round-trip.
- Bridge: a fake upstream Unix socket returns a canned HTTP response; assert the
  proxied bytes are unchanged AND one `AccessEntry` with the right method/path/
  status/bytes is recorded.
- API (`tests/backend_contract.rs`): `/v1/metrics/node` shape; `/v1/workloads`
  with and without a promoted deployment; `/v1/services/{id}/requests` returns
  recorded entries.

## Out of scope

Time-series storage / charts history (client keeps a ring), per-request body or
header capture, distributed tracing, alerting/thresholds, log shipping, Traefik
access-log ingestion. Frontend is the companion spec
`2026-05-25-observability-frontend.md`.
