# ADR-027: Workload Lifecycle Bound to the Daemon (Stop-All on Shutdown, Autostart on Boot)

- **Status**: Accepted
- **Date**: 2026-05-29

## Context

Stopping the Denia daemon left workloads orphaned: the namespaced processes,
their Unix sockets, and overlay mounts survived the daemon's exit. Restarting the
daemon did **not** bring previously-running services back. Two concrete defects:

1. **Shutdown never stopped workloads.** The graceful-shutdown future was
   `tokio::signal::ctrl_c()` (SIGINT only) and the post-`serve` cleanup only
   signalled/joined background tasks (scheduler, Pingora, ACME, autoscaler, OCI
   GC) — it never called `runtime.stop()`. Worse, `ctrl_c()` does not observe
   SIGTERM, so under systemd (`systemctl stop` → SIGTERM) the entire
   graceful-shutdown path was dead code; the unit was SIGKILLed after
   `TimeoutStopSec=30s`, orphaning every workload.
2. **Boot never restarted plain services.** `fail_orphan_deployments()` only
   transitions *in-flight* deployments (Pending/Building/Starting) to Failed;
   promoted `Healthy` deployments survive in SQLite but nothing relaunched them.
   The autoscaler's `reconcile_boot_all()` restarts only services that carry an
   `autoscale` policy (the catalog excludes the rest), so plain
   single-replica services stayed down.

The user observed both: two leftover `service.sock` files after Ctrl+C, and a
deployed service that did not come back on restart.

## Decision

Bind workload lifecycle to the daemon process, driven by the existing
desired-state signal in the control plane.

**Desired-state model (pre-existing, now load-bearing).** A service whose
promoted deployment is `Healthy` means "should be running". Explicit
`coordinator.stop_service()` sets the deployment `Stopped` **and clears the
`promoted_deployments` row** — that cleared row is the durable "should NOT be
running" signal. Boot therefore restarts a service iff it still has a promoted
deployment; shutdown stop-all must **not** mutate the DB, so promoted/`Healthy`
survive for the next boot.

**1. Shutdown stop-all (`src/daemon.rs`).** The graceful-shutdown future now
resolves on **either** SIGINT (Ctrl+C) or SIGTERM (`systemctl stop`), via a
`shutdown_signal()` helper that `select!`s `ctrl_c()` and a
`tokio::signal::unix` `SignalKind::terminate()` stream. After the background
tasks are joined (the autoscaler first, so no tick can launch a replica
mid-teardown), the daemon enumerates `runtime.list_running()` and calls
`runtime.stop()` on each. Stops run concurrently (a `JoinSet`) wrapped in a 25s
`tokio::time::timeout`, comfortably under the unit's `TimeoutStopSec=30s`, so N
replicas do not serialise N × the per-replica SIGTERM grace. The DB is never
touched here.

**2. Boot orphan sweep (`src/runtime`).** `list_running()` reflects only
in-memory tracking, which is empty on a fresh process — so it cannot see
survivors of a SIGKILL/crash/power-loss, and neither launcher below would either.
A new `Runtime::sweep_orphans()` (default `Ok(0)`; implemented for
`LinuxRuntime`) scans `runtime_dir/{service_id}/{deployment_id}/{replica_index}`
and, for each leftover, in the order **kill → unmount → remove**: writes
`cgroup.kill`, waits briefly for the cgroup to drain, unmounts the `merged`
overlay (`MS_DETACH`), then removes the cgroup leaf and the replica directory
(reusing `remove_cgroup_dir_if_exists` / `remove_dir_if_exists`). It then removes
dangling socket aliases under `<data_dir>/sock` whose symlink target no longer
resolves. The sweep runs once at boot, **before** any launcher, so both the
autoscaler reconcile and the plain autostart start from a clean tree.

