# ADR-028: Deploy→Autoscale Replica Ownership Handoff

- **Status**: Accepted
- **Date**: 2026-05-29

## Context

The deploy path and the autoscale controller were two decoupled subsystems that
shared only the runtime and the ingress socket pool — **never** the autoscaler's
`ReplicaRegistry`. The deploy coordinator (`finalize`/`deploy` in
`src/deploy/coordinator.rs`) unconditionally, for **every** service:

1. `runtime.start(replica_index 0)`,
2. health-checked it,
3. `promote_deployment`,
4. `write_routing_config` → registered the workload's socket in the ingress pool
   as `DEPLOY_REPLICA_ID` (`Uuid::nil()`) and marked it healthy.

The autoscale `ReplicaRegistry` (`src/autoscale/registry.rs`) is populated only
by `launch_replica` on tick/activate, or by `reconcile_boot` adoption at startup.
`/v1/workloads` (`src/api/observability.rs`) counts **only** the registry.

So when an **autoscaled** service was deployed at runtime, the coordinator
started a `DEPLOY_REPLICA_ID` workload and marked it healthy in ingress, but the
registry stayed empty. The observed symptoms (reported by the operator for a
`min=0,max=3` service):

- `/v1/workloads` reported `replica_count = 0` while the service served traffic.
- No cold start: `resolve_or_activate` found the deploy socket via `next_socket`
  and returned immediately, never invoking the activator.
- No scale-to-zero: `tick` read `start = registry.replica_count = 0`, hit the
  `start==0`/`min==0` branch, wrote `desired=0`, and continued — it could not
  drain a workload it did not track, and never logged anything.

The autoscaler only adopted the workload on a daemon **restart** via
`reconcile_boot`, and ADR-027 already notes that adoption is "effectively inert"
across a real process boundary because `list_running()` starts empty. ADR-027
deliberately kept *plain* services out of the autoscaler, but nothing bridged a
*runtime deploy of an autoscaled service* into it — the coordinator's
unconditional `DEPLOY_REPLICA_ID` start shadowed the autoscaler.

## Decision

The autoscaler is the single owner of replicas for `autoscale.is_some()`
services from deploy-time onward.

1. **Coordinator splits routing.** `write_routing_config` is split into
   `add_deploy_replica` (registers the `DEPLOY_REPLICA_ID` ingress endpoint) and
   `write_route_table` (rebuilds the verified-domains → service_id route table).
   `finalize`/`deploy` branch on `service.autoscale`:
   - **Plain**: unchanged — start `replica_index 0`, health-check,
     `add_deploy_replica`, `write_route_table`.
   - **Autoscaled**: do **not** start a workload or register an ingress replica.
     Only `promote_deployment` + `write_route_table` (so Host/SNI routing and the
     scale-to-zero activator can resolve the service even at zero replicas).
2. **Deploy hands off to the controller.** After a successful autoscaled deploy,
   the API layer (`src/api/deployments.rs`, in the spawned deploy task) calls a
   new `Controller::reconcile_service(service_id)` — a single-service version of
   `reconcile_boot`'s start-bounded top-up loop (no adopt/orphan pass). It
   launches `min` replicas (each health-gated by `launch_replica`), or none for
   `min==0`. The periodic tick remains the safety net.
3. **Deploy success is decoupled from launch.** For autoscaled services, promote
   + route table ⇒ `Healthy`. For `min>=1`, capacity/health failure during
   `reconcile_service` emits an event (logged) but does not fail the deploy —
   consistent with ADR-018 ("capacity denial is an event, not a failure") and the
   boot top-up path. For `min==0`, the activator health-gates the first
   cold-start replica.
4. **Redeploy reuses the tick rollout.** `reconcile_service` is start-bounded, so
   when replicas already exist it launches nothing; the existing `tick` rollout
   (drain-then-launch, one replica per tick) rolls them onto the new deployment.
