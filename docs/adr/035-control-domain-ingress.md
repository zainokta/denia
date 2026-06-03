# ADR-035: Control Domain Over Ingress

- **Status**: Accepted
- **Date**: 2026-06-03
- **Extends**: ADR-020 (Pingora ingress). **References**: ADR-004 (web console),
  ADR-008 (RBAC), ADR-031 (hosted registry), ADR-013 (domain verification).

## Context

The control plane (console `/`, management API `/v1`, hosted registry `/v2`,
`/healthz`) is one axum server bound to `bind_addr` (default `127.0.0.1:7180`),
reachable only on loopback. Operators cannot serve the console on a domain, and
`docker push` cannot authenticate over the loopback HTTP endpoint (OCI clients
refuse Basic auth over plaintext HTTP). The `control_domain`/`control_tls`
config fields existed but were never routed.

## Decision

When `control_domain` is set, the in-process Pingora ingress serves it on
`:443` (ACME TLS) and reverse-proxies to the loopback control backend, exposing
console + `/v1` + `/v2` on that hostname. `:7180` stays loopback-bound.

- Routing: `DeniaProxy` matches `Host == control_domain` ahead of workload
  routing and dials the control backend via `HttpPeer::new`. On `:80` the
  control domain redirects to `https://` when `control_tls` (ACME challenge
  interception still wins first).
- TLS: the control domain is issued/renewed by the existing ACME orchestration
  as a distinct branch (it has no service row); HTTP-01 proves DNS control.
- Client IP: the proxy overwrites `X-Forwarded-For` with the real downstream
  peer, so the loopback-trusting per-IP rate limiter (login 5/min, admin
  300/min) keys on the true client IP. Overwrite (not append) prevents
  spoofing.
- Guardrail: a workload service domain cannot equal `control_domain`.

## Consequences

- The control plane becomes internet-facing when `control_domain` is set.
  Mitigations: argon2id passwords, 64-hex bearer/API tokens, per-IP login
  throttle. `/v2` registry auth stays unthrottled (tokens unbruteforceable).
- `docker push <control_domain>/<project>/<service>` works natively over HTTPS.
- No second proxy; ADR-020's "Denia owns `:80`/`:443`" stands.
- `control_tls=false` with `control_domain` set serves the domain on `:80`
  plaintext only (local/testing).

## Alternatives Considered

- External reverse proxy / second TLS terminator: rejected — ADR-020 reserves
  `:80`/`:443` for Denia's ingress.
- Binding the control backend directly to `:443`: rejected — conflicts with the
  ingress listener.
- IP allowlist / distributed-brute-force backstop: deferred (YAGNI); per-IP
  limits only.

## References

- `docs/superpowers/specs/2026-06-03-control-domain-ingress-design.md`
- `docs/superpowers/plans/2026-06-03-control-domain-ingress.md`
- ADR-020, ADR-004, ADR-008, ADR-031, ADR-013.
