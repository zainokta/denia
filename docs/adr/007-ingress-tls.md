# ADR-007: Ingress + TLS

- **Status**: Proposed
- **Date**: 2026-05-25

## Context

Operators need opt-in per-service TLS via Traefik ACME, a routable control-plane
domain, and read-only ingress observability. Denia owns the Traefik dynamic file
provider config and the loopback bridge listeners; ACME issuance stays in
Traefik's operator-owned static config.

## Decision

- `ServiceConfig` gains `tls_enabled: bool` (defaults to `false`, serde-default
  backfills older rows stored as JSON).
- `traefik::IngressRenderOptions` carries `acme_resolver`, `control_domain`,
  `control_tls`, `control_backend_addr`. Sourced from `AppConfig` (`DENIA_*`
  env). `tls_enabled` services render a `websecure` router with
  `tls.certResolver` plus an HTTP→HTTPS redirect router; plain services render
  a single `web` router.
- A control-plane router from `DENIA_CONTROL_DOMAIN` is emitted in the same
  document when set.
- `AppState` owns a shared `routes: Arc<Mutex<BTreeMap<String, RouteSpec>>>`
  and a single `IngressRenderOptions`. Deploy and stop paths mutate this map
  and re-render the dynamic config file. The shared map is the canonical
  promoted-routing snapshot.
- `GET /v1/ingress/routes` returns the live snapshot as JSON.
- `GET /v1/ingress/config` returns the on-disk Traefik dynamic config as
  `text/yaml` (empty body when missing).

## Consequences

- Per-service TLS is a single boolean toggle. No per-service certResolver
  override yet.
- The ingress snapshot stays consistent with the file on disk only as long as
  Denia owns all writes to it — operators must not hand-edit the dynamic file.
- Control-domain TLS uses the same single ACME resolver as services.

## Alternatives Considered

- **Per-service ACME resolver**: rejected for now; one resolver covers the
  single-node case.
- **Rebuilding routes from `services` on every read**: rejected; the bridge
  port is not in SQLite, so the in-memory snapshot is the source of truth.
- **Parsing the Traefik YAML back for `/ingress/routes`**: rejected as fragile.

## References

- `docs/superpowers/plans/2026-05-25-ingress-tls.md`
- `docs/superpowers/specs/2026-05-25-ingress-tls.md`