5. **Stop drains via the controller.** The autoscaled stop path
   (`src/api/services.rs`) calls a new `Controller::drain_all(service_id)` FIRST
   (drains every replica, releases the ledger, removes ingress + registry
   entries), THEN `DeploymentCoordinator::stop_service_routes_only` (rebuilds the
   route table, marks the deployment `Stopped`, clears the promoted row). Order
   matters: `drain_all` resolves limits via the catalog while the service is still
   promoted, and clearing the promoted row stops the autoscaler relaunching.

## Consequences

- `/v1/workloads` reports the real replica count for autoscaled services;
  scale-to-zero, cold-start, and scale-up/down all work as designed in ADR-018.
- Plain (non-autoscaled) services are unchanged: they keep the
  `DEPLOY_REPLICA_ID` / `replica_index 0` convention used by stop and redeploy.
- An autoscaled service never has a `DEPLOY_REPLICA_ID` ingress entry, so a
  scale-to-zero genuinely empties the pool and the activator fires on the next
  request — there is no nil-id endpoint shadowing it.
- No data migration: the ingress pool is in-memory and wiped per daemon session
  (ADR-027), so any stale pre-fix `DEPLOY_REPLICA_ID` entry disappears on the
  next restart; the first post-fix deploy/boot re-establishes pools cleanly.
- The autoscaled stop holds the controller lock across the per-replica
  `drain_grace` window inside the request handler. Lock ordering is safe (every
  controller-holding path takes the ingress pool lock *after* the controller lock;
  the activation gate is only taken *before* it), so this serializes against ticks
  but cannot deadlock.

## Alternatives Considered

- **Give the coordinator an `Arc<Controller>` field.** Rejected: the coordinator
  is generic over `R: Runtime, H: HealthChecker`, while the controller is a
  concrete type behind `Arc<tokio::sync::Mutex<>>`; threading it through every
  constructor and test wrapper is intrusive. The API layer already holds the
  handle, so the hand-off lives there.
- **Rely on the periodic tick alone (no `reconcile_service`).** Rejected: up to
  `autoscale_interval_s` (default 30s) of `/v1/workloads = 0` and a routable but
  backend-less service after deploy.
- **Adopt the deploy-started `DEPLOY_REPLICA_ID` replica into the registry**
  rather than letting the controller launch its own. Rejected: it would require
  reconciling the nil-id ingress entry with the controller's UUIDv7 entries (a
  dual-id pool hazard) and complicate scale-to-zero teardown. Letting the
  controller own launches from the start keeps a single replica-id space.
- **Fail the deploy if zero replicas come up (min>=1).** Rejected: diverges from
  ADR-018's capacity-denial-is-an-event contract and from the boot top-up path.

## References

- ADR-018 (Autoscaling) — `reconcile_boot`, `tick` rollout, scale-to-zero/activator;
  this ADR adds the previously-missing deploy→controller link for runtime deploys.
- ADR-027 (Daemon Lifecycle) — boot autostart keeps *plain* services out of the
  autoscaler (`autostart_plain_promoted` filters `autoscale.is_none()`); that
  remains correct. This ADR extends ownership so RUNTIME deploys of autoscaled
  services also hand off to the controller. The `DEPLOY_REPLICA_ID` /
  `replica_index 0` convention is now explicitly plain-services-only.
- ADR-020 (In-Process Pingora Ingress) — `DEPLOY_REPLICA_ID`, route table, UDS upstreams.
- ADR-024 (Async Deployments) — `DeploymentLogWriter`, deployment status model.
- `src/autoscale/controller.rs` — `reconcile_service`, `drain_all`, `top_up_to_desired`, `drain_all_replicas`.
- `src/deploy/coordinator.rs` — `add_deploy_replica`, `write_route_table`, `stop_service_routes_only`.
- `src/api/deployments.rs`, `src/api/services.rs` — deploy hand-off + stop branch.
