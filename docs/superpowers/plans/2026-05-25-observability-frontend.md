# Observability UI (Frontend) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an observability UI: a node dashboard (CPU%/mem/disk/load), a running-workloads table, and a per-service request-log viewer, on the Effect + TanStack Query layer.

**Architecture:** New `ApiClient` node/workloads/requests methods (typed Effects + Schema) bridged into Query via `runQuery`. A `/observability` file route renders the node panel + workloads table; the request-log table mounts on the service detail view. CPU% is computed client-side from cumulative counters held in a small sample ring; status renders as DESIGN.md signal colors.

**Tech Stack:** TanStack Start/Router/Query, React 19, Effect (`effect@beta`), `@effect/vitest`, `@testing-library/react`. Spec: `docs/superpowers/specs/2026-05-25-observability-frontend.md`. Depends on the observability backend + operator-console (`StatusSignal`) + projects (B) + RBAC (C) companions.

---

## File Structure

- `web/src/effect/schema.ts` — `NodeSnapshot`, `WorkloadView`, `AccessEntry`.
- `web/src/effect/api-client.ts` — node/workloads/requests methods.
- `web/src/lib/metrics.ts` — `cpuPercent(prev, next)`, `formatBytes`.
- `web/src/components/NodeMetricsPanel.tsx`, `WorkloadsTable.tsx`, `RequestLogTable.tsx`.
- `web/src/routes/observability.tsx` — node panel + workloads.
- `web/src/components/Header.tsx` — `/observability` nav link.
- Service detail route — mount `RequestLogTable`.
- Tests colocated.

Commit after each task.

---

## Task 1: Schemas

**Files:**
- Modify: `web/src/effect/schema.ts`
- Test: `web/src/effect/api-client.test.ts`

- [ ] **Step 1: Write failing test** — decode a `NodeSnapshot` (all numeric fields, `load*` floats), a `WorkloadView` (with `snapshot: null` + `running: false`, and with a snapshot + `status: "Healthy"`), and an `AccessEntry`.
- [ ] **Step 2: Run** `pnpm test` → FAIL.
- [ ] **Step 3: Implement** — `NodeSnapshot` (`Schema.Class`, all `Schema.Number`); `WorkloadView` with `snapshot: Schema.NullOr(MetricSnapshot)`, `status: Schema.NullOr(DeploymentStatus)`, `deployment_id: Schema.NullOr(Schema.String)`, `running: Schema.Boolean`; `AccessEntry { ts, method, path, status: Schema.Number, bytes, duration_ms }`. Arrays `Workloads`, `AccessEntries`. Reuse existing `MetricSnapshot`/`DeploymentStatus` if present, else add.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** — `git commit -m "feat(web): observability schemas"`

---

## Task 2: ApiClient methods

**Files:**
- Modify: `web/src/effect/api-client.ts`
- Test: `web/src/effect/api-client.test.ts`

- [ ] **Step 1: Write failing tests** — `getNodeMetrics` decodes from a stub `HttpClient`; `listWorkloads` decodes an array; `listServiceRequests(id)` decodes entries; a 500 on node maps to a distinguishable `ApiError`.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** — add `getNodeMetrics` (`GET /v1/metrics/node`), `listWorkloads` (`GET /v1/workloads`), `listServiceRequests(id)` (`GET /v1/services/{id}/requests`) to the `ApiClient` shape + `ApiClientLive`. Bearer from `AppConfig`; Schema decode; carry status on `ApiError`.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** — `git commit -m "feat(web): ApiClient observability methods"`

---

## Task 3: metrics helpers

**Files:**
- Create: `web/src/lib/metrics.ts`
- Test: `web/src/lib/metrics.test.ts`

- [ ] **Step 1: Write failing test** — `cpuPercent({total:1000,idle:800},{total:1100,idle:850})` -> `50` (busy 50/100); equal samples -> `0`; `formatBytes(1024)` -> `"1.0 KiB"`, `formatBytes(0)` -> `"0 B"`.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** — `cpuPercent(prev, next)` = `(1 - dIdle/dTotal) * 100` clamped `[0,100]`, `0` when `dTotal<=0`; `formatBytes(n)` binary units with one decimal. No `any`/`as`.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** — `git commit -m "feat(web): cpu/bytes metric helpers"`

---

## Task 4: NodeMetricsPanel

**Files:**
- Create: `web/src/components/NodeMetricsPanel.tsx`
- Test: `web/src/components/NodeMetricsPanel.test.tsx`

- [ ] **Step 1: Write failing test** — given one sample, CPU card shows `—`; given two samples, shows the `cpuPercent` value; mem/disk render via `formatBytes`; load shows `load1/5/15`.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** — component takes the current `NodeSnapshot` and a previous one (caller holds a ring); `.panel` cards (CPU%, mem used/total, disk used/total, load) with `.tnum`. No color literals; mem/disk pressure can use `.signal-warn` past a threshold.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** — `git commit -m "feat(web): NodeMetricsPanel"`

---

## Task 5: WorkloadsTable + RequestLogTable

**Files:**
- Create: `web/src/components/WorkloadsTable.tsx`, `web/src/components/RequestLogTable.tsx`
- Test: colocated `.test.tsx`

- [ ] **Step 1: Write failing tests** — `WorkloadsTable` renders a row per workload with `StatusSignal` (Healthy->ok, Failed->violet) + CPU/mem, and an empty-state for `[]`; `RequestLogTable` maps status 200->ok, 404->warn, 500->violet and shows method/path/duration.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** — `WorkloadsTable` reuses the console `StatusSignal`; CPU delta computed from a per-row sample ring (or shows `—` on first poll); `formatBytes` for memory. `RequestLogTable` maps the status-code band to a signal class, renders `.tnum` times. No color literals.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** — `git commit -m "feat(web): workloads + request-log tables"`

---

## Task 6: Observability route + wiring

**Files:**
- Create: `web/src/routes/observability.tsx`
- Modify: `web/src/components/Header.tsx`, service detail route
- Test: `web/src/routes/observability.test.tsx`

- [ ] **Step 1: Write failing test** — `/observability` renders the node panel (from a mocked `['node-metrics']` Query) and the workloads table (`['workloads']`); the service detail view renders `RequestLogTable` from `['services', id, 'requests']`.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** — `createFileRoute('/observability')`; `['node-metrics']` + `['workloads']` Queries with `refetchInterval ~2s` paused when the tab is hidden; hold the last N node samples in route state for the panel ring. Add a `/observability` nav link in `Header`. On the service detail route, add a request-log `.panel` with `RequestLogTable` from a `['services', id, 'requests']` Query (`refetchInterval ~5s`). Node 500 -> inline "node metrics unavailable" without crashing workloads.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** — `git commit -m "feat(web): observability route + request log panel"`

---

## Final Verification

- [ ] `cd web && pnpm typecheck` (no `any`/`as`).
- [ ] `cd web && pnpm test` — schemas, ApiClient, helpers, panel, tables, route green.
- [ ] `pnpm build`.
- [ ] Manual (backend running): open `/observability` -> CPU% updates after the
  second poll, mem/disk/load show; deploy a service -> it appears in workloads
  running with a snapshot; curl the service, open its detail -> request rows show
  method/path/status/duration; kill procfs access (simulate 500) -> inline node
  error, workloads still render.

## Notes

- Never display a server CPU "percentage" — compute it client-side from
  cumulative counters (mirrors the backend contract).
- Read-only surface: `viewer` is sufficient; no mutations or action gating.
- Builds on operator-console (`StatusSignal`), projects (active project), RBAC
  (read gating); sequence after them.