**3. Boot autostart (`src/daemon.rs`, `src/deploy/coordinator.rs`).** Autoscaled
services are already restored by the autoscaler's `reconcile_boot_all()`
(unchanged). For **plain** (non-autoscaled) services, a new boot driver
`autostart_plain_promoted()` iterates services with `autoscale.is_none()` and a
promoted deployment, calling a new `DeploymentCoordinator::restart_promoted()`
that resolves the promoted deployment + its linked artifact and reuses the
existing private `finalize()` (runtime start at `replica_index 0` +
healthcheck + idempotent re-promote + `write_routing_config`). A promoted
deployment whose artifact is missing (e.g. GC'd) is logged and skipped. Plain
services are deliberately **not** routed through the autoscaler: it allocates its
own replica ids, which would break the coordinator's single-replica
`DEPLOY_REPLICA_ID`/`replica_index 0` convention used by stop and redeploy.

**4. Per-session log hygiene (`src/observability/logs.rs`).** Each daemon process
lifetime is treated as a "session". On startup, `clean_session_logs(log_dir)`
empties `<log_dir>` — the service workload logs (`{service_id}.log`) and the
`deployments/` subtree — so every session begins with a clean log tree. The wipe
preserves `log_dir` itself (and its `0700` perms from `denia setup`); it does not
remove+recreate the directory. It is best-effort per entry and a missing
`log_dir` is a no-op. The trigger is **startup, not shutdown**, so unclean exits
(SIGKILL/crash/power-loss) are also covered — the next boot cleans them. The
daemon's own diagnostics go to stderr → journald and are out of scope (systemd
manages those). The wipe runs **before** `fail_orphan_deployments()` so its
synthetic `RESTART` markers land in the freshly emptied tree.

**Boot ordering** in `daemon::run`: `clean_session_logs` →
`fail_orphan_deployments` → `relocate_daemon_cgroup` → `sweep_orphans` →
autoscaler `reconcile_boot_all` → `autostart_plain_promoted` → serve.

## Consequences

- Stopping the daemon (Ctrl+C **or** `systemctl stop`) cleanly stops every
  workload — no orphaned processes, sockets, or mounts.
- Starting the daemon re-establishes exactly the set that was running, minus any
  explicitly stopped service (cleared promoted row).
- The SIGTERM fix also makes the pre-existing background-task cleanup actually
  run under systemd, not only under interactive Ctrl+C.
- There is a brief per-service downtime on each control-plane restart: workloads
  are stopped on daemon stop and relaunched on boot — there is no live process
  hand-off across a restart. With the unit's `Restart=on-failure`, a daemon crash
  triggers a systemd restart whose boot sweep + autostart re-establish the fleet.
- `sweep_orphans` needs `CAP_SYS_ADMIN` (cgroup kill, unmount); present in the
  systemd unit. In unprivileged dev those steps fail best-effort and are skipped.

## Risks

- **Stop-all time budget**: bounded by a 25s timeout under `TimeoutStopSec=30s`;
  a timeout logs and defers any survivor to the next boot's orphan sweep.
- **Sweep correctness**: only directories whose names parse as UUID/index are
  visited, so bookkeeping dirs (e.g. `jobs`) and the daemon's own `.daemon`
  cgroup (under `cgroup_root`, not `runtime_dir`) are never touched.
- **Cold-boot adoption**: because `list_running()` is empty on a fresh process,
  the autoscaler's `reconcile_boot` adopt/orphan pass is effectively inert across
  process boundaries; the filesystem sweep is the real reaper. Teaching
  `list_running` to enumerate from disk so the autoscaler can *adopt* survivors
  (instead of kill+relaunch) is a possible future change, deferred here.

## Alternatives Considered

- **Generalise the autoscaler catalog to include plain services** (synthesised
  pinned policy) so one `reconcile_boot_all` handles everything. Rejected: it
  would route plain services through autoscaler-generated replica ids, breaking
  the `DEPLOY_REPLICA_ID` convention that `stop_service`/redeploy depend on, and
  couple every service to per-tick metric sampling and scale-to-zero.
- **Mark workloads `Stopped` on shutdown.** Rejected: it would erase the
  desired-state signal and prevent autostart; explicit stop remains the only path
  that clears the promoted row.
- **Leave workloads running across daemon restarts** (today's accidental
  behaviour). Rejected: the operator asked for a clean stop on daemon stop, and
  orphaned mounts/sockets accumulate.

## References

- ADR-018 (Autoscaling) — `reconcile_boot_all`, catalog opt-in
- ADR-020 (In-Process Pingora Ingress) — `DEPLOY_REPLICA_ID`, route table
- ADR-024 (Async Deployments) — `DeploymentLogWriter`, deployment status model
- `src/daemon.rs` — `shutdown_signal`, `stop_all_workloads`, `autostart_plain_promoted`
- `src/runtime/linux.rs` — `sweep_orphans`
- `src/deploy/coordinator.rs` — `restart_promoted`, `finalize`, `stop_service`
- `src/observability/logs.rs` — `clean_session_logs`
