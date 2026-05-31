# ADR-013: Domain Support With HTTP File Verification

- **Status**: Accepted
- **Date**: 2026-05-25

## Context

Operators need to attach custom domains to a service and prove control before
the domain routes traffic or requests an ACME certificate. Previously
`ServiceConfig.domains: Vec<String>` was a flat list with no ownership proof:
any operator could attach any hostname and Traefik rendered it immediately,
including an ACME cert request for a host the operator might not control. This
mirrors a gap Dokploy closes with domain verification.

## Decision

- Domains become a first-class entity in a new SQLite table `service_domains`
  (`id`, `service_id`, `hostname` UNIQUE, `status`, `challenge_token` UNIQUE,
  `verified_at`, `last_check_at`, `last_error`, `created_at`). Migration `005`
  creates it and backfills each existing `ServiceConfig.domains` entry as a
  `verified` row (operators added them manually pre-ADR; trusted).
- Verification method is **HTTP file challenge only**. The DNS A record is
  already required for Traefik to receive traffic, so requiring it for
  verification adds no operator burden. No DNS TXT path.
- A pending or failed domain is **blocked entirely** from Traefik — no plain
  HTTP router, no TLS, no ACME request. Only `verified` domains appear in the
  generated dynamic config.
- The challenge is served by the Denia control plane at the public route
  `GET /.well-known/denia-challenge/{token}` (no auth — the token is the
  secret) returning `200 text/plain` with the token body, or `404`. The request
  `Host` header must match the hostname stored for that token; a valid token
  presented for another hostname returns `404`.
- Traefik exposes the challenge via a single global router matching
  `PathPrefix(`/.well-known/denia-challenge`)` on the `web` entrypoint at
  priority `1000`, forwarding to `IngressRenderOptions.control_backend_addr`.
  Emitted unconditionally on every render.
- Verification is **operator-triggered** (manual). `POST
  /v1/services/{id}/domains/{domain_id}/verify` fetches
  `http://{hostname}/.well-known/denia-challenge/{token}` with 5s connect/read
  timeouts, no redirects, a 1 KiB body cap, and a constant-time body compare
  (`subtle`). Success sets `verified` and re-renders Traefik; failure sets
  `failed` with a short `last_error`. Re-triggering a `failed` domain retries.
  One in-flight verification per domain is enforced via an in-memory
  `HashSet<Uuid>` guard (409 on concurrent attempt).
- The verifier rejects internal destinations before the HTTP fetch, including
  IPv4 private/link-local/loopback/CGNAT and IPv6 loopback, link-local,
  unique-local (`fc00::/7`), multicast (`ff00::/8`), and IPv4-mapped internal
  addresses.
- `tls_enabled` stays per-service. TLS routers render only for verified
  domains, so ACME is never asked for an unverified host.

## Consequences

- Operators must verify a domain before it serves traffic. New domains added
  after a service is deployed do not route until both verified AND the service
  is re-deployed (the deploy path is what assigns the bridge route; the verify
  re-render reuses an existing route entry and skips services with none).
- `ServiceConfig.domains` is retained for one release as a read-only API field
  derived from verified rows; a future ADR removes it.
- No DNS TXT, wildcard domains, automatic retry loop, per-domain TLS override,
  or forced re-verification of already-verified domains. Each can be a
  follow-up ADR.
- Frontend console support for adding/verifying domains is out of scope here.

## Alternatives Considered

- **DNS TXT verification**: rejected for v1; the A record is already a
  prerequisite, so HTTP file challenge is sufficient and simpler.
- **Per-domain pending routers** for the challenge: rejected in favor of one
  global path router — less config churn, lower blast radius.
- **Routing unverified domains as informational**: rejected; would let Traefik
  request ACME certs for unowned hosts.
- **Storing verification state inside `ServiceConfig.domains` JSON**: rejected;
  a first-class table makes "all pending domains" queryable and keeps the
  unique-hostname constraint at the database layer.

## References

- `docs/superpowers/specs/2026-05-25-domain-verification.md`
- `docs/superpowers/plans/2026-05-25-domain-verification.md`
- `docs/adr/007-ingress-tls.md`
- `docs/adr/008-rbac.md`
