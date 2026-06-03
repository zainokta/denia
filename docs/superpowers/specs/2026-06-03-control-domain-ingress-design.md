# Design: Control Domain Over Ingress (Console + API + Registry) with Per-IP Auth Limits

- **Status**: Draft (design approved, pre-implementation)
- **Date**: 2026-06-03
- **Related**: ADR-020 (Pingora ingress), ADR-004 (embedded web console), ADR-008 (RBAC),
  ADR-013 (domain verification), ADR-031 (hosted OCI registry), ADR-007 (ingress TLS data model)

## Problem

Denia's control plane — the web console (`/`), management API (`/v1`), hosted OCI
registry (`/v2`), and `/healthz` — is one axum server bound to `bind_addr`
(default `127.0.0.1:7180`). It is only reachable on that loopback port.

Two consequences:

1. **No way to reach the console/API on a domain.** An operator on a VPS cannot
   serve `https://denia.example.com` for the console. A DNS A record maps a name
   to an IP, not a port; standard traffic lands on `:443`, which Denia's Pingora
   ingress owns. Nothing routes a hostname to the control backend, and ADR-020
   forbids running a second proxy.
2. **`docker push` cannot work over the loopback HTTP endpoint.** OCI clients
   (docker, containers/image) refuse to send Basic credentials over plaintext
   HTTP, so registry auth against `http://localhost:7180/v2` fails with
   `unauthorized`. Basic auth requires TLS.

The config fields `control_domain` and `control_tls` already exist (`config.rs`)
and are parsed, but no code routes them — the wiring was never built.

## Goals

- Serve one operator-configured hostname (`control_domain`) over the Pingora
  ingress (`:443`, ACME TLS), reverse-proxied to the existing axum control
  backend, exposing console + `/v1` + `/v2` + `/healthz` on that domain.
- Make `docker push <control_domain>/<project>/<service>` work natively over
  HTTPS (closes the registry-over-HTTP gap for free — same backend).
- Make the existing per-IP auth rate limiters effective behind the ingress by
  forwarding the real client IP.

## Non-Goals

- No distributed/global rate-limit backstop, IP allowlist, or account lockout
  (explicitly deferred — per-IP limits only).
- No rate limiting added to `/v2` registry auth (API/admin tokens are 64-hex,
  unbruteforceable).
- No split console/registry hostnames — one domain serves everything.
- No change to `:7180` binding — it stays loopback; the control plane remains
  reachable locally / via SSH tunnel exactly as today.
- No second proxy or external TLS terminator (ADR-020: Denia owns `:80`/`:443`).

## Decisions

### Routing

The ingress proxy special-cases the control domain ahead of workload routing:

- `proxy.rs::upstream_peer`: if the request `Host` equals `control_domain`,
  return a new `UpstreamChoice::ControlBackend` → `HttpPeer::new(control_backend)`
  (plain HTTP to loopback axum; TLS already terminated at the ingress). This
  check precedes workload `RouteTable` resolution, so the control domain always
  wins over any workload route.
- `proxy.rs::request_filter` (`:80` only): when `Host == control_domain` and
  `control_tls`, issue a 308 redirect to `https://`. ACME / denia challenge
  interception still runs first (unchanged precedence).
- The control domain is **not** entered into the workload `RouteTable`; it is a
  distinct branch so the TCP control backend never collides with the
  host→service→UDS model.

### Client IP propagation (rate-limiter correctness)

Add `proxy.rs::upstream_request_filter` that **overwrites** the
`X-Forwarded-For` header with the real downstream peer IP (removing any
client-supplied value) for all proxied requests. The axum rate limiter
(`rate_limit.rs::extract_client_ip`) already trusts `X-Forwarded-For` only when
the TCP peer is loopback — which it always is once the on-host Pingora proxy
dials the backend. Overwrite (not append) prevents a client from spoofing the
rate-limit key.

No change to limiter logic: `LoginRateLimiter` (5/60s) on public `/v1/auth`,
`AdminRateLimiter` (300/60s) on authed routes — both now key on the real IP.

### TLS certificate for the control domain

Reuse the existing ACME machinery. The boot-issue + renewal orchestration lives
in `src/daemon.rs::run()`: `issue_missing_certs` / `reissue` (issuance) and
`select_renewals` (renewal scan), calling `AcmeDriver::issue` / `persist_cert` /
`CertStore` insert.

Important: `issue_missing_certs` iterates **services** and their verified
domains. `control_domain` is operator-level config with no service row, so it
will **not** be picked up by that loop — it needs a deliberate, distinct branch
that unconditionally appends `control_domain` to the issuance set when
`control_tls`. The renewal scan (`select_renewals`) already covers any SNI in
the store, so once the cert is issued/persisted, renewal is automatic; only the
initial issuance pass needs the extra branch.

The operator-configured domain is implicitly authorized (root-level config);
ACME HTTP-01 still proves DNS control via the `:80` challenge path. Persisted
certs auto-load on restart via the existing `load_certs_from_disk`.

### Configuration

- `control_domain: Option<String>` and `control_tls: bool` already exist. When
  set, `control_domain` is validated with `validate_domain` at config load
  (fail fast on bad input).
