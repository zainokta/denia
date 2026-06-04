# General-Purpose Protocol Ingress Design

## Goal

Support non-HTTP workloads such as game servers while preserving Denia's current
Docker-free runtime and in-process HTTP ingress architecture.

## Decisions

- Keep HTTP and WebSocket on Pingora `:80`/`:443` with verified hostname
  routing.
- Add service endpoint modeling for `http`, `tcp`, and `udp`.
- Auto-allocate public TCP and UDP ports from operator-configured ranges.
- Keep TCP/UDP endpoints always-on in the first L4 release.
- Defer native gRPC until HTTP/2 ingress satisfies ADR-032.
- Use an isolated worktree for the implementation branch.

## Current Implementation Slice

This slice intentionally avoids changing `RuntimeStartRequest`, deployment
launch, autoscale ownership, or privileged runtime behavior because GitNexus
impact analysis marks `RuntimeStartRequest` as CRITICAL risk.

Implemented foundation:

- `ServiceEndpointProtocol` and `ServiceEndpoint` domain types.
- Legacy projection from `ServiceConfig.internal_port` to a default HTTP
  endpoint when `endpoints` is empty.
- Validation for endpoint names, internal ports, and HTTP public-port shape.
- `PortRange` parsing and `PortAllocator` for deterministic first-free
  allocation.
- `DENIA_TCP_PORT_RANGE` and `DENIA_UDP_PORT_RANGE` config parsing with defaults.

## Later Runtime Slice

The next implementation slice should add SQLite endpoint/port persistence,
runtime endpoint socket paths, TCP listeners, UDP datagram transport, and
WebSocket E2E coverage. It must run fresh GitNexus impact analysis for every
modified runtime/deploy/autoscale symbol before editing.

## Acceptance Criteria

- Existing service JSON remains backward compatible.
- Existing services without endpoints still expose one HTTP endpoint on
  `internal_port`.
- Malformed L4 port ranges fail config loading.
- Port allocation is deterministic and reuses released ports.
- HTTP/2 remains disabled until ADR-032 requirements are implemented.
