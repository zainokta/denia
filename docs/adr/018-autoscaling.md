# ADR-018: Autoscaling

- **Status**: Accepted
- **Date**: 2026-05-27

## Context

Today a service maps 1:1 to one workload: one process, one cgroup, one ingress
Unix socket, one Denia loopback bridge port exposed to Traefik as a single
server. There is no mechanism to run replicas of a service or to react to load
changes. TODO #14 asks for Kubernetes-HPA-like behavior: declare CPU/memory
targets, auto-clone into replicas when load rises, load-balance across them,
account for remaining host resources, and scale back down — including to zero.

## Decision

| Dimension | Choice | Rationale |
|-----------|--------|-----------|
| Trigger signal | CPU or memory; memory triggers scale-UP only | Memory cannot be safely reclaimed by shrinking mid-request; CPU is bidirectional |
| Replica budget | Per-replica fixed — each replica receives the full `ResourceLimits` | Predictable resource accounting; matches k8s HPA semantics |
| Capacity exhaustion | Reject scale-up; emit event; keep running at current replica count | Single-node: never overcommit or OOM the host or control plane; reserved host headroom enforced by a resource ledger |
| Load balancing | Denia bridge fan-out behind one stable port per service | The bridge already proxies and logs every request; keeps Traefik config static; required for the scale-to-zero activator — Traefik config does NOT change on scale events |
<!-- ADR-020 update: load balancing is in-process Pingora fan-out (Unix-socket upstreams via HttpPeer::new_uds), replacing the original Denia bridge fan-out; no external Traefik involved. -->
| Anti-flap | k8s-style cooldown: scale-up fast, scale-down only after a stabilization window | Prevents oscillation under bursty load |
| Policy location | Optional field on `ServiceConfig` (`None` = current 1:1 behavior) | Backward-compatible; existing services are unaffected |
| Replica persistence | Hybrid — `desired_replicas` persisted in SQLite, live replica handles held in memory | Survives control-plane restarts; avoids serializing OS handles |
| Clone source | A replica binds to the service's active `deployment_id`; redeploy rolls replicas to the new deployment (drain-then-launch, one at a time) | Keeps all replicas coherent; matches rolling-update semantics |
| Scale-to-zero | Included; a bridge-resident activator holds the connection and cold-starts a workload on the first request; idle detection uses the bridge's existing per-request activity log | Zero idle cost; activator is colocated with the load balancer already present |

## Consequences

- The Denia bridge becomes a stateful load balancer and scale-to-zero activator,
  not just a single-backend proxy. It must track live replica endpoints and
  route intelligently.
- The Linux runtime gains a per-replica identity: rootfs, Unix socket, cgroup,
  and child-process tracking are all keyed by a replica index. Cgroup paths gain
  a trailing replica-index segment (e.g. `denia/<service>/<replica>`). A
  replica-scoped stop and a `list_running` enumeration are added to the runtime
  interface.
- Scale-to-zero adds a cold-start latency penalty on the first request to an
  idle service; the activator holds the connection during boot.
- A resource ledger with reserved host headroom protects the single-node control
  plane from accidental overcommit; scale-up requests that would exceed the
  ledger are rejected with an event rather than silently failing.
- Traefik configuration remains static across scale events, which preserves the
  existing file-provider integration and avoids Traefik reloads on every
  scale-up or scale-down.

## Alternatives Considered

- **Multiple Traefik server entries per replica**: direct mapping of each replica
  to a Traefik server, rewriting the file-provider config on every scale event.
  Rejected — causes Traefik reloads on every scale change, breaks the stable
  single-port contract, and cannot support scale-to-zero without an activator
  layer inside Traefik itself.
- **Memory-bidirectional scaling (scale down to reclaim memory)**: allow memory
  pressure to trigger scale-down as well as scale-up. Rejected — reclaiming
  memory from a live replica mid-request risks OOM-killing in-flight work; the
  asymmetric policy (UP only for memory) is safer.
- **Variable per-replica `ResourceLimits` (bin-packing)**: assign each replica a
  fractional share of the host. Rejected — makes capacity accounting complex and
  diverges from the k8s HPA model that operators already understand.
- **External autoscaler daemon**: a separate process that calls the Denia API to
  adjust replica counts. Rejected — increases operational surface and latency;
  the control loop is simpler inside the single-node control plane where it has
  direct access to cgroup metrics and the resource ledger.

## References

- `docs/superpowers/specs/2026-05-27-autoscaling-design.md`
- TODO #14 (HPA-like autoscaling)
- ADR-003 (Linux Runtime Process Runner) — runtime identity model extended by this ADR
- ADR-007 (Ingress + TLS) — bridge architecture that the load-balancer and activator build on
