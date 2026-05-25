# Observability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add node metrics (host CPU/mem/disk/load), a running-workloads roll-up, and per-service request (access) logs captured at the Denia bridge — all read-only `/v1` endpoints.

**Architecture:** A `NodeMetricsReader` (procfs + `libc::statvfs`, no new crate) returns cumulative counters point-in-time. `/v1/workloads` joins the store's services + promoted deployments + live `CgroupMetricsReader` snapshots. `LoopbackBridge` is taught to tee the first HTTP request line + response status and append an `AccessEntry` to a per-service access log, leaving the byte proxy behaviour unchanged.

**Tech Stack:** Rust 2024, axum 0.8, rusqlite, tokio, serde, `libc`, thiserror. Spec: `docs/superpowers/specs/2026-05-25-observability.md`. **Depends on sub-project B (Projects)** for `project_id` on `WorkloadView`.

---

## File Structure

- `src/node_metrics.rs` (new) — procfs/statvfs reader + `NodeSnapshot`.
- `src/access_log.rs` (new) — `AccessLogStore`, `AccessEntry`, line parsers.
- `src/bridge.rs` — tee request/response in `LoopbackBridge`.
- `src/app.rs` — `/v1/metrics/node`, `/v1/workloads`, `/v1/services/{id}/requests`.
- `src/config.rs` — `DENIA_NODE_DISK_PATH`.
- `src/lib.rs` — `pub mod node_metrics; pub mod access_log;`.
- `docs/adr/010-observability.md` + `docs/adr/README.md`, `AGENTS.md`.
- Tests colocated + `tests/backend_contract.rs`.

Commit after each task.

---

## Task 1: Node metrics parsers + reader

**Files:**
- Create: `src/node_metrics.rs`
- Modify: `src/lib.rs`, `src/config.rs`
- Test: `src/node_metrics.rs`

- [ ] **Step 1: Write failing tests** — `parse_proc_stat_cpu("cpu  100 0 50 800 ...")` returns `total=950+..` and `idle=800`; `parse_meminfo` with `MemTotal: 16384 kB`/`MemAvailable: 8192 kB` returns bytes (`*1024`); `parse_loadavg("0.10 0.20 0.30 ...")` returns `(0.10,0.20,0.30)`; empty/garbage -> `NodeMetricsError`.
- [ ] **Step 2: Run** `cargo test node_metrics` → FAIL.
- [ ] **Step 3: Implement** — `NodeSnapshot { cpu_total_jiffies, cpu_idle_jiffies, mem_total_bytes, mem_available_bytes, load1, load5, load15, disk_total_bytes, disk_available_bytes }` (serde); pure parsers; `NodeMetricsReader { proc_root, disk_path }` with `read()` composing the parsers + a `statvfs` helper (`libc::statvfs`, `disk_total = f_blocks*f_frsize`, `disk_available = f_bavail*f_frsize`). `NodeMetricsError` (`Io`, `Empty`, `InvalidInteger`, `Missing{field}`). Add `disk_path` to `AppConfig` from `DENIA_NODE_DISK_PATH` (default `runtime_dir`); mirror in `AppConfig::for_test`. Register modules in `lib.rs`.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** — `git commit -m "feat(metrics): node procfs/statvfs reader"`

---

## Task 2: Access-log store + line parsers

**Files:**
- Create: `src/access_log.rs`
- Modify: `src/lib.rs`
- Test: `src/access_log.rs`

- [ ] **Step 1: Write failing tests** — `parse_request_line("GET /api/x HTTP/1.1")` -> `("GET","/api/x")`; `parse_status_line("HTTP/1.1 200 OK")` -> `200`; malformed -> `None`. `AccessLogStore` round-trip: `append` then `read_recent` returns the entry newest-first; reading a missing service -> `Ok(vec![])`.
- [ ] **Step 2: Run** `cargo test access_log` → FAIL.
- [ ] **Step 3: Implement** — `AccessEntry { ts: String (rfc3339), method: String, path: String, status: u16, bytes: u64, duration_ms: u64 }` (serde). `AccessLogStore { dir }`: `append(service, &AccessEntry)` writes one JSON line to `{service}.access.log` (create dir; reuse `LogStore` file conventions); `read_recent(service, limit)` reads, parses JSON-lines, newest-first, capped, NotFound -> empty. `parse_request_line`/`parse_status_line` return `Option`.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** — `git commit -m "feat(logs): per-service access log store"`

---

## Task 3: Bridge request/response tee

**Files:**
- Modify: `src/bridge.rs`
- Test: `src/bridge.rs`

