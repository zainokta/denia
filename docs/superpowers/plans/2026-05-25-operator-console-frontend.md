# Operator Console (Frontend) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the operator console: services list, service detail (deployments, logs, metrics), deploy trigger, and stop action, on the Effect + TanStack Query layer.

**Architecture:** New `ApiClient` methods (typed Effects + Schema) for the `/v1` service/deployment/log/metric endpoints, bridged into Query via `runQuery`. File-based routes `/services` and `/services/$serviceId`. Deployment status renders as DESIGN.md signal colors. Logs and metrics poll on the detail view.

**Tech Stack:** TanStack Start/Router/Query, React 19, Effect (`effect@beta`), `@effect/vitest`, `@testing-library/react`, Tailwind v4. Spec: `docs/superpowers/specs/2026-05-25-operator-console-frontend.md`. Depends on the backend `/v1` control plane.

---

## File Structure

- `web/src/effect/schema.ts` — add `Service`, `Deployment`, `MetricSnapshot` (+ status enum).
- `web/src/effect/api-client.ts` — add service/deployment/log/metric methods.
- `web/src/routes/services/index.tsx` — services list + deploy/stop.
- `web/src/routes/services/$serviceId.tsx` — detail (deployments/logs/metrics).
- `web/src/components/StatusSignal.tsx` — maps `DeploymentStatus` -> `.signal-*`.
- `web/src/components/Header.tsx` — add `/services` nav link.
- Tests colocated.

Commit after each task.

---

## Task 1: Domain schemas

**Files:**
- Modify: `web/src/effect/schema.ts`
- Test: `web/src/effect/api-client.test.ts`

- [ ] **Step 1: Write failing test** — decode a `Service` and a `Deployment` sample; assert fields + status union.
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement** — `Schema.Class` for `Service` (id, project_id, name, domains, internal_port, ...), `Deployment` (id, service_id, status, created_at), `MetricSnapshot`. Status as `Schema.Literal('Pending','Building','Starting','Healthy','Failed','Stopped')`. Arrays `Services`, `Deployments`, `Metrics`.
- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** — `git commit -m "feat(web): console domain schemas"`

---

## Task 2: ApiClient service/deploy/log/metric methods

**Files:**
- Modify: `web/src/effect/api-client.ts`
- Test: `web/src/effect/api-client.test.ts`

- [ ] **Step 1: Write failing tests** — `listServices` decodes from stub `HttpClient`; `createDeployment` POSTs; a 404 maps to `ApiError`.
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement** — add to `ApiClient` shape + `ApiClientLive`:
  `listServices`, `getServiceDeployments(id)`, `getServiceLogs(id)` (-> `ReadonlyArray<string>`), `getServiceMetrics(id)`, `createDeployment(input)`, `stopService(id)` (`POST /v1/services/{id}/stop`). Bearer header from `AppConfig`; Schema decode; typed errors (carry status for 404/409).
- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** — `git commit -m "feat(web): ApiClient console methods"`

---

## Task 3: StatusSignal component

**Files:**
- Create: `web/src/components/StatusSignal.tsx`
- Test: `web/src/components/StatusSignal.test.tsx`

- [ ] **Step 1: Write failing test** — `Healthy`->`signal-ok`, `Failed`->`signal-fault`, `Building`->`signal-warn`, `Stopped`->muted.
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement** — pure mapping component rendering `<span className={"signal signal-..."}/>` + label. No color literals; use the DESIGN.md classes.
- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** — `git commit -m "feat(web): deployment StatusSignal"`

---

## Task 4: Services list route

**Files:**
- Create: `web/src/routes/services/index.tsx`
- Test: `web/src/routes/services/index.test.tsx`

- [ ] **Step 1: Write failing test** — renders services from a mocked Query with their status signal; deploy button calls the mutation.
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement** — `createFileRoute('/services/')`; `['services']` Query (filter by `?project=` if present); flat `.panel` rows: name, domains, `StatusSignal`, deploy `.btn-primary`, stop `.btn`. Mutations invalidate `['services']`. Empty-state copy.
- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** — `git commit -m "feat(web): services list route"`

---

## Task 5: Service detail — deployments timeline

**Files:**
- Create: `web/src/routes/services/$serviceId.tsx`
- Test: `web/src/routes/services/$serviceId.test.tsx`

- [ ] **Step 1: Write failing test** — renders the deployments list (newest first) with status signals + tabular timestamps.
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement** — `createFileRoute('/services/$serviceId')`; `['services', id, 'deployments']` Query; timeline `.panel`; deploy + stop actions here too.
- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** — `git commit -m "feat(web): service detail deployments"`

---

## Task 6: Service detail — logs (polled)

**Files:**
- Modify: `web/src/routes/services/$serviceId.tsx`
- Test: same test file

- [ ] **Step 1: Write failing test** — logs panel renders mono lines from a mocked logs Query.
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement** — `['services', id, 'logs']` Query with `refetchInterval` (e.g. 3s), `refetchIntervalInBackground: false`; render mono, tabular line numbers; empty-state when no logs.
- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** — `git commit -m "feat(web): service logs panel"`

---

## Task 7: Service detail — metrics (polled)

**Files:**
- Modify: `web/src/routes/services/$serviceId.tsx`
- Test: same test file

- [ ] **Step 1: Write failing test** — metrics readout renders CPU/mem from a mocked metrics Query with tabular figures.
- [ ] **Step 2: Run, verify fail.**
- [ ] **Step 3: Implement** — `['services', id, 'metrics']` Query polled; compact mono readout; empty-state when no promoted deployment. No charts.
- [ ] **Step 4: Run, verify pass.**
- [ ] **Step 5: Commit** — `git commit -m "feat(web): service metrics readout"`

---

## Final Verification

- [ ] `cd web && pnpm typecheck` (no `any`/`as`).
- [ ] `cd web && pnpm test` — schemas, ApiClient, StatusSignal, routes green.
- [ ] `pnpm build`.
- [ ] Manual (`pnpm dev`, backend running): list services, deploy one, watch the
  status signal change, read logs, see metrics, stop it.

## Notes

- Polling intervals pause when the tab is hidden; back off on error.
- Honor DESIGN.md: status is the only color; deploy = pink primary, fault =
  violet. No charts/streaming in v1.
- Depends on the backend `/v1` endpoints; coordinate with that plan.
