# ADR-024: Async Deployments With Per-Deployment Log Stream

- Status: Accepted
- Date: 2026-05-28
- Related: [ADR-010 Jobs and Scheduler](010-jobs-scheduler.md), [ADR-017 Service CRUD API](017-service-crud-api.md), [ADR-009 Observability](009-observability.md)

## Context

`POST /v1/deployments` currently runs the entire deploy pipeline (registry
auth, OCI pull, layer unpack, rootfs assembly, cgroup + namespace setup,
process exec, health check) inline on the request task. Real-world image
pulls and unpacks take tens of seconds and occasionally minutes; recent
failures hit ~32 s before returning a 500 with a single `tower_http` line
that gives the operator no way to see *where* the deploy died.

This breaks three things the operator console needs:

1. **Responsive UX.** The browser must wait the full deploy duration for a
   2xx/5xx. Operators cannot navigate away, refresh, or close the tab.
2. **Visible progress.** The frontend has no signal between "request sent"
   and "request finished" — no phase, no log line, no spinner backed by
   real state.
3. **Postmortem.** When a deploy fails, the only artifact is one HTTP
   error body. There is no per-deployment log to scroll through.

`DeploymentStatus` already models the phases (`Pending`, `Building`,
`Starting`, `Healthy`, `Failed`, `Stopped`), and `SqliteStore::fail_orphan_runs`
already proves the pattern for crash recovery on boot. The status column is
just never written by the coordinator until the very end of the synchronous
pipeline.

## Decision

Deployments become asynchronous, owned by the control-plane process, with
per-deployment text logs and an SSE tail endpoint.

1. **API contract.**
   - `POST /v1/projects/{pid}/services/{sid}/deployments` (and the existing
     `POST /v1/deployments`) immediately persists a `Pending` row and
     returns `202 Accepted` with the `Deployment` body. No deploy work is
     done on the request task.
   - `GET /v1/deployments/{id}` returns the current `Deployment` (status,
     timestamps, request).
   - `GET /v1/deployments/{id}/logs` streams the per-deployment log file
     as `text/event-stream`. Each event is one captured line, server
     timestamps included as event metadata. The stream emits a final
     terminal event when the deployment reaches `Healthy`, `Failed`, or
     `Stopped`, then closes.
   - The endpoint also accepts `?since=<byte_offset>` to resume after a
     reconnect; reconnects start from the saved offset and continue
     tailing.
2. **Execution.**
   - The handler calls `coordinator.spawn(deployment.id, plan)`, which
     `tokio::spawn`s a deploy task keyed by `deployment_id`. The task
     updates `DeploymentStatus` through the existing phases and writes
     log lines to a sink (see below). The handler does not await it.
   - Crash recovery: `SqliteStore::fail_orphan_deployments()` runs at
     boot (parallel to `fail_orphan_runs`) and marks any deployment left
     in `Pending`/`Building`/`Starting` as `Failed` with a synthetic last
     log line "control plane restarted; deployment aborted". This keeps
     the table honest after a crash.
3. **Logging.**
   - Log lines are appended to
     `<log_dir>/deployments/<deployment_id>.log` (`log_dir` is already
     defined in `AppConfig`). The file is opened `0600` with `create_new`
     by the deploy task; orphan recovery does not touch the file, so
     post-restart readers see whatever the crashed process flushed.
   - The deploy task wraps each pipeline phase in a function that writes
     a structured line: `<rfc3339_ts> <phase> <event>` (e.g. `2026-05-28T09:15:01Z
     OCI_PULL layer sha256:abc… (3/8) 12.4 MB`). Errors include the
     `Debug` repr of the inner error variant.
   - Secrets discipline: log emitters never print decrypted SOPS
     payloads, registry credentials, key authorizations, or SSH keys.
     Error wrappers strip the inner payload before logging.
4. **SSE tail.**
   - A new `crate::deploy::log_tail` module opens the file, optionally
     seeks to `since`, yields existing bytes line-by-line, and then
     `inotify`-watches the file for appends (using `tokio` polling fallback
     when inotify is unavailable, e.g. in test sandboxes).
   - The handler attaches its own state-poll loop that closes the SSE
     stream once the deployment row's status is terminal AND the file's
     final newline has been emitted, so the browser sees a clean EOF.
5. **Existing path retained.** `Deployment::status` semantics are
   unchanged; the only difference is who writes the transitions. The
   sync coordinator method remains for tests (`deploy_external_image_source`
   becomes a thin wrapper that spawns the task, awaits it, and returns
   the final deployment row).

## Consequences

- Easier: operator gets `202` in <200 ms with a deployment id. The UI can
  immediately open the log view and watch the deploy run.
- Easier: failures surface as durable log lines, not transient HTTP error
  bodies. Postmortem is a `cat` on `<log_dir>/deployments/<id>.log`.
- Easier: ingress promotion (route swap) still happens inside the deploy
  task; no behavior change for the routing layer.
- Harder: control plane now owns long-lived in-flight tasks. Graceful
  shutdown must drain them or transition them to `Failed` honestly.
- Harder: orphan recovery semantics expand to deployments — adds one
  SQLite query at boot and one new migration column if `failed_reason`
  text is added (out of scope for v1; the synthetic log line suffices).
- Harder: each deployment now writes one file under `log_dir`. A future
  retention/GC ADR may be needed, mirroring [ADR-022](022-oci-layer-cache.md)'s
  approach. For v1, files are retained until the deployment row is
  deleted.

## Alternatives Considered

- **Reuse the existing job system (ADR-010).** Rejected: `Job` is
  cron-shaped (schedule + repo) and would have to grow a deployment
  payload type, blurring the model. Deployment-owned `tokio::spawn`
  fits the existing crash-recovery shape (`fail_orphan_runs`) without
  cross-contaminating jobs.
- **WebSocket instead of SSE.** Rejected: deploy logs are one-directional
  server→client. SSE is HTTP/1.1-native, works through ingress, and the
  browser `EventSource` API gives reconnect-with-Last-Event-Id for free.
- **SQLite log table.** Rejected: structured query is not needed for a
  text tail; SQLite writes would contend with deployment status updates
  on the same DB while a layer unpack pegs the CPU. A flat file is the
  cheapest correct thing.
- **Synchronous endpoint + separate `/logs`.** Rejected: keeps the
  blocking HTTP behavior the operator already complained about.

## References

- [ADR-009 Observability](009-observability.md) — access log and per-workload runtime metrics.
- [ADR-010 Jobs and Scheduler](010-jobs-scheduler.md) — pattern for orphan recovery (`fail_orphan_runs`).
- [ADR-019 Per-Replica Runtime Filesystem Isolation](019-runtime-filesystem-isolation.md) — what each deploy phase has to set up.
- [`src/api/deployments.rs`](../../src/api/deployments.rs) — current sync handler.
- [`src/deploy/coordinator.rs`](../../src/deploy/coordinator.rs) — sync pipeline.
