# ADR-032: HTTP/2 Ingress Hardening

- Status: Accepted
- Date: 2026-06-03

## Context

Denia owns public ingress in-process through Pingora 0.8 with the boringssl TLS
backend (ADR-020). The current `:443` listener constructs
`TlsSettings::with_callbacks` for SNI-based certificate selection and does not
call `enable_h2`, so Denia does not advertise HTTP/2 over ALPN today.

On 2026-06-02, Calif published "Codex Discovered a Hidden HTTP/2 Bomb", a remote
denial-of-service class against HTTP/2 termination points. The attack combines
HPACK indexed header amplification with a stalled HTTP/2 response flow-control
window. Maximum decoded header bytes are not sufficient mitigation because the
amplification can come from many tiny header fields and per-entry allocation
overhead, especially split `cookie` fields.

The article lists Cloudflare Pingora among affected default HTTP/2
configurations. Denia is not currently exposed because HTTP/2 is off, but a
future performance change could accidentally enable it by calling
`TlsSettings::enable_h2`, setting ALPN to `h2`, enabling h2c, or placing Denia
behind an HTTP/2 terminator without equivalent controls.

## Decision

Keep HTTP/2 disabled on Denia-owned public ingress until an accepted
implementation provides protocol-level resource controls and regression tests.

Denia public listeners must not call `TlsSettings::enable_h2`, set ALPN to
`ALPN::H2` or `ALPN::H2H1`, or enable h2c unless all of the following are true:

- request header field count is capped per request before application routing,
  including repeated `cookie` header crumbs and pseudo-headers;
- decoded header byte limits remain in place, but are treated as separate from
  the field-count cap;
- stalled response streams have an absolute lifetime or write-stall bound that
  cannot be extended indefinitely by tiny `WINDOW_UPDATE` progress;
- HTTP/2 concurrent streams and connection-level resource usage are bounded;
- the Pingora version in `Cargo.lock` is reviewed for an upstream fix or Denia
  carries a documented local mitigation;
- tests prove the listener rejects or terminates HPACK/header-count bombs and
  stalled-window response holds without unbounded memory growth.

If Pingora does not expose a hook that enforces header-count limits before the
HTTP/2 request is materialized, Denia must not enable HTTP/2 directly. In that
case, HTTP/2 support requires either an upstream Pingora fix or a separate,
accepted ingress design with a trusted fronting component that enforces the
limits before forwarding to Denia.

Any future ADR or PR that enables HTTP/2 must include:

- the exact Pingora or Denia control that caps total header fields;
- explicit treatment of split `cookie` fields;
- the timeout or lifetime rule for client-stalled response streams;
- adversarial tests or a reproducible lab proving the mitigation;
- an operator rollback path that disables HTTP/2 without changing service
  routing.

## Consequences

- Easier: Denia avoids the published HTTP/2 Bomb class by keeping the vulnerable
  protocol path disabled.
- Easier: future HTTP/2 work has concrete acceptance criteria instead of relying
  on decoded header size alone.
- Easier: the safe default remains aligned with Denia's single-node posture and
  in-process ingress ownership.
- Harder: Denia cannot enable HTTP/2 only for performance or browser feature
  parity.
- Harder: implementing HTTP/2 later may require waiting for Pingora upstream
  controls or carrying Denia-specific protocol tests.
- Constraint: operators that put an HTTP/2-capable CDN, load balancer, or proxy
  in front of Denia must ensure that component enforces equivalent header-count
  and stalled-stream protections, or disable HTTP/2 there as well.

## Alternatives Considered

- **Enable HTTP/2 and rely on decoded header-size limits.** Rejected because the
  attack specifically bypasses size-only defenses with many small fields and
  allocation overhead.
- **Enable HTTP/2 with only normal write timeouts.** Rejected because the
  published attack uses response flow-control progress to keep allocations live;
  mitigation needs a lifetime or stall rule that cannot be refreshed forever.
- **Front Denia with a separate proxy by default.** Rejected because ADR-020
  intentionally makes Denia the owner of `:80`/`:443`; adding a mandatory proxy
  would reverse that architecture.
- **Disable TLS entirely to avoid HTTP/2.** Rejected because TLS and HTTP/2 are
  separate decisions. Denia can keep TLS while advertising only HTTP/1.1.

## References

- [Codex Discovered a Hidden HTTP/2 Bomb](https://blog.calif.io/p/codex-discovered-a-hidden-http2-bomb)
- ADR-020 (in-process Pingora ingress)
- RFC 7541 (HPACK)
- RFC 9113 (HTTP/2)
