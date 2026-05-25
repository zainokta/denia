# Ingress + TLS UI (Frontend) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an ingress view (routes table + raw Traefik YAML) and a per-service TLS toggle to the console, on the Effect + TanStack Query layer.

**Architecture:** New `ApiClient.listRoutes` (Schema-decoded) and `getIngressConfig` (raw text) bridged into Query via `runQuery`. A `/ingress` route renders the routes table + collapsible YAML. A `TlsToggle` on the console service-detail view PUTs the service with `tls_enabled` flipped.

**Tech Stack:** TanStack Start/Router/Query, React 19, Effect (`effect@beta`), `@effect/vitest`, `@testing-library/react`. Spec: `docs/superpowers/specs/2026-05-25-ingress-tls-frontend.md`. Depends on backend `/v1/ingress/*` + service `tls_enabled`.

---

## File Structure

- `web/src/effect/schema.ts` — add `RouteView`.
- `web/src/effect/api-client.ts` — add `listRoutes`, `getIngressConfig` (raw text).
- `web/src/routes/ingress.tsx` — routes table + raw YAML panel.
- `web/src/components/TlsToggle.tsx` — per-service TLS switch.
- `web/src/routes/services/$serviceId.tsx` — mount `TlsToggle` (console companion).
- `web/src/components/Header.tsx` — `/ingress` nav link.
- Tests colocated.

Commit after each task.

---

## Task 1: RouteView schema

**Files:**
- Modify: `web/src/effect/schema.ts`
- Test: `web/src/effect/api-client.test.ts`

- [ ] **Step 1: Write failing test** — decode `{ service_name, domains: [..], bridge_port, tls }`.
- [ ] **Step 2: Run** `pnpm test` → FAIL.
- [ ] **Step 3: Implement**

```ts
export class RouteView extends Schema.Class<RouteView>('RouteView')({
  service_name: Schema.String,
  domains: Schema.Array(Schema.String),
  bridge_port: Schema.Number,
  tls: Schema.Boolean,
}) {}
export const RouteViews = Schema.Array(RouteView)
```

- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** — `git commit -m "feat(web): ingress RouteView schema"`

---

## Task 2: ApiClient ingress methods

**Files:**
- Modify: `web/src/effect/api-client.ts`
- Test: `web/src/effect/api-client.test.ts`

- [ ] **Step 1: Write failing tests** — `listRoutes` decodes an array from a stub `HttpClient`; `getIngressConfig` returns the raw text body unchanged.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** — add to `ApiClient`:
  - `listRoutes: Effect<ReadonlyArray<RouteView>, ApiError | DecodeError>` -> `GET /v1/ingress/routes` (Schema decode).
  - `getIngressConfig: Effect<string, ApiError>` -> `GET /v1/ingress/config` (read `response.text`, no Schema). Bearer header from `AppConfig`; map errors.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** — `git commit -m "feat(web): ApiClient ingress methods"`

---

## Task 3: Ingress route (table + raw YAML)

**Files:**
- Create: `web/src/routes/ingress.tsx`
- Test: `web/src/routes/ingress.test.tsx`

- [ ] **Step 1: Write failing test** — renders route rows from a mocked Query with a TLS badge for `tls: true`; the raw YAML panel shows mocked config text when expanded.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** — `createFileRoute('/ingress')`; `['ingress','routes']` Query -> `.panel` table (domain(s), service, bridge port, TLS `.signal-steady` badge vs muted "http"); collapsible `.panel` with `['ingress','config']` raw YAML (mono + copy), fetched on expand. Label the control-plane pseudo-route. Empty-state when no routes.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** — `git commit -m "feat(web): ingress routes + config view"`

---

## Task 4: TlsToggle on service detail

**Files:**
- Create: `web/src/components/TlsToggle.tsx`
- Modify: `web/src/routes/services/$serviceId.tsx`
- Test: `web/src/components/TlsToggle.test.tsx`

- [ ] **Step 1: Write failing test** — toggling calls the service mutation with `tls_enabled` flipped and invalidates `['services']` + `['ingress','routes']`.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** — `TlsToggle` takes the service; a `.btn`-styled switch; `useMutation` PUTs the service with `tls_enabled` toggled (reuse `ApiClient.putService`/`createService`); on success invalidate the two query keys. Mount on the service-detail view.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** — `git commit -m "feat(web): per-service TLS toggle"`

---

## Final Verification

- [ ] `cd web && pnpm typecheck` (no `any`/`as`).
- [ ] `cd web && pnpm test` — schema, ApiClient, ingress route, TlsToggle green.
- [ ] `pnpm build`.
- [ ] Manual (backend running): open `/ingress`, see routes + TLS badges + raw
  YAML; toggle a service's TLS on the detail view, refetch, confirm the routes
  table flips to TLS.

## Notes

- `getIngressConfig` returns raw text — do not Schema-decode it.
- TLS badge is steady pink vs muted "http"; plain HTTP is not a fault (no violet).
- Depends on backend `/v1/ingress/*` + `tls_enabled`; builds on the
  operator-console companion for the service-detail mount.