- `IngressServerConfig` gains `control_domain` + `control_tls`, threaded into
  `DeniaProxy::http`/`https`.
- `control_tls=false` with `control_domain` set → route on `:80` plain HTTP
  only (no cert, no redirect) — local/testing convenience.
- `control_domain` unset → behavior is byte-for-byte today's (nothing routed to
  the control plane except challenge paths).

### Guardrail

The domains API (`src/api/domains.rs`, `create_service_domain` /
`verify_service_domain`) rejects creating or verifying a *service* domain equal
to `control_domain`, so a workload cannot hijack the control hostname.
`AppState` already carries `config` (holding `control_domain`). The comparison
must lowercase both sides (the domains API uses `crate::verification::validate_hostname`,
which lowercases) to avoid a case-mismatch bypass.

## Architecture

```
client ──https──▶ :443 Pingora (SNI=control_domain → CertStore cert)
                    │ TLS terminate
                    │ upstream_request_filter: X-Forwarded-For = client IP
                    ▼
                127.0.0.1:7180 axum control backend
                    ├─ /         SPA console
                    ├─ /v1/*     management API   (rate_limit_login on /v1/auth)
                    ├─ /v2/*     hosted registry   (docker push/pull over HTTPS)
                    └─ /healthz

:80 Pingora
  ├─ /.well-known/acme-challenge/*   → control backend (existing interception)
  └─ Host==control_domain && tls     → 308 https://
```

## Data Flow Examples

- **Console**: `GET https://denia.example.com/` → `:443` → backend `/` → SPA.
- **Login**: `POST https://denia.example.com/v1/auth/...` → `:443`
  (XFF=client IP) → backend → `rate_limit_login` keys on client IP → 5/min/IP.
- **Push**: `docker push denia.example.com/default/personal` → `:443` → `/v2`
  → token auth over TLS → upload (body limit already disabled on `/v2`).

## Error Handling

- Cert not yet issued (DNS not pointed / ACME pending): `:443` SNI callback
  declines cleanly (`TLSHandshakeFailure`); the domain is simply unreachable
  over HTTPS until issuance succeeds. No crash.
- ACME issuance failure for `control_domain`: logged; retried by the renewal
  scan; never blocks boot (same posture as service certs).
- `control_domain` fails `validate_domain`: config error at load.
- Unknown SNI / wrong host on `:443`: unchanged (declines / 404 via existing
  paths).

## Security Considerations

- `:7180` stays loopback-bound; the control plane is reachable only through the
  ingress (or locally / SSH tunnel).
- `X-Forwarded-For` is overwritten, not appended → the rate-limit key cannot be
  spoofed by the client. Per-IP login (5/min) and admin (300/min) become
  effective.
- Accepted residual risk: the control plane is now internet-facing. Mitigations:
  argon2id passwords, 64-hex bearer/API tokens, per-IP login throttle. `/v2`
  auth stays unthrottled by design (token unbruteforceable).
- This widens the trust boundary (control plane public), so it requires a new
  ADR extending ADR-020 (references ADR-004, ADR-008, ADR-031).

## Testing Strategy

- Unit (`proxy.rs`): `classify_port80` for the control domain — redirect when
  `control_tls`, passthrough when not, challenge precedence still wins; a
  control-domain branch in `upstream_peer`; XFF-overwrite helper drops a
  client-supplied value and sets the real peer.
- Unit (`rate_limit.rs`): `extract_client_ip` honors the overwritten XFF from a
  loopback peer.
- Unit (ACME orchestration): the issue/renew domain set includes
  `control_domain` iff `control_tls`.
- Unit (domains API): a service domain equal to `control_domain` is rejected.
- Privileged / e2e (gated, extends the existing Phase-8 ingress test): real
  `:443` handshake for `control_domain` and a proxied request reaching the
  backend.

## Affected Files (anticipated)

- `src/ingress/pingora/proxy.rs` — control-domain routing, `:80` redirect,
  `upstream_request_filter` (XFF overwrite), new `UpstreamChoice::ControlBackend`
  handling.
- `src/ingress/pingora/server.rs` — `IngressServerConfig` carries
  `control_domain`/`control_tls`; threaded into `DeniaProxy`. Update
  `IngressServerConfig::from_ports` (the sole non-test builder) and its caller
  in `daemon.rs`.
- `src/daemon.rs` (`run()`, ACME wiring) — append `control_domain` to the
  boot issuance set (`issue_missing_certs`/`reissue`) as a distinct branch when
  `control_tls`; renewal via `select_renewals` is already automatic.
- `src/config.rs` — validate `control_domain` at load.
- `src/rate_limit.rs` — comment cleanup (Traefik → Pingora); no logic change.
- domains API module — reject service domain == `control_domain`.
- New ADR — control-plane public exposure (extends ADR-020).

## Open Questions

- Whether to also set `X-Forwarded-Proto`/`X-Forwarded-Host` for backend
  correctness (not required for the rate limiter; the SPA is same-origin with
  relative paths). Default: XFF only unless a concrete need surfaces.
