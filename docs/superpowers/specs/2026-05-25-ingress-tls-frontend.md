# Spec: Ingress + TLS UI (Frontend) — companion to ingress-tls

Status: Draft · Date: 2026-05-25 · Frontend companion to
[`2026-05-25-ingress-tls.md`](2026-05-25-ingress-tls.md)

## Problem

The backend adds opt-in per-service TLS, a control-plane domain, and
`/v1/ingress/{routes,config}`. The console has no way to see routing or toggle
TLS, so operators cannot verify how a service is exposed.

## Goal

An ingress view in the console: list generated routes (domain, target service,
TLS state, bridge port), show the raw generated Traefik YAML for debugging, and a
per-service TLS toggle. Effect + Query layer, same-origin `/v1`, DESIGN.md system.

## Backend surface consumed

- `GET /v1/ingress/routes` -> `[{ service_name, domains, bridge_port, tls }]`
- `GET /v1/ingress/config` -> `text/plain` YAML
- `POST /v1/services` (existing) to set `tls_enabled` on a service

## Decisions

- **Effect first:** `ApiClient.listRoutes` (Schema-decoded) + `getIngressConfig`
  (raw text, no Schema). React via `runQuery`/Query.
- **Route** (`web/src/routes/`): `/ingress` — routes table + collapsible raw YAML.
- **TLS state as signal (DESIGN.md):** `tls: true` -> a steady (Stagecraft pink)
  "TLS" badge; `false` -> muted "http". Not a fault — plain HTTP is a valid
  choice — so no violet here.
- **TLS toggle** lives on the console service-detail view (re-uses that route):
  a switch that PUTs the service with `tls_enabled` flipped, then invalidates
  services + `['ingress','routes']`.
- **Raw YAML** rendered mono in a collapsible `.panel`; copy button.

## Components / data flow

- `Schema` `RouteView { service_name, domains, bridge_port, tls }`.
- `ApiClient.listRoutes`, `ApiClient.getIngressConfig` (returns `string`).
- `web/src/routes/ingress.tsx` — `['ingress','routes']` table + `['ingress','config']` raw YAML (manual/refetch-on-demand, not polled).
- `TlsToggle` on `services/$serviceId` (from the console companion) -> mutation.

## Errors / edge cases

- Empty routes (no services) -> empty-state copy.
- Control-plane pseudo-route shown distinctly (labelled "control plane").
- `getIngressConfig` failure -> inline error in the YAML panel, table still works.
- 401 -> auth-needed banner.

## Success criteria

- Operator sees every route, which service it targets, and whether it is TLS.
- Can read the exact generated Traefik YAML.
- Can toggle a service's TLS and see the routes table reflect it after refetch.

## Testing

- `@effect/vitest`: `listRoutes` Schema decode + `getIngressConfig` returns text;
  error mapping.
- `@testing-library/react`: routes table renders + TLS badge mapping; raw YAML
  collapsible; `TlsToggle` calls the mutation and invalidates queries.

## Out of scope

ACME/cert status display (Traefik owns issuance; no API for it here), editing the
Traefik static config, request-tracing (sub-project D). Backend behaviour (its own
spec). Builds on the operator-console companion for the service-detail toggle.
