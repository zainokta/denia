# ADR-036: General-Purpose Protocol Ingress

- **Status**: Proposed
- **Date**: 2026-06-04
- **Extends**: ADR-020 (Pingora ingress), ADR-032 (HTTP/2 ingress hardening), ADR-018 (autoscaling), ADR-005 (runtime security hardening).

## Context

Denia currently routes workload traffic as HTTP/1.1 through in-process Pingora
on `:80`/`:443`. A hostname resolves to a service route, Pingora picks a healthy
replica, and the request is proxied to the replica's Denia-owned Unix stream
socket. That model supports normal HTTP and should support WebSocket upgrades,
but it does not expose raw TCP or UDP workloads such as game servers.

Native gRPC requires HTTP/2. ADR-032 deliberately keeps HTTP/2 disabled on
public ingress until Denia has protocol-level resource controls and regression
tests for the published HTTP/2 Bomb class. Therefore gRPC cannot be enabled as
just another Pingora route in this change.

## Decision

Denia will add general-purpose protocol ingress in phases:

- **HTTP/WebSocket** remains on Pingora `:80`/`:443`, routed by verified
  hostname. WebSocket support is treated as HTTP/1.1 upgrade compatibility and
  must have an end-to-end regression test.
- **TCP/UDP** are modeled as service endpoints and exposed through Denia-owned
  public ports allocated from operator-configured ranges. Public L4 ports are
  distinct from HTTP hostname routing.
- **Native gRPC is deferred** until an accepted HTTP/2 implementation satisfies
  ADR-032. Operators that need gRPC earlier may run it as raw TCP on an
  allocated public port and let the workload own HTTP/2/TLS.
- **TCP/UDP services are always-on in v1.** Denia rejects scale-to-zero for any
  service exposing TCP or UDP endpoints. TCP cold-start connection holding and
  UDP datagram buffering are out of scope for the first L4 release.

The first implementation slice adds the stable domain/config foundation:
service endpoint types, legacy HTTP endpoint projection, TCP/UDP port-range
configuration, and deterministic in-memory allocation. Runtime launch changes,
SQLite persistence of allocated ports, and live L4 listeners follow in later
slices.

## Consequences

- Denia can represent non-HTTP workloads without weakening the current HTTP/TLS
  ingress model.
- Operators get explicit `DENIA_TCP_PORT_RANGE` and `DENIA_UDP_PORT_RANGE`
  controls instead of ad hoc public port binding.
- Backward compatibility is preserved: existing services without endpoint lists
  project to one default HTTP endpoint using their existing `internal_port`.
- UDP support requires a datagram-preserving transport between the daemon and
  workload namespace; the existing stream-only `socket-proxy` is insufficient.
- Native gRPC remains intentionally unavailable through Denia-managed TLS until
  HTTP/2 hardening is designed and tested.

## Alternatives Considered

- **Enable HTTP/2 now for gRPC:** rejected by ADR-032. gRPC is valuable, but
  enabling HTTP/2 without header-count, stream, and stalled-flow-control
  protections would knowingly reopen a denied protocol path.
- **Use only domain-based routing:** rejected because TCP and UDP game server
  protocols do not carry HTTP Host headers and usually expect public ports.
- **Let users request exact ports in v1:** deferred. Auto-allocation avoids
  collisions and keeps the first API smaller. Exact requested ports can be added
  with authorization and conflict handling later.
- **Allow UDP scale-to-zero best effort:** rejected for v1. UDP has no
  connection to hold during activation; dropping or buffering early datagrams
  needs explicit semantics.

## References

- ADR-020 (in-process Pingora ingress)
- ADR-032 (HTTP/2 ingress hardening)
- ADR-018 (autoscaling)
- ADR-005 (runtime security hardening)
- `docs/superpowers/specs/2026-06-04-general-purpose-ingress-design.md`
- `docs/superpowers/plans/2026-06-04-general-purpose-ingress.md`
