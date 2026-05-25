# Spec: Ingress + TLS (TODO #2 / #7 / #10)

Status: Draft · Date: 2026-05-25 · Sub-project E of the TODO decomposition

## Problem

Denia generates a Traefik **dynamic** file-provider config (`src/traefik.rs`):
plain HTTP routers pointing at `127.0.0.1:<bridge_port>`. There is no TLS, the
PaaS control plane is only reachable by IP, and operators cannot see the
generated routing config. This covers TODO #2 (automatic Let's Encrypt TLS), #7
(Traefik config viewer), and #10 (own domain for the control plane).

## Goal

Add opt-in per-service TLS via Traefik ACME, a routable control-plane domain, and
read-only ingress config endpoints, while keeping Denia's boundary at the dynamic
file (ACME issuance/storage stays in Traefik's operator-owned static config).

## Decisions

- **ACME boundary:** Denia only annotates the routers it generates with
  `tls.certResolver` + the `websecure` entrypoint. Traefik's static config
  (entrypoints, `certificatesResolvers.<name>.acme`, `acme.json`) is set up once
  by the installer/operator. Resolver name from `DENIA_ACME_RESOLVER` (default
  `le`).
- **TLS is opt-in per service:** `ServiceConfig` gains `tls_enabled: bool`
  (serde default `false`). Only enabled services get a TLS router; they also get
  a companion HTTP->HTTPS redirect router.
- **Control-plane own domain:** if `DENIA_CONTROL_DOMAIN` is set, Denia emits a
  router for it -> the parsed `AppConfig.bind_addr` backend (using loopback when
  the bind IP is unspecified), TLS per `DENIA_CONTROL_TLS`.
- **Config viewer:** `GET /v1/ingress/routes` (typed JSON) and
  `GET /v1/ingress/config` (rendered YAML, text/plain).

## Config (`src/config.rs`)

- `acme_resolver: String` (`DENIA_ACME_RESOLVER`, default `le`).
- `control_domain: Option<String>` (`DENIA_CONTROL_DOMAIN`).
- `control_tls: bool` (`DENIA_CONTROL_TLS`, default `false`).
- `web_entrypoint: String` (`DENIA_WEB_ENTRYPOINT`, default `web`).
- `websecure_entrypoint: String` (`DENIA_WEBSECURE_ENTRYPOINT`, default `websecure`).

## Domain / data model

- `ServiceConfig.tls_enabled: bool` (default false; migration backfills existing
  rows to false).
- `traefik::RouteSpec` gains `route_key: String` and `tls: bool`. `route_key`
  is generated/sanitized (service id or project-qualified key once Projects has
  landed) and is the only value used for Traefik YAML object names; display
  `service_name` remains data.
- A render-time `IngressRenderOptions { acme_resolver, web_entrypoint,
  websecure_entrypoint, control_domain, control_tls, control_backend_addr }`
  carries the config into the renderer (keeps `RouteSpec` free of node-wide
  config).
- Ingress views and config rendering read the live promoted route snapshot that
  deployment promotion writes. They must not rebuild routes from
  `store.list_services()`: bridge ports exist only after `BridgeAllocator.assign`,
  and undeployed services are not active ingress routes.

## Renderer (`src/traefik.rs`)

- Router per route:
  - TLS on -> `entryPoints: [websecure]`, `tls: { certResolver: <resolver> }`,
    plus a companion redirect router on `[web]` using a `redirectScheme`
    middleware (to `https`).
  - TLS off -> `entryPoints: [web]` (current behaviour).
- Services block unchanged (loadBalancer -> `http://127.0.0.1:<port>`).
- Control-plane: when `control_domain` is set, add a router (+ service) to the
  Denia control backend address, TLS per `control_tls`.
- Validation: TLS requested but empty resolver -> `TraefikError::MissingResolver`.
  Route keys are generated safe identifiers; domains are validated/rejected if
  empty, containing backticks/control characters, or otherwise unsafe for a
  Traefik `Host()` rule.

## API (`/v1`, bearer-protected)

- `GET /v1/ingress/routes` -> typed route views from the live promoted route
  snapshot, for example service rows
  `[{ kind: "service", route_key, service_name, domains, bridge_port, tls }]`
  and a control row `{ kind: "control", domains, backend_url, tls }` when
  configured.
- `GET /v1/ingress/config` -> `text/plain` rendered dynamic YAML.

## Errors / edge cases

- TLS enabled, resolver name empty -> 500 config error surfaced clearly.
- `control_domain` unset -> no control route, endpoints still work.
- A service with no domains -> existing `MissingDomain` error.
- Rendered config comes from the same render path the writer uses (single source
  of truth), not re-read from disk, to avoid drift.
- The API must not call `BridgeAllocator.assign` while answering read-only
  endpoints; reads cannot allocate new ports or activate undeployed services.

## Success criteria

- An opt-in service serves HTTPS via Traefik ACME; HTTP redirects to HTTPS.
- The control plane is reachable at `DENIA_CONTROL_DOMAIN` (TLS optional).
- `GET /v1/ingress/routes` + `/config` reflect the live generated config.
- Non-TLS services keep working unchanged.

## Testing

- `traefik` unit: render snapshot with TLS on (entrypoints + certResolver +
  redirect router) and off; control-plane route; missing-resolver error;
  generated safe keys; unsafe domains rejected.
- `config`: env parsing + defaults.
- `state`: `tls_enabled` default + migration backfill.
- API: `/v1/ingress/routes` shape; `/v1/ingress/config` content-type + body;
  undeployed services absent; stopped services removed; read-only requests do not
  allocate bridge ports.

## Out of scope

ACME static bootstrap (installer/operator + ADR), DNS-01 / DNS providers,
wildcard certs, per-request tracing/access logs (sub-project D). Frontend covered
by the companion spec.

## Dependencies

Uses the shared versioned-migration infra from sub-project B (Projects). If B is
not built first, introduce that shared migration ledger here before adding the
`tls_enabled` migration; do not add a separate ad-hoc migration path that later
conflicts with Projects.
