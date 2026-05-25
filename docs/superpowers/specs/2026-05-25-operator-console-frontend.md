# Spec: Operator Console (Frontend) — companion to denia-backend-v1-deploy-runtime

Status: Draft · Date: 2026-05-25 · Frontend companion to
[`2026-05-24-denia-backend-v1-deploy-runtime.md`](../plans/2026-05-24-denia-backend-v1-deploy-runtime.md)

## Problem

The backend exposes the core control plane under `/v1` (services, deployments,
logs, metrics, lifecycle), but the web console only ships the scaffold demo
route. Operators have no UI to see services, trigger deploys, watch status, read
logs, or view metrics.

## Goal

The primary operator console: a services overview, a service detail view
(deployments timeline, logs, metrics), a deploy trigger, and a stop action. Built
on the Effect + Query layer; same-origin `/v1`, bearer-authed; DESIGN.md system.
This is the dashboard the design system was written for.

## Backend surface consumed

- `GET /v1/services`, `POST /v1/services`
- `POST /v1/deployments`
- `GET /v1/services/{id}/deployments`
- `GET /v1/services/{id}/logs`
- `GET /v1/services/{id}/metrics`
- `POST /v1/services/{id}/{action}` (currently `stop`)

## Decisions

- **Effect first.** Each endpoint is an `ApiClient` method (typed Effect + Schema
  decode); React consumes via `runQuery` in Query `queryFn`/`mutationFn`.
- **Routes** (`web/src/routes/`): `/services` (list), `/services/$serviceId`
  (detail with deployments / logs / metrics sections).
- **State as signal (DESIGN.md):** map `DeploymentStatus` to signal colors —
  `Healthy`->ok, `Pending/Building/Starting`->warn, `Failed`->fault (Breakdown
  violet), `Stopped`->muted. Steady pink reserved for the primary deploy action.
- **Logs:** poll `GET .../logs` on the detail view (interval, paused when tab
  hidden); render mono, tabular line numbers; no streaming in v1.
- **Metrics:** poll `GET .../metrics`; render compact mono readouts (CPU/mem) with
  tabular figures. No charts in v1 (charts are a later enhancement).
- **Project scoping:** if the Projects UI ships, the services list filters by the
  active `?project=`. Degrade gracefully if projects are absent.

## Components / data flow

- `ApiClient` methods + `Schema`: `Service`, `Deployment`, `MetricSnapshot`,
  `listServices`, `getServiceDeployments(id)`, `getServiceLogs(id)`,
  `getServiceMetrics(id)`, `createDeployment(input)`, `stopService(id)`.
- `web/src/routes/services/index.tsx` — services `.panel` list with a status
  signal per service; deploy/stop actions.
- `web/src/routes/services/$serviceId.tsx` — detail: deployments timeline
  (`['services', id, 'deployments']`), logs panel (polled), metrics readout
  (polled), deploy button -> mutation, stop button -> mutation.
- Mutations invalidate the relevant service queries.

## Errors / edge cases

- Deploy of unknown service -> 404 surfaced inline.
- Empty states: no services; no deployments yet; empty logs; no promoted
  deployment (metrics empty).
- Polling backs off / pauses on error and when the document is hidden.
- 401 -> auth-needed banner (RBAC login is sub-project C).

## Success criteria

- Operator sees all services and their current status at a glance.
- Can open a service, trigger a deploy, watch status change, read recent logs,
  and see current metrics.
- Can stop a running service.
- All network state flows through Effect + Query; no raw fetch in components.

## Testing

- `@effect/vitest`: each `ApiClient` method + Schema decode against a stub
  `HttpClient`; error mapping (404, decode failure).
- `@testing-library/react`: list renders + status signal mapping; detail renders
  deployments/logs/metrics from mocked queries; deploy/stop call mutations.

## Out of scope

Metric charts, log streaming/search, request-tracing (sub-project D), RBAC,
project CRUD (its own companion). Backend behaviour (its own plan).
