# Autoscaling (HPA-like) — Design

Date: 2026-05-27
Status: Draft (brainstormed, pending implementation plan)
Relates to: TODO #14. New ADR-016 required (runtime + ingress change).

## Problem

Today a Denia service maps 1:1 to a single workload: one process, one cgroup,
one ingress Unix socket fronted by one Denia loopback bridge port, exposed to
Traefik as a single server. There is no way to run multiple instances of a
service or to react to load. Operators want Kubernetes-HPA-like behavior:
declare CPU/memory targets, automatically clone a service into replicas when
usage exceeds the target, load-balance across replicas, account for remaining
host resources, and scale back down — including all the way to zero.

## Goals

- Per-service autoscaling policy: min/max replicas, CPU and memory targets.
- Automatic scale-up when aggregate CPU or memory crosses target.
- Automatic scale-down with anti-flap stabilization.
- Scale-to-zero with request-driven cold start (activator).
- Load-balance traffic across live replicas.
- Track committed vs. available host resources; never overcommit; protect the
  control plane with reserved headroom.

## Non-goals

- Multi-node scheduling. Single-node control plane only (no node-spreading).
- Memory-based scale-*down* (memory triggers scale-up only; shrinking on memory
  is unsafe with in-flight work).
- Full per-request HTTP metrics as a scaling input (TODO #9). The bridge already
  records per-request activity, which is sufficient for idle detection; richer
  request-rate-based scaling is future work.
- Custom/external metrics adapters.

## Locked decisions

| Decision | Choice | Rationale |
|---|---|---|
| Trigger signal | CPU **or** memory; memory scale-up only | Memory cannot be safely reclaimed by shrinking replicas mid-request. |
| Replica budget | Per-replica fixed: each replica gets the full `ResourceLimits` | Predictable, matches k8s HPA semantics. |
| Capacity exhaustion | Reject scale-up + reserved host headroom; emit event, keep running | Single-node: never destabilize host/control plane via overcommit/OOM. |
| Load balancing | Denia bridge fan-out behind one stable port per service | Bridge already proxies+logs all traffic; keeps Traefik config static; required for scale-to-zero activator. |
| Anti-flap | k8s-style cooldown: scale-up fast, scale-down after stabilization window | Predictable, well-understood. |
| Policy location | Optional field on `ServiceConfig` | Co-located with `resource_limits`; `None` = current 1:1 behavior. |
| Replica persistence | Hybrid: `desired_replicas` in SQLite, live handles in memory | Survives restart without persisting volatile per-replica handles. |
| Clone source | Replica binds to the service's active `deployment_id` | Replicas run the exact built artifact; redeploy rolls replicas to new deployment. |
| Scale-to-zero | In v1, via bridge activator + bridge-log idle detection | Bridge is the only Denia-owned point that can hold a connection and wake a workload. |

## Existing system facts (verified)

- `ServiceConfig` (`src/domain/service.rs:149`) carries `resource_limits:
  Option<ResourceLimits>`, `internal_port`, `health_check`.
- Ingress data path: Traefik → `http://127.0.0.1:{bridge_port}` (one server per
  route, `src/ingress/traefik.rs:140`) → Denia `LoopbackBridge` `tee_proxy`
  (`src/ingress/bridge.rs:208`) → workload Unix socket.
- `tee_proxy` already parses request line + status + bytes + duration and writes
  per-service entries to an `AccessLogStore`. This gives near-free per-service
  last-activity tracking for idle detection — no dependency on TODO #9.
- `BridgeAllocator`/`LoopbackBridgeSupervisor` manage one bridge task per service
  today (one TCP port → one socket).
- Observability reads cgroup v2 + procfs per workload (`src/observability/`).
- Periodic background work has a home in `src/scheduler.rs`.
- Deployments are persisted in SQLite (`src/repo/sqlite/deployments.rs`,
  `src/domain/deployment.rs:45`).

## Architecture

### Component 1 — `AutoscalePolicy` (domain)

Add to `src/domain/service.rs`:

```rust
pub struct AutoscalePolicy {
    pub min_replicas: u32,          // 0 = scale-to-zero allowed
    pub max_replicas: u32,
    pub target_cpu_pct: u8,         // e.g. 80
    pub target_mem_pct: Option<u8>, // None = memory is not a trigger
    pub scale_down_cooldown_s: u32, // stabilization window, e.g. 300
    pub idle_timeout_s: u32,        // scale-to-zero idle window, e.g. 600
}
```

`ServiceConfig.autoscale: Option<AutoscalePolicy>`. `None` preserves today's
1:1 behavior (exactly one instance, no autoscaler involvement). `validate()`
enforces `min_replicas <= max_replicas`, `max_replicas >= 1`, `1..=100`
target percentages, and `idle_timeout_s >= scale_down_cooldown_s` (so CPU
scale-down precedes the idle→zero path; see Component 7).

### Component 2 — Replica model (hybrid persistence, deployment-bound)

- **SQLite**: persist a `desired_replicas` counter per service (column on the
  services table or a small `autoscale_state` row keyed by service id). The
  autoscaler writes it on every scale decision so the count survives restart.
- **In-memory `ReplicaRegistry`**: live handles, not persisted.

```rust
pub struct Replica {
    pub id: Uuid,            // Uuid::now_v7()
    pub service_id: Uuid,
    pub deployment_id: Uuid, // the active deployment this replica runs
    pub index: u32,          // stable ordinal for socket-path derivation
    pub socket_path: PathBuf,
    pub state: ReplicaState, // Pending | Healthy | Draining | Stopped
    pub started_at: DateTime<Utc>,
}
```

- Each replica binds to the service's **active `deployment_id`**; scale-up
  launches another instance of that exact built artifact.
- **Boot reconcile**: read `desired_replicas` from SQLite, relaunch that many
  from the active deployment.
- **Redeploy = rolling replace**: when a new deployment becomes active, the
  autoscaler reconciles replicas from the old `deployment_id` to the new one
  **one at a time, drain-then-launch** (maxUnavailable=1, **no surge**): drain
  one old replica, launch one new replica of the new deployment, wait Healthy,
  repeat. Drain-then-launch keeps the rollout within the existing resource
  budget (no transient 2× footprint that would self-deny via the ledger on a
  tight single node), at the cost of briefly running `desired-1` healthy
  replicas. The healthy floor during rollout is `max(min_replicas, 1)` unless
  `desired == 1`, in which case launch-then-drain is used for that single
  replica to avoid a zero-capacity gap. Autoscale scale events are **deferred**
  while a rollout is in progress (rollout takes priority; the loop resumes after).

### Component 3 — Metrics sampler

Extend `src/observability/` to sample each live replica's cgroup v2 + procfs and
aggregate per service:
- CPU: average CPU% across replicas (% of each replica's CPU limit).
- Memory: average and max mem% across replicas.

Exposes a per-service `ServiceUsage { avg_cpu_pct, avg_mem_pct, max_mem_pct,
replica_count }` for the autoscaler.

### Component 4 — Autoscaler control loop

Periodic task on `src/scheduler.rs`, ~15s tick. For each service with a policy:

1. Read `ServiceUsage` (only defined when `current >= 1`; see Component 7 for
   the 0→1 transition, which the autoscaler loop does **not** own).
2. Compute desired, with CPU and memory treated asymmetrically:
   - `cpu_desired = ceil(current * cpu_pct / target_cpu_pct)`.
   - `mem_desired = ceil(current * mem_pct / target_mem_pct)` when
     `target_mem_pct` is set, **else 0**.
   - **Scale-up** candidate = `max(cpu_desired, mem_desired)`.
   - **Scale-down** candidate = `cpu_desired` **only**. Memory never drives a
     scale-down (memory is scale-up-only), so a memory-resident-but-idle process
     cannot block CPU-driven shrink.
   - `desired = max(scale_up_candidate, current)` if scaling up this tick, else
     `scale_down_candidate`.
3. Clamp to `[max(min_replicas, 1), max_replicas]` (the loop never targets 0;
   the 1→0 step is owned by Component 7).
4. Apply cooldown: scale-up applied immediately; scale-down applied only after
   the CPU metric has stayed below target for `scale_down_cooldown_s`.
5. Reconcile through the `ResourceLedger` (Component 5): launch or drain. The
   live `ReplicaRegistry` is authoritative for actual replicas; `desired_replicas`
   in SQLite is a restore hint, reconciled toward the registry every tick.
6. Persist new `desired_replicas`.

### Component 5 — `ResourceLedger`

- **Units** match `ResourceLimits { cpu_millis: u32, memory_bytes: u64 }`. Host
  totals are normalized to the same units: CPU total = `num_cpus * 1000`
  millicores; memory total = total physical RAM bytes (from sysinfo).
- **Committed set**: a replica counts toward committed from the moment a launch
  is *intended* — i.e. `Pending` reserves its full `ResourceLimits` immediately
  (reservation precedes spawn) so two concurrent scale-ups cannot double-spend
  headroom. `Healthy` replicas count. `Draining` replicas **still count** until
  the workload is killed and the allocation is explicitly freed (Component 8).
  Because rolling replace is drain-then-launch (Component 2), at most one extra
  replica's budget is needed transiently, and that is covered by freeing the
  drained replica *before* reserving the new one.
- Reserved **headroom** (configurable, e.g. `DENIA_AUTOSCALE_HEADROOM_CPU_MILLIS`
  / `DENIA_AUTOSCALE_HEADROOM_MEM_BYTES`, defaults reserve enough for the control
  plane + Traefik) is subtracted from allocatable capacity.
- Scale-up denied when `committed + next_replica.limits > host_total - headroom`:
  emit event `scale_up_denied:insufficient_capacity`, keep current count, retry
  next tick.

### Component 6 — Bridge fan-out + activator

Rework `src/ingress/bridge.rs` so each service's bridge holds a **pool** of
replica sockets instead of one:

- On inbound connection:
  - ≥ 1 `Healthy` replica → round-robin pick → existing `tee_proxy`.
  - Zero replicas (scaled to zero) → **single-flight** launch request (the
    bridge, not the autoscaler loop, owns the 0→1 transition; it still goes
    through the `ResourceLedger` and respects `max_replicas`, but **bypasses the
    scale-down cooldown** since this is a scale-up). Hold the connection until a
    replica is `Healthy` (bounded timeout → return 503), then proxy.
  - **Activator launch failure**: if the single-flight launch fails or times
    out, the held connection and **all concurrent waiters** for that service
    receive 503; the launch latch is released so the *next* inbound request
    retries a fresh launch. No automatic retry storm within one attempt.
- `tee_proxy` continues to log each request; update a per-service
  `last_activity` timestamp used for idle detection.
- Traefik config stays static: still one server (one stable port) per service;
  no rewrite/reload on scale events.

### Component 7 — Idle detection → scale-to-zero

The 1→0 transition is a **separate path** from the metric-driven N→1 scale-down
and has its own trigger: `min_replicas == 0` AND
`now - last_activity > idle_timeout_s` AND aggregate metrics are low. It does
**not** reuse the `scale_down_cooldown_s` metric window; `last_activity` (from
the bridge log, Component 6) is the authority for "is anyone using this." To
avoid oscillation, `validate()` requires `idle_timeout_s >= scale_down_cooldown_s`
so a service fully scales down on CPU before the idle path can zero it. When
triggered, drain all replicas to zero (Component 8); the bridge activator wakes
the service on the next request.

### Component 8 — Draining (scale-down primitive)

To remove a replica: mark it `Draining` (remove from the round-robin pool so it
gets no new connections), wait for in-flight connections to finish up to a grace
timeout, kill the workload, free its `ResourceLedger` allocation, drop the handle.

### Component 9 — API / observability surface

- `WorkloadView` (`src/api/observability.rs`) and observability endpoints expose
  replica count and per-replica metrics.
- Policy is read/written through the existing service config update path (the
  field lives on `ServiceConfig`).

### Component 10 — ADR-016

Author ADR-016 capturing the autoscaling architecture (runtime + ingress change)
per project rules in `docs/adr/`.

## Error handling & edge cases

- **Launch failure mid scale-up**: keep current count, emit event, retry next tick.
- **Cold-start timeout at zero**: 503 to the held connection and all concurrent
  waiters; latch released so the next request retries (Component 6).
- **Thundering herd at zero**: single-flight launch per service so concurrent
  requests trigger exactly one launch.
- **Flapping**: scale-down stabilization window (CPU only).
- **Boot reconcile + orphan adoption**: on startup the registry is empty but
  workloads/cgroups/sockets from before a crash may persist. Boot must (a)
  enumerate leftover replica cgroups + socket files, (b) adopt still-running
  ones into the registry where the deployment still matches, and (c) kill +
  clean up the rest (stale cgroup, stale socket file) before launching to reach
  persisted `desired_replicas` (floor `max(min_replicas, 1)` unless policy
  allows zero and the service was idle). This prevents orphaned workloads after
  a restart mid-drain or mid-rollout.
- **Stale socket files**: replica socket paths are derived from `index`; the
  launch path already removes a pre-existing socket file before binding
  (`socket_proxy::run`), so a crashed replica's leftover socket does not block a
  same-index relaunch.
- **desired/actual drift**: the live `ReplicaRegistry` is the runtime authority;
  the autoscaler reconciles registry → desired each tick, so a failed launch or
  manual kill self-heals on the next tick rather than leaving SQLite and reality
  diverged.
- **Capacity exhaustion**: reject + event (Component 5).
- **Redeploy during high load**: rolling replace is drain-then-launch within
  budget (Component 2); autoscale events defer until the rollout completes.

## Testing strategy

- **Unit**: scale math (CPU & memory formulas, clamp), `ResourceLedger`
  capacity + headroom accounting, cooldown/stabilization state machine,
  round-robin selection, single-flight activation, drain state transitions.
- **Integration** (fake runtime + fake bridge): full scale up → down → zero →
  wake cycle; redeploy rolling replace; capacity-denied path.
- **Privileged** (gated `DENIA_RUN_PRIVILEGED_TESTS=1`): real cgroup sampling and
  real replica launch/teardown.

## Verification commands

- `cargo build`
- `cargo test`
- `cargo fmt --all`
- `cargo clippy --all-targets --all-features`
- `DENIA_RUN_PRIVILEGED_TESTS=1 cargo test --test linux_runtime_privileged -- --ignored`
