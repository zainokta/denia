# ADR-020: In-Process Pingora Ingress

- **Status**: Accepted
- **Date**: 2026-05-28
- **Supersedes**: ADR-016 (Denia-Managed Traefik); replaces the ingress mechanism of ADR-007 (Ingress + TLS), retaining its `tls_enabled` data model.

## Context

ADR-016 had Denia acquire, run, and supervise its own Traefik child process
(OCI-pulled Go binary) for ingress, while a loopback "bridge" (`src/ingress/bridge.rs`)
proxied each workload's Unix socket to a loopback TCP port Traefik forwarded to.
That delivered zero-Traefik-setup for the operator but kept several liabilities:
an external binary to acquire/supervise/restart, SELinux/AppArmor exec exposure,
`EADDRINUSE` fatal handling, unrotated `traefik.log`, dropped connections on
process restart, and a whole UDS→TCP transport layer whose only reason to exist
was Traefik compatibility.

We want Denia to be its own L7 proxy: no external binary, native Rust request
path, direct UDS upstreams, and TLS issued in-process.

## Decision

**Denia binds `:80`/`:443` itself via an in-process [Pingora](https://github.com/cloudflare/pingora)
0.8 proxy (boringssl backend).** Traefik (acquisition, supervisor, dynamic
file-provider config) and the loopback-bridge transport are removed.

- **Request path** (`src/ingress/pingora/proxy.rs`, `DeniaProxy: ProxyHttp`):
  `upstream_peer` resolves the `Host` header to a service via an in-memory
  `RouteTable`, picks a healthy replica (round-robin), and dials the workload's
  Unix socket directly with `HttpPeer::new_uds(path, false, sni)`. Unknown host →
  404; no healthy replica → scale-from-zero activation (single-flight, bounded by
  `ACTIVATION_WAIT`) → 503 on timeout. The bridge's control brain (replica pools,
  health, activation hook, idle `last_activity`, access log) moved into
  `IngressState`; only the UDS→TCP *transport* was deleted. `bridge_port`,
  `BridgeAllocator`, and `DENIA_BRIDGE_START_PORT` are gone.
- **`:80` challenge + redirect:** `request_filter` intercepts
  `/.well-known/acme-challenge/*` and `/.well-known/denia-challenge/*`
  unconditionally (before host routing) and proxies them to the control-plane
  backend (loopback axum). ACME tokens are served by token lookup. Denia domain
  verification tokens are served only when the request `Host` matches the
  hostname stored for that token. A `tls_enabled` host on `:80` gets a 308
  redirect to `https://`.
- **TLS:** the `:443` listener uses a `TlsAccept::certificate_callback` that
  selects a cert by SNI from an `ArcSwap<CertStore>` at handshake; unknown/absent
  SNI declines cleanly (`TLSHandshakeFailure`, never a wrong cert). Cert
  selection is synchronous; issuance is fully out-of-band.
- **ACME** (`src/ingress/pingora/acme.rs`, `instant-acme`, HTTP-01): orders are
  enqueued only for `tls_enabled` services whose hostnames are verified
  (`list_verified_hostnames`, ADR-013). The account key (`<tls_dir>/account.key`)
  and leaf cert/key (`<tls_dir>/<domain>/{fullchain,key}.pem`) are written
  atomically (temp@0600 → rename) in `0700` directories; certs are boot-loaded
  into `CertStore` **before** `:443` accepts. A renewal task re-orders within the
  expiry window. Every hostname is validated (`validate_domain`: rejects
  empty/whitespace/control/backtick/CRLF/wildcard/non-ASCII/overlong/dot-edges,
  lowercases) before becoming a route key, ACME order identifier, cert directory
  name, or SNI key.
- **Lifecycle:** the Pingora `Server` runs on a dedicated `std::thread` via
  `Server::run(RunArgs { shutdown_signal, .. })` (never `run_forever()`, which
  `process::exit`s) with a custom `ShutdownSignalWatch` fed by Denia's existing
  shutdown channel, so Denia keeps authoritative control of OS signals. Route,
  scale, and cert changes are applied live by swapping `ArcSwap` state — no
  process restart.
- **Failure isolation:** if Pingora fails to bind `:80`/`:443`, the failure is
  logged ("Denia owns these ports — stop any external proxy") and the control
  plane keeps serving on `bind_addr`. There is no fallback that serves the admin
  API on the public ports or disables TLS.

## Consequences

- **`GET /v1/ingress/config` is removed** (it returned the Traefik dynamic YAML).
  `GET /v1/ingress/routes` remains; its response no longer carries `bridge_port`.
- **Config removed:** `DENIA_TRAEFIK_IMAGE`, `DENIA_TRAEFIK_DYNAMIC_CONFIG`,
  `DENIA_BRIDGE_START_PORT`, `DENIA_ACME_RESOLVER` (Traefik certResolver name).
  **Added:** `DENIA_ACME_DIRECTORY_URL`, `DENIA_TLS_DIR`. Kept: `DENIA_HTTP_PORT`,
  `DENIA_HTTPS_PORT`, `DENIA_ACME_EMAIL`, `DENIA_CONTROL_DOMAIN`.
- **`DENIA_ACME_DIRECTORY_URL` defaults to Let's Encrypt production.** For
  non-prod / staging nodes, set it to the LE staging directory
  (`https://acme-staging-v02.api.letsencrypt.org/directory`) to avoid burning
  production rate limits during testing.
- **Unauthenticated activation posture (accepted):** an unauthenticated client on
  `:80`/`:443` can trigger scale-from-zero for an already-routed cold service.
  This is bounded by per-service single-flight + `ACTIVATION_WAIT` (no unbounded
  thread/issuance growth) and the challenge hop always dials the fixed control
  backend (no SSRF). General request rate-limiting on the data plane is
  intentionally out of scope for this ADR.
- **TLS backend is boringssl** — Pingora's `rustls` path is a stub; Cargo enables
  the `boringssl` feature explicitly. Key bytes are owned by foreign types
  (`boring`, `instant-acme`); zeroize-on-drop is not feasible, so the mitigation
  is no-`Debug`/`Serialize`/no-log discipline plus `0600` at-rest files (residual
  risk is memory-disclosure/core-dump, inside the already-trusted host-root
  boundary).
- Operators must stop any external proxy on `:80`/`:443`; Denia is the sole
  owner.

## Alternatives Considered

- **Keep managed Traefik (ADR-016 status quo):** external binary lifecycle,
  exec-security exposure, and a redundant transport layer. Rejected — the whole
  point was to own ingress in-process.
- **Pingora with `rustls` + `rustls-acme` (TLS-ALPN-01):** less ACME code, but
  Pingora 0.8's rustls path is a stub and `TlsSettings` does not expose a raw
  rustls `ServerConfig`/resolver; ALPN-01 also collides with h2/h1 ALPN. Rejected
  in favour of boringssl + instant-acme HTTP-01.
- **Keep the loopback-TCP bridge under Pingora:** unnecessary once `HttpPeer::new_uds`
  proved viable (spike, 2026-05-28). Rejected — direct UDS removes a hop.

## References

- `docs/superpowers/specs/2026-05-28-pingora-ingress-design.md`
- `docs/superpowers/specs/2026-05-28-pingora-ingress-spike-notes.md`
- `docs/superpowers/plans/2026-05-28-pingora-ingress.md`
- `docs/security-audit-pingora-2026-05-28.md`
- ADR-016 (superseded), ADR-007 (ingress mechanism replaced), ADR-013 (domain
  verification), ADR-018 (autoscaling — activation hook), ADR-005 (isolation
  posture Pingora deliberately does not share).
