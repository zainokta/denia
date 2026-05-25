# Spec: Observability UI (Frontend) — companion to observability

Status: Draft · Date: 2026-05-25 · Frontend companion to
[`2026-05-25-observability.md`](2026-05-25-observability.md)

## Problem

The backend adds node metrics (`/v1/metrics/node`), a running-workloads roll-up
(`/v1/workloads`), and per-service request logs
(`/v1/services/{id}/requests`). The console has no UI to see host health, what is
running, or the HTTP traffic a service is serving.

## Goal

An observability surface in the console: a node dashboard (CPU%/mem/disk/load), a
running-workloads table, and a per-service request-log viewer. Effect + Query
layer, same-origin `/v1`, DESIGN.md system, role-gated read via RBAC when present.

## Backend surface consumed

- `GET /v1/metrics/node` -> cumulative counters (`cpu_total_jiffies`,
  `cpu_idle_jiffies`, `mem_*`, `load*`, `disk_*`).
- `GET /v1/workloads` -> per-service running + status + latest snapshot.
- `GET /v1/services/{id}/requests` -> recent access entries.

## Decisions

- **Effect first:** node/workloads/requests are `ApiClient` methods (typed
  Effects + Schema); React via `runQuery`/Query. Schema `NodeSnapshot`,
  `WorkloadView`, `AccessEntry`.
- **Client-side deltas, client-side ring.** The server returns cumulative CPU
  jiffies and cumulative per-service `cpu_usage_usec`; the client keeps the last
  N polled samples in a small ring (component state) and renders CPU% as the
  delta over the poll interval. Never display a server "percentage" — the backend
  doesn't compute one.
- **Routes** (`web/src/routes/`): `/observability` (node dashboard + workloads
  table). Request logs render as a panel on the existing service detail view
  (sub-project B / operator-console route), not a new top-level route.
- **Status as signal (DESIGN.md):** workload status reuses the console's
  `StatusSignal` — `Healthy`->ok, `Pending`/`Building`/`Starting`->warn,
  `Failed`->Breakdown violet, `Stopped`->muted.
- **Polling:** node + workloads Queries poll (~2s) while the tab is visible,
  paused when hidden; request logs poll slower (~5s). No websockets.
- **Tabular numbers:** all metric values use `.tnum`; bytes rendered with a
  shared `formatBytes`, durations as `ms`.
- **Read-only:** these views require only `viewer`; no mutations, no gating
  beyond the auth guard (RBAC companion handles the redirect).

## Components / data flow

- `ApiClient`: `getNodeMetrics`, `listWorkloads`, `listServiceRequests(id)`.
- `web/src/components/NodeMetricsPanel.tsx` — CPU%/mem/disk/load cards from the
  cumulative-counter ring; `.panel` + `.tnum`.
- `web/src/components/WorkloadsTable.tsx` — rows: service name, project,
  `StatusSignal`, CPU (delta), memory; empty-state when nothing runs.
- `web/src/components/RequestLogTable.tsx` — method, path, status signal
  (2xx ok / 4xx warn / 5xx violet), bytes, duration; tabular times.
- `web/src/routes/observability.tsx` — node panel + workloads table.
- `web/src/components/Header.tsx` — `/observability` nav link.
- Request-log panel mounted on the service detail route.

## Errors / edge cases

- First poll has no previous sample -> CPU% shows `—` until a second sample.
- `/v1/metrics/node` 500 (procfs read error) -> inline "node metrics
  unavailable", dashboard does not crash; workloads still render.
- No workloads / no requests -> calm empty states.
- 401 -> auth-needed (RBAC companion handles the redirect).

## Success criteria

- Operator sees host CPU%/mem/disk/load updating live.
- Workloads table shows which services run with current CPU/mem + status signal.
- Hitting a service shows request rows (method/path/status/duration) on its
  detail view.

## Testing

- `@effect/vitest`: node/workloads/requests `ApiClient` methods + Schema decode;
  500 mapping for node metrics.
- `@testing-library/react`: `NodeMetricsPanel` computes a CPU% delta from two
  injected samples (and shows `—` on the first); `WorkloadsTable` renders status
  signals + empty-state; `RequestLogTable` maps 2xx/4xx/5xx to the right signal.

## Out of scope

Historical charts/time-series, alerting/thresholds, log download/streaming, header
or body inspection, request search/filter. Backend behaviour (its own spec).
Builds on the operator-console (`StatusSignal`), projects (active project), and
RBAC (read gating) companions.