- [ ] **Step 1: Write failing test** — bind a `LoopbackBridge` whose upstream is a fake Unix socket that, on connect, reads the request and writes `HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello`. Client connects, sends `GET /ping HTTP/1.1\r\nHost: x\r\n\r\n`, reads back the full canned response unchanged; assert the injected `AccessLogStore` recorded one entry `{method:"GET", path:"/ping", status:200, bytes:5}`.
- [ ] **Step 2: Run** `cargo test bridge` → FAIL.
- [ ] **Step 3: Implement** — `LoopbackBridge` gains `service_name: String` + `access_log: AccessLogStore`. Thread the access-log dir into `LoopbackBridgeSupervisor` at construction (add a `LoopbackBridgeSupervisor::with_log_dir(log_dir)`; `AppState::new` passes `config.log_dir.clone()` instead of `LoopbackBridgeSupervisor::default()`). In `activate`, build the per-service `AccessLogStore` from the dir + `target.service_name` and pass it (plus `target.service_name`) into `LoopbackBridge::bind`. Replace `copy_bidirectional` in `serve_one` with a manual proxy: buffer bytes up to the first `\r\n` on each direction to extract the request line / status line (and scan response headers for `Content-Length`), record `Instant::now()` start -> `duration_ms`, append the `AccessEntry`, then stream all buffered + remaining bytes both ways. On parse failure, skip the entry and proxy normally. A response with no `Content-Length` (chunked/streamed) records `bytes: 0`. Never inspect headers other than `Content-Length`.
- [ ] **Step 4: Run** → PASS (and existing bridge tests still green).
- [ ] **Step 5: Commit** — `git commit -m "feat(bridge): tee request line for access logs"`

---

## Task 4: Node + workloads + requests endpoints

**Files:**
- Modify: `src/app.rs`
- Test: `tests/backend_contract.rs`

- [ ] **Step 1: Write failing tests** — `GET /v1/metrics/node` returns 200 with the `NodeSnapshot` fields; `GET /v1/workloads` returns a row per service with `running=false`/`snapshot=null` when nothing is promoted, and `running=true` + a snapshot when a deployment is promoted (use the existing fake-runtime deploy path); `GET /v1/services/{id}/requests` returns recorded entries and `[]` for a service with none; unauthenticated -> 401.
- [ ] **Step 2: Run** `cargo test --test backend_contract` → FAIL.
- [ ] **Step 3: Implement** — add routes under the protected router: `GET /metrics/node` (`NodeMetricsReader::new("/proc", config.disk_path.clone())`), `GET /workloads`, `GET /services/{service_id}/requests`. `WorkloadView { service_id, service_name, project_id, running, deployment_id: Option<Uuid>, status: Option<DeploymentStatus>, snapshot: Option<MetricSnapshot> }`; handler iterates `list_services`, calls `promoted_deployment(service_id)` for the `deployment_id`, looks the status up via `list_deployments(service_id)` find-by-id, sets `running = status == Some(DeploymentStatus::Healthy)`, and reads `CgroupMetricsReader` when promoted (ignore per-service read errors -> `snapshot: None`, `running`/`status` still reported). No promoted deployment -> `running: false`, `deployment_id/status/snapshot: None`. `requests` handler uses `AccessLogStore::read_recent(&service.name, 200)`. New `ApiError` arm (`NodeMetrics`) mapped to 500. Gate `viewer` when RBAC present.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** — `git commit -m "feat(api): node metrics, workloads, request logs"`

---

## Task 5: ADR + docs

**Files:**
- Create: `docs/adr/010-observability.md`
- Modify: `docs/adr/README.md`, `AGENTS.md`

- [ ] **Step 1:** ADR-010 (Proposed): procfs/statvfs node metrics (no crate, cumulative point-in-time), workloads roll-up via store+cgroup join, request logs by tapping the bridge (request line + status only, no headers/body, one entry per connection). Alternatives: `sysinfo` crate; Traefik access-log scrape; server-side time-series.
- [ ] **Step 2:** Index row in `README.md`; `AGENTS.md` note (`DENIA_NODE_DISK_PATH`, access-log files, bridge logs no header values).
- [ ] **Step 3: Commit** — `git commit -m "docs: ADR-010 observability"`

---

## Final Verification

- [ ] `cargo build`, `cargo fmt --all`, `cargo clippy --all-targets --all-features`.
- [ ] `cargo test` — node parsers, access-log store, bridge tee, API green.
- [ ] Manual (backend running): `GET /v1/metrics/node` twice -> CPU jiffies advance;
  deploy a service, `GET /v1/workloads` shows it running with a snapshot; curl the
  service through its bridge port, then `GET /v1/services/{id}/requests` shows the
  request with status + duration; confirm the proxied response body is byte-identical.

## Notes

- Observability must never degrade traffic: a parse failure at the bridge skips
  the log entry and proxies normally.
- Never capture header values at the bridge (no `Authorization`/`Cookie`).
- CPU/mem are cumulative counters; the client computes deltas (mirrors the
  existing `MetricSnapshot` contract).
- Builds on B (`project_id` on `WorkloadView`); RBAC gates the endpoints (`viewer`
  read) when present.
- Frontend is the companion plan `2026-05-25-observability-frontend.md`.
