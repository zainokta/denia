# Autoscaling (HPA-like) Implementation Plan — Pingora ingress

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Status:** supersedes `docs/superpowers/plans/2026-05-27-autoscaling.md`. The previous plan assumed a stateful loopback bridge in `src/ingress/bridge.rs` and a static Traefik file-provider config; ADR-020 removed both. Denia now binds `:80`/`:443` itself via an in-process Pingora 0.8 proxy whose `ProxyHttp` impl dials workload Unix sockets directly through `HttpPeer::new_uds`. There is no `bridge.rs`, no `BridgeAllocator`, no `bridge_port`, and no Traefik supervisor. Every responsibility the old plan parked on the loopback bridge (replica pool, round-robin LB, `last_activity`, cold-start activator, drain state) now lives inside the Pingora-shared `IngressState` (`src/ingress/pingora/state.rs`) and the `ProxyHttp` implementation `DeniaProxy` (`src/ingress/pingora/proxy.rs`).

**Goal:** Add per-service, CPU/memory-triggered replica autoscaling (including scale-to-zero) with Pingora-fronted load balancing and host resource accounting to the single-node control plane.

**Architecture:** A new `src/autoscale/` module holds pure decision logic (policy, scale math, resource ledger, in-memory replica registry) plus a control loop on the existing scheduler pattern. The Pingora `IngressState` (already present per ADR-020) gains the per-service replica pool, round-robin selection, idle tracking, and single-flight cold-start activation. The Linux runtime gains a per-replica cgroup/socket/rootfs discriminator. `desired_replicas` is persisted in SQLite; live replica handles stay in memory. **Pingora is the load balancer** — there is no Traefik to reconfigure on scale events.

**Tech Stack:** Rust 2024, axum (control plane), Pingora 0.8 (data plane, dedicated thread), tokio, rusqlite (SQLite), cgroup v2 + procfs.

Spec: `docs/superpowers/specs/2026-05-27-autoscaling-design.md`
ADRs: `docs/adr/018-autoscaling.md` (autoscaling decisions), `docs/adr/020-pingora-ingress.md` (ingress transport that this plan plugs into).

---

## CHANGES_FROM_PREVIOUS

A diff of intent against `docs/superpowers/plans/2026-05-27-autoscaling.md`. Every entry below changed because ADR-020 replaced the Traefik+bridge ingress with in-process Pingora.

| Section | Change | Why |
|---|---|---|
| Goal / Architecture | "Denia bridge becomes a pool + activator" → "`IngressState` (Pingora) becomes a pool + activator"; "Traefik file provider (unchanged)" deleted | The bridge file and Traefik are both gone (ADR-020). Pingora is the LB. |
| File structure (Modified) | `src/ingress/bridge.rs` → `src/ingress/pingora/state.rs` + `src/ingress/pingora/proxy.rs`; `src/ingress/traefik.rs` row deleted | The bridge file no longer exists. The shared route+pool brain lives in `IngressState`; the request hot path lives in `DeniaProxy: ProxyHttp`. |
| Phase 8 (fan-out) | Rewritten from scratch. Old: extend `LoopbackBridgeSupervisor`/`BridgeTask` with a TCP listener per service. New: extend `IngressState` with per-service `Vec<ReplicaEndpoint{ replica_id, socket_path, healthy }>`, an `ArcSwap`-friendly mutation API (`add_replica`/`set_replica_healthy`/`remove_replica`/`healthy_count`/`next_socket`/`last_activity`), and a round-robin cursor over Healthy endpoints. The pool is the ProxyHttp upstream source — `DeniaProxy::upstream_peer` calls `IngressState::resolve_or_activate` and `HttpPeer::new_uds` with the returned path. No TCP listener per service exists. | The hot path is now Pingora's `upstream_peer`, not a per-service TCP accept loop. UDS upstreams are dialed directly. |
| Phase 9 (cold start) | Rewritten from scratch. Old: per-service `tokio::sync::Mutex` latch inside the bridge accept loop; on failure write 503 to the TCP stream. New: `ActivationHook` trait on `IngressState`; per-service single-flight gate inside `resolve_or_activate`; `DeniaProxy::upstream_peer` maps `Ok(None)`/`Err(_)` through `classify_resolution` to `UpstreamChoice::Unavailable` and emits a 503 via `Session::respond_error(503)`. Bounded by `ACTIVATION_WAIT` (`tokio::time::timeout`). Controller implements `ActivationHook::activate(service)`. | Pingora owns the response writing path; there is no raw TCP stream to write into. Test surface stays: synthetic ProxyHttp inputs + a fake `ActivationHook`. |
| Phase 10 (wiring) | Old: build registry/ledger/controller in `app.rs`, spawn loop in `main.rs`, connect bridge `ActivationHook` to the controller. New: same, but the activator is installed via `IngressState::set_activator(...)` on the shared `Arc<IngressState>` already constructed for the Pingora server. No `LoopbackBridgeSupervisor` to plumb. | The hook lives on `IngressState`, not on a bridge supervisor. |
| Phase 11 (observability) | Same goal (`replica_count` in `WorkloadView`). No change in shape; data still comes from `ReplicaRegistry`. | Untouched by ADR-020. |
| Phase 12 (e2e) | Fake-runtime + fake-`IngressState` lifecycle test; no fake bridge manager. | The bridge supervisor no longer exists; `IngressState::default()` is the in-test fan-out and accepts a fake `ActivationHook`. |
| Notes for the implementer | "Traefik file config does NOT change" removed. Added: "Pingora is the LB — there is no proxy config to rewrite on scale events. Route changes go through `IngressState::swap_routes` (lock-free for readers); pool changes go through `add_replica`/`set_replica_healthy`/`remove_replica` (Mutex-guarded mutations behind an immutable `Arc<IngressState>` handle)." | New transport reality. |
| Task numbering | Now contiguous Tasks 1..21. Old "Task 0: ADR" is dropped because ADR-018 is already Accepted; the ADR step is replaced by Task 1, which appends an ADR-018 follow-up note pointing at ADR-020 for the transport. | ADR-018 is already in place; the plan should not re-author it. |

---

## Conventions for every task

- TDD: write the failing test, run it red, implement minimal, run green, commit.
- All persisted/keyed UUIDs use `uuid::Uuid::now_v7()` (project rule).
- Typed errors with `thiserror` at boundaries; no panics for expected failures.
- Verify per task: `cargo test <module>` then `cargo fmt --all`. Full gate before final commit of each phase: `cargo build && cargo test && cargo clippy --all-targets --all-features`.
- Commit message format: `<type>(<scope>): message` (`feat`, `fix`, `docs`, `test`, `refactor`).
- Line numbers in this plan are **hints from authoring time and drift** — locate by symbol name, not by line.
- Do NOT run privileged tests unless on a root host: `DENIA_RUN_PRIVILEGED_TESTS=1 cargo test --test linux_runtime_privileged -- --ignored`.
- The replica pool key in `IngressState` is `service.id.to_string()` (globally unique, F-3 in ADR-020 spike notes). NEVER key the pool by `service_name` — that is project-scoped and was the C1 BLOCKER during the Pingora cutover.
- Pingora ingress runs on a dedicated `std::thread` (`main.rs`, the `denia-ingress` thread). The autoscale controller runs on the tokio runtime and shares `Arc<IngressState>` with that thread.

## File structure

**New module `src/autoscale/`:**
- `mod.rs` — module exports.
- `scaler.rs` — pure desired-replica math + `CooldownState`. No I/O.
- `ledger.rs` — `ResourceLedger`: committed vs. host capacity + headroom. No I/O.
- `registry.rs` — `Replica`, `ReplicaState`, `ReplicaRegistry` (in-memory).
- `usage.rs` — `ServiceUsage` + `UsageSampler` aggregating per-replica `MetricSnapshot`s.
- `lifecycle.rs` — `launch_replica` / `drain_replica` primitives that reserve in the ledger, start the runtime, register the replica, wire the `IngressState` pool, and gate on the health check.
- `catalog.rs` — `RepoServiceCatalog` implementing `ServiceCatalog` (resolve `service_id` strings to a full `ManagedService` from the SQLite repos).
- `controller.rs` — control loop wiring sampler + scaler + ledger + registry + lifecycle + `IngressState`; `tick`, `reconcile_boot`, `activate_one`, `SharedController` (impls `ActivationHook`), `run_until_shutdown`.

**Modified:**
- `src/domain/service.rs` — add `autoscale: Option<AutoscalePolicy>` to `ServiceConfig` (+ field threading in `new`/serde).
- `src/ingress/pingora/state.rs` — extend `IngressState` with per-service `ServicePool { endpoints: Vec<ReplicaEndpoint>, cursor, last_activity }`, `ActivationHook` trait, single-flight `activation_gates`, and `resolve_or_activate`. **This is the only ingress file the autoscaler touches** — there is no `src/ingress/bridge.rs`.
- `src/ingress/pingora/proxy.rs` — `DeniaProxy::upstream_peer` already calls `IngressState::resolve_or_activate` and `HttpPeer::new_uds`; this plan adds the unit-test coverage that activation paths produce `UpstreamChoice::Unavailable` → 503 deterministically (the `classify_resolution` helper).
- `src/observability/metrics.rs` — add per-replica `read_replica(service_name, service_id, deployment_id, replica_index)` matching the runtime's new per-replica cgroup path.
- `src/runtime/linux.rs`, `src/runtime/plan.rs`, `src/runtime/runtime_trait.rs`, `src/runtime/fake.rs`, `src/domain/deployment.rs` (`RuntimeStartRequest`, `RuntimeStatus`, `RuntimeInstanceId`) — per-replica cgroup + socket discriminator, replica-scoped `stop`, and `list_running` enumeration.
- `src/repo/sqlite/autoscale.rs` (new) + `src/repo/sqlite/pool.rs` (migration) + `src/state.rs` (facade methods `get_desired_replicas` / `set_desired_replicas`) — persist `desired_replicas` keyed by `service_id`.
- `src/app.rs` / `src/main.rs` — construct registry/ledger/controller, install activator via `IngressState::set_activator`, spawn the loop alongside the Pingora ingress thread, run `reconcile_boot_all` once on startup. Env config in `src/config.rs`.
- `src/api/observability.rs` — expose `replica_count` and `healthy_replicas` on `WorkloadView`.

**Docs:** No new ADR; `docs/adr/018-autoscaling.md` is Accepted. If the implementation diverges from ADR-018 (it should not), update that ADR — do not author a new one. Cross-reference ADR-020 in any consequence notes.

---

## Phase 0 — Domain policy

### Task 1: `AutoscalePolicy` type + validation + cross-ref ADR-018

**Files:**
- Modify: `src/domain/service.rs` (add type near `ResourceLimits`; add field to `ServiceConfig`)
- Modify: `docs/adr/018-autoscaling.md` (append a one-line note that the load-balancing row reads "in-process Pingora fan-out" per ADR-020, replacing the original "Denia bridge fan-out")
- Test: inline `#[cfg(test)]` in `src/domain/service.rs`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn autoscale_policy_validates_bounds() {
    let ok = AutoscalePolicy { min_replicas: 0, max_replicas: 3, target_cpu_pct: 80, target_mem_pct: Some(75), scale_down_cooldown_s: 300, idle_timeout_s: 600 };
    assert!(ok.validate().is_ok());
    let bad = AutoscalePolicy { min_replicas: 5, max_replicas: 2, ..ok.clone() };
    assert!(bad.validate().is_err());
    let bad2 = AutoscalePolicy { idle_timeout_s: 100, scale_down_cooldown_s: 300, ..ok.clone() };
    assert!(bad2.validate().is_err());
    let bad3 = AutoscalePolicy { target_cpu_pct: 0, ..ok.clone() };
    assert!(bad3.validate().is_err());
}
```

- [ ] **Step 2:** Run red.
- [ ] **Step 3: Implement**

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoscalePolicy {
    pub min_replicas: u32,
    pub max_replicas: u32,
    pub target_cpu_pct: u8,
    pub target_mem_pct: Option<u8>,
    pub scale_down_cooldown_s: u32,
    pub idle_timeout_s: u32,
}
```

Validate `max_replicas >= 1`, `min_replicas <= max_replicas`, `1..=100` target percentages, `idle_timeout_s >= scale_down_cooldown_s`. Add `InvalidAutoscale(String)` variant to `DomainError`. Add `#[serde(default)] pub autoscale: Option<AutoscalePolicy>` to `ServiceConfig`, default `None` in `ServiceConfig::new`, call `policy.validate()?` in `new` when `Some`.

- [ ] **Step 4:** Run green. Confirm existing `ServiceConfig` tests still pass (serde `default` keeps backward compat).
- [ ] **Step 5: Commit**

```bash
git add src/domain/service.rs docs/adr/018-autoscaling.md
git commit -m "feat(domain): add AutoscalePolicy to ServiceConfig"
```

---

## Phase 1 — Pure scale math

### Task 2: desired-replica computation

**Files:**
- Create: `src/autoscale/mod.rs`, `src/autoscale/scaler.rs`
- Modify: `src/lib.rs` (add `pub mod autoscale;`)
- Test: inline in `scaler.rs`

- [ ] **Step 1: Failing tests** (encode spec Component 4 asymmetry)

```rust
#[test]
fn scale_up_uses_max_of_cpu_and_mem() {
    // current=2, cpu 90/target80 => ceil(2*90/80)=3 ; mem 50/target75 => ceil(2*50/75)=2 => up=3
    assert_eq!(desired_up(2, 90, 80, Some(50), Some(75)), 3);
}
#[test]
fn scale_down_ignores_memory() {
    assert_eq!(desired_down(2, 20, 80), 1);
}
#[test]
fn clamp_respects_bounds_never_zero_from_loop() {
    assert_eq!(clamp_loop(0, 1, 5), 1);
    assert_eq!(clamp_loop(0, 0, 5), 1); // even with min=0, loop floor is 1
    assert_eq!(clamp_loop(9, 1, 5), 5);
}
```

- [ ] **Step 2:** Run red.
- [ ] **Step 3: Implement** pure functions:

```rust
fn ceil_div(a: u64, b: u64) -> u64 { a.div_ceil(b.max(1)) }

pub fn desired_up(current: u32, cpu_pct: u32, target_cpu: u8,
                  mem_pct: Option<u32>, target_mem: Option<u8>) -> u32 {
    let c = ceil_div(current as u64 * cpu_pct as u64, target_cpu as u64) as u32;
    let m = match (mem_pct, target_mem) {
        (Some(mp), Some(tm)) => ceil_div(current as u64 * mp as u64, tm as u64) as u32,
        _ => 0,
    };
    c.max(m).max(current)
}
pub fn desired_down(current: u32, cpu_pct: u32, target_cpu: u8) -> u32 {
    ceil_div(current as u64 * cpu_pct as u64, target_cpu as u64) as u32
}
pub fn clamp_loop(desired: u32, min: u32, max: u32) -> u32 {
    desired.clamp(min.max(1), max)
}
```

- [ ] **Step 4:** Run green.
- [ ] **Step 5: Commit** `feat(autoscale): pure desired-replica math`.

### Task 3: cooldown / stabilization state machine

**Files:** Modify `src/autoscale/scaler.rs`; tests inline.

- [ ] **Step 1: Failing test**

```rust
#[test]
fn cooldown_gates_scale_down_only() {
    let mut st = CooldownState::default();
    assert!(!st.scale_down_allowed(0, 300));
    assert!(!st.scale_down_allowed(299, 300));
    assert!(st.scale_down_allowed(300, 300));
    st.note_above_target(310);
    assert!(!st.scale_down_allowed(320, 300));
}
```

- [ ] **Step 2-4:** Implement `CooldownState { below_since: Option<u64> }`. `note_above_target(now)` clears `below_since`; `scale_down_allowed(now, cooldown_s)` returns true once `now - below_since >= cooldown_s` (setting `below_since` on first below call). Run red→green.
- [ ] **Step 5:** Commit `feat(autoscale): scale-down cooldown state machine`.

---

## Phase 2 — ResourceLedger

### Task 4: ResourceLedger accounting

**Files:** Create `src/autoscale/ledger.rs`; export in `mod.rs`; tests inline.

- [ ] **Step 1: Failing test** (Pending+Healthy+Draining all count; headroom subtracted).

```rust
#[test]
fn ledger_denies_when_exceeding_capacity_minus_headroom() {
    let mut l = ResourceLedger::new(HostCapacity { cpu_millis: 4000, mem_bytes: 4<<30 },
                                    Headroom { cpu_millis: 1000, mem_bytes: 1<<30 });
    let lim = ResourceLimits { cpu_millis: 1000, memory_bytes: 1<<30 };
    assert!(l.try_reserve(&lim).is_ok());
    assert!(l.try_reserve(&lim).is_ok());
    assert!(l.try_reserve(&lim).is_ok()); // 3000mc/3GiB == allocatable
    assert!(l.try_reserve(&lim).is_err());
    l.release(&lim);
    assert!(l.try_reserve(&lim).is_ok());
}
```

- [ ] **Step 2-4:** Implement `HostCapacity`, `Headroom`, `ResourceLedger { committed_cpu, committed_mem, allocatable_cpu, allocatable_mem }`. `try_reserve(&ResourceLimits) -> Result<(), LedgerError::InsufficientCapacity>`; `release(&ResourceLimits)`. Reservation precedes spawn so concurrent scale-ups cannot double-spend. Run red→green.
- [ ] **Step 5:** Commit `feat(autoscale): resource ledger with headroom`.

### Task 5: HostCapacity from sysinfo

**Files:** Modify `src/autoscale/ledger.rs`; reuse `crate::observability::parse_meminfo` (already public from `src/observability/node_metrics.rs`).

- [ ] **Step 0 (verify):** Open `src/observability/node_metrics.rs` and confirm `parse_meminfo` exposes `(MemTotal, MemAvailable)`. Reuse it; do NOT add a duplicate parser or a new dep.
- [ ] Implement `HostCapacity::detect()` → `cpu_millis = available_parallelism()*1000`, `mem_bytes` from MemTotal. Add a unit test that returns non-zero values on the test host.
- [ ] Commit `feat(autoscale): detect host capacity`.

---

## Phase 3 — Replica registry

### Task 6: Replica + ReplicaState + ReplicaRegistry

**Files:** Create `src/autoscale/registry.rs`; export; tests inline.

- [ ] **Step 1: Failing tests** — add/transition/remove; count Healthy; round-robin pick over Healthy only.

```rust
#[test]
fn registry_round_robin_over_healthy_only() {
    let mut reg = ReplicaRegistry::default();
    let svc = Uuid::now_v7();
    let r1 = reg.add(svc, Uuid::now_v7(), 0, "/run/denia/s-0.sock".into());
    let r2 = reg.add(svc, Uuid::now_v7(), 1, "/run/denia/s-1.sock".into());
    reg.set_state(r1, ReplicaState::Healthy);
    reg.set_state(r2, ReplicaState::Draining);
    assert_eq!(reg.next_healthy(svc).map(|r| r.id), Some(r1));
    assert_eq!(reg.healthy_count(svc), 1);
}
```

- [ ] **Step 2-4:** Implement `Replica { id (Uuid::now_v7()), service_id, deployment_id, index, socket_path, state, started_at }`, `ReplicaState { Pending, Healthy, Draining, Stopped }`, `ReplicaRegistry` keyed by `service_id -> Vec<Replica>` with a per-service round-robin cursor; `next_healthy` skips non-Healthy. Expose `replicas(service_id) -> &[Replica]` and `replica_count(service_id)`. Run red→green.
- [ ] **Step 5:** Commit `feat(autoscale): in-memory replica registry`.

---

## Phase 4 — Persist `desired_replicas`

### Task 7: SQLite migration + repo + state facade

**Files:**
- Create: `src/repo/sqlite/autoscale.rs` (query fns, follow `services.rs` shape).
- Modify: `src/repo/sqlite/pool.rs` (add migration), `src/repo/sqlite/mod.rs` (mod line), `src/state.rs` (facade `get_desired_replicas` / `set_desired_replicas`).
- Test: in `src/repo/sqlite/autoscale.rs` using an in-memory pool.

- [ ] **Step 1: Failing test** — round-trip desired count keyed by `service_id`; default `None`.

```rust
#[test]
fn desired_replicas_round_trip() {
    let store = SqliteStore::open_in_memory().unwrap();
    store.migrate().unwrap();
    let svc = Uuid::now_v7();
    assert_eq!(store.get_desired_replicas(svc).unwrap(), None);
    store.set_desired_replicas(svc, 3).unwrap();
    assert_eq!(store.get_desired_replicas(svc).unwrap(), Some(3));
    store.set_desired_replicas(svc, 5).unwrap();
    assert_eq!(store.get_desired_replicas(svc).unwrap(), Some(5));
}
```

- [ ] **Step 2:** Run red.
- [ ] **Step 3: Implement** migration `CREATE TABLE IF NOT EXISTS autoscale_desired (service_id TEXT PRIMARY KEY, desired_replicas INTEGER NOT NULL)` in the migration list in `pool.rs`. Add `get_desired_q`/`set_desired_q` (upsert via `ON CONFLICT(service_id) DO UPDATE`) in `autoscale.rs`. Add `SqliteStore::get_desired_replicas` / `set_desired_replicas` in `state.rs`.
- [ ] **Step 4:** Run green.
- [ ] **Step 5:** Commit `feat(repo): persist desired_replicas`.

---

## Phase 5 — Runtime replica identity + per-replica metrics

> **Critical prerequisite:** the runtime is currently per-service-name. `LinuxRuntime::start` tracks children keyed by service name, `stop(service_name)` removes by service name, and `Runtime` has no enumeration. Running multiple replicas of one service therefore requires a runtime identity rework FIRST. Tasks 10 (launch/drain) and 17 (boot reconcile) depend on this.

### Task 8: replica-scoped runtime identity, stop, and enumeration

**Files:**
- Modify: `src/domain/deployment.rs` — add `replica_index: u32` to `RuntimeStartRequest` (`#[serde(default)]`); introduce `RuntimeInstanceId { service_id: Uuid, service_name: String, replica_index: u32 }` (hash/eq on `(service_id, replica_index)`); add `RuntimeStatus { service_id, service_name, deployment_id, replica_index, socket_path }`.
- Modify: `src/runtime/runtime_trait.rs` — change `stop` signature to `async fn stop(&self, instance: &RuntimeInstanceId) -> Result<(), RuntimeError>;`. Add `async fn list_running(&self) -> Result<Vec<RuntimeStatus>, RuntimeError> { Ok(Vec::new()) }` with a default impl for back-compat.
- Modify: `src/runtime/linux.rs` — key the children map by `RuntimeInstanceId`; thread `replica_index` into the rootfs leaf (`…/{service_id}/{deployment_id}/{replica_index}`), the in-rootfs socket path, the host socket path, **and** the cgroup path (the cgroup path is derived independently from `cgroup_root/service_id/deployment_id`, so add the replica segment explicitly: `cgroup_root/service_id/deployment_id/replica_index`). `RuntimeStatus` returns the real per-replica `socket_path` so consumers (lifecycle/`IngressState`) read it from there.
- Modify: `src/runtime/fake.rs` — mirror the new key + `list_running` (return what `start` recorded).
- Update all existing `stop(...)`/start call sites (deploy coordinator, scheduler/job path, existing tests) to pass `replica_index: 0` and the new `RuntimeInstanceId`.

- [ ] **Step 1: Failing test** in `src/runtime/linux.rs`: `LinuxRuntime::plan` for the same service+deployment with `replica_index: 0` vs `1` yields distinct `rootfs_path`, host `socket_path`, and `cgroup_path`. Add a fake-runtime test that `list_running` returns one entry per `start`.
- [ ] **Step 2:** Run red.
- [ ] **Step 3:** Implement; update fake; update call sites.
- [ ] **Step 4:** `cargo build` to catch all call-site breaks; fix them. Run `cargo test`.
- [ ] **Step 5:** Commit `refactor(runtime): replica-scoped identity, stop, and enumeration`.

### Task 9: ServiceUsage aggregation + sampler with prev-tick state

**Files:** Create `src/autoscale/usage.rs`; modify `src/observability/metrics.rs`. Tests inline.

> **Verify first:** `CgroupMetricsReader` (`src/observability/metrics.rs`) currently exposes `read_service` and `read_by_id` returning `MetricSnapshot { cpu_usage_usec, memory_current_bytes }`. CPU is a **cumulative** counter, so a percentage needs a delta between ticks. Prev-tick state lives in a `UsageSampler` owned by the controller, NOT in the stateless reader.

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn cpu_pct_from_delta() {
    // 800ms CPU over a 1s window at a 1000m limit => 80%
    assert_eq!(cpu_pct(0, 800_000, 1_000_000, 1000), 80);
    assert_eq!(cpu_pct(0, 800_000, 1_000_000, 500), 160);
    assert_eq!(cpu_pct(0, 800_000, 0, 1000), 0);
}
#[test]
fn usage_aggregates_avg_cpu_and_mem() {
    let u = ServiceUsage::aggregate(&[(80, 50), (40, 75)]);
    assert_eq!((u.avg_cpu_pct, u.avg_mem_pct, u.max_mem_pct, u.replica_count), (60, 62, 75, 2));
}
```

- [ ] **Step 2:** Run red.
- [ ] **Step 3:** Implement `cpu_pct(prev_usec, cur_usec, window_us, cpu_millis) -> u32`; `ServiceUsage { avg_cpu_pct, avg_mem_pct, max_mem_pct, replica_count }` + `aggregate`; `UsageSampler { prev: HashMap<Uuid, (u64, Instant)> }` with `sample(service_name, replicas, reader, limits) -> ServiceUsage`. Add `read_replica(service_name, service_id, deployment_id, replica_index)` to `CgroupMetricsReader` matching the cgroup path from Task 8.
- [ ] **Step 4:** Run green.
- [ ] **Step 5:** Commit `feat(autoscale): metric aggregation + usage sampler`.

---

## Phase 6 — Launch / drain primitives

### Task 10: replica launch + drain via runtime + `IngressState`

**Files:** Create `src/autoscale/lifecycle.rs`. Reuse `RuntimeStartRequest` and `Runtime` (post-Task 8). Test with `FakeRuntime` + an `IngressState::default()` + a fake `HealthChecker`.

- [ ] **Step 1: Failing test** — `launch_replica(spec, &mut registry, &mut ledger, &runtime, &ingress, &health)`:
  - reserves in the ledger,
  - builds a `RuntimeStartRequest` with the right `replica_index`,
  - calls `runtime.start` and reads the returned `RuntimeStatus.socket_path`,
  - registers a `Pending` replica in `ReplicaRegistry`,
  - calls `ingress.add_replica(&service_id.to_string(), replica_id, socket_path)` (unhealthy),
  - runs `health.check(...)`; on pass → `registry.set_state(Healthy)` + `ingress.set_replica_healthy(..., true)`; on fail → stop runtime, release ledger, remove from registry + ingress pool. Returns `replica_id` on Ok or a typed `LifecycleError { Capacity, Runtime(String), Health }` on failure.
  - `drain_replica(service_key, replica_id, &instance, &limits, grace, &mut registry, &mut ledger, &runtime, &ingress)`: sets Draining (calls `ingress.set_replica_healthy(..., false)`), sleeps `grace`, calls `runtime.stop(&instance)` (ignoring stop errors), then UNCONDITIONALLY releases the ledger and removes from registry + ingress pool so resources never leak.

  Pool key is `service_id.to_string()`. NEVER use `service_name`.

- [ ] **Step 2-4:** Implement using the fakes (`FakeRuntime` has `list_running` + replica-scoped stop from Task 8); keep the real health check call behind the existing `HealthChecker` trait. Tests must cover: success path; health failure rollback (ledger released, replica removed, runtime stopped); capacity-denied (no `start` call, no registry/ingress mutation). Run red→green.
- [ ] **Step 5:** Commit `feat(autoscale): replica launch and drain primitives`.

---

## Phase 7 — `IngressState` fan-out (REWRITTEN for Pingora)

### Task 11: `IngressState` per-service replica pool + round-robin + `last_activity`

**Files:** Modify `src/ingress/pingora/state.rs`. There is **no** `src/ingress/bridge.rs`.

`IngressState` already owns `routes: ArcSwap<RouteTable>` and `certs: ArcSwap<CertStore>` (per ADR-020). This task adds a per-service pool whose mutations happen behind a `tokio::sync::Mutex<BTreeMap<String, ServicePool>>` field — chosen over `ArcSwap` because writers are frequent (every health-state flip, every `last_activity` advance) and readers do not need lock-free traversal (they take a short async lock, dial UDS, return; the bottleneck is the workload, not the pool).

> **Concurrency rationale (mandatory note in the doc comment):** routes/certs swap rarely and are read on every request → `ArcSwap`. Pool entries mutate on every accept (cursor advance, last_activity update) and on every health transition → `Mutex<BTreeMap<...>>`. Mixing the two is intentional and consistent with the rest of the Pingora module.

- [ ] **Step 1: Failing tests** in `src/ingress/pingora/state.rs`:

```rust
#[tokio::test]
async fn next_socket_round_robins_healthy_replicas() {
    let state = IngressState::default();
    let a = Uuid::now_v7(); let b = Uuid::now_v7();
    let pa = PathBuf::from("/run/denia/a.sock"); let pb = PathBuf::from("/run/denia/b.sock");
    state.add_replica("svc", a, pa.clone()).await;
    state.add_replica("svc", b, pb.clone()).await;
    assert_eq!(state.healthy_count("svc").await, 0); // unhealthy by default
    assert_eq!(state.next_socket("svc").await, None);

    state.set_replica_healthy("svc", a, true).await;
    state.set_replica_healthy("svc", b, true).await;
    let s1 = state.next_socket("svc").await.unwrap();
    let s2 = state.next_socket("svc").await.unwrap();
    let s3 = state.next_socket("svc").await.unwrap();
    assert_ne!(s1, s2);
    assert_eq!(s1, s3);

    state.set_replica_healthy("svc", a, false).await;
    assert_eq!(state.next_socket("svc").await, Some(pb.clone()));
    state.remove_replica("svc", b).await;
    assert_eq!(state.next_socket("svc").await, None);
}

#[tokio::test]
async fn next_socket_advances_last_activity() {
    let state = IngressState::default();
    let id = Uuid::now_v7();
    state.add_replica("svc", id, "/run/denia/z.sock".into()).await;
    state.set_replica_healthy("svc", id, true).await;
    let before = state.last_activity("svc").await.unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;
    let _ = state.next_socket("svc").await.unwrap();
    let after = state.last_activity("svc").await.unwrap();
    assert!(after > before);
}
```

- [ ] **Step 2:** Run red.
- [ ] **Step 3:** Implement:

```rust
pub struct ReplicaEndpoint {
    pub replica_id: Uuid,
    pub socket_path: PathBuf,
    pub healthy: bool,
}
struct ServicePool {
    endpoints: Vec<ReplicaEndpoint>,
    cursor: usize,
    last_activity: Instant,
}
```

Add `pools: Mutex<BTreeMap<String, ServicePool>>` to `IngressState`. Public async API:
- `add_replica(&self, service: &str, replica_id: Uuid, socket_path: PathBuf)` — default `healthy = false`, replaces entry with same `replica_id`.
- `set_replica_healthy(&self, service: &str, replica_id: Uuid, healthy: bool)` — no-op if absent.
- `remove_replica(&self, service: &str, replica_id: Uuid)` — no-op if absent; clamp cursor.
- `healthy_count(&self, service: &str) -> usize`.
- `last_activity(&self, service: &str) -> Option<Instant>`.
- `set_last_activity(&self, service: &str, when: Instant)` (test backdating).
- `next_socket(&self, service: &str) -> Option<PathBuf>` — round-robin over `healthy == true` endpoints, advancing the cursor and bumping `last_activity = Instant::now()`. `None` when no healthy endpoint exists.

Run red→green.

- [ ] **Step 4:** Verify `DeniaProxy::upstream_peer` already uses these via `state.resolve_or_activate(&route.service_id)` and `HttpPeer::new_uds(...)` (no change to the proxy required *yet* — see Task 12).
- [ ] **Step 5:** Commit `feat(ingress): per-service replica pool with round-robin in IngressState`.

---

## Phase 8 — Cold-start activator (REWRITTEN for Pingora)

### Task 12: `ActivationHook` + single-flight `resolve_or_activate` + 503 mapping in `ProxyHttp`

**Files:** Modify `src/ingress/pingora/state.rs` and `src/ingress/pingora/proxy.rs`. There is no per-service TCP accept loop to write 503 into — the `ProxyHttp` impl writes the response via `Session::respond_error(503)`.

The activator lives inside `IngressState`:

```rust
#[async_trait::async_trait]
pub trait ActivationHook: Send + Sync {
    async fn activate(&self, service: &str) -> Result<(), ActivationError>;
}

#[derive(Debug, Error)]
pub enum ActivationError {
    #[error("activation timed out")]
    Timeout,
    /// Capacity exhausted — workload would launch if capacity were available. The
    /// client can retry later. Pingora maps this to 503 with a Retry-After hint.
    #[error("activation denied: {0}")]
    Denied(String),
    /// Activation hard-failed (image pull, health probe, runtime error). No retry hint.
    #[error("activation failed: {0}")]
    Failed(String),
}

pub const ACTIVATION_WAIT: std::time::Duration = std::time::Duration::from_secs(30);
```

`IngressState` gains:
- `activator: Mutex<Option<Arc<dyn ActivationHook>>>`
- `activation_gates: Mutex<BTreeMap<String, Arc<Mutex<()>>>>` (per-service single-flight)
- `pub async fn set_activator(&self, hook: Arc<dyn ActivationHook>)` — install the hook (called once during startup).
- `pub async fn resolve_or_activate(&self, service: &str) -> Result<Option<PathBuf>, ActivationError>`:
  1. Try `next_socket(service)`; if Some, return.
  2. If no activator is set, return `Ok(None)` (the proxy maps this to 503).
  3. Take the per-service single-flight gate (`activation_gates.entry(service).or_insert_with(...).clone()`).
  4. Lock the gate. Recheck `next_socket` (another waiter may have already activated).
  5. Wrap `activator.activate(service)` + a bounded post-activation poll loop (a small number of retries with a tens-of-ms delay) in `tokio::time::timeout(ACTIVATION_WAIT, ...)`. Return `Err(Timeout)` on elapse, `Err(Failed)` from the hook, or the resolved socket on success.

The proxy hot path (`DeniaProxy::upstream_peer` in `src/ingress/pingora/proxy.rs`):
- Resolves `Host` via the routes table. Unknown host → 404 via `Session::respond_error(404)`.
- The pool key is `route.service_id` (the F-3 fix). NEVER use `service_name`.
- Calls `state.resolve_or_activate(&route.service_id).await`.
- Pure helper `classify_resolution(Result<Option<PathBuf>, ActivationError>) -> UpstreamChoice` maps:
  - `Ok(Some(p)) → Uds(p)`,
  - `Ok(None) → Unavailable { retry_after: None }`,
  - `Err(ActivationError::Denied(_)) → Unavailable { retry_after: Some(Duration::from_secs(5)) }` (capacity — client should retry),
  - `Err(ActivationError::Timeout) | Err(ActivationError::Failed(_)) → Unavailable { retry_after: None }`.
  On `Unavailable`, the proxy calls `Session::respond_error(503)` and, when `retry_after` is set, also injects a `Retry-After: <secs>` header. On `Uds(socket)`, the proxy returns `HttpPeer::new_uds(&socket.to_string_lossy(), false, host)`.
- All decision logic — `classify_port80` (`:80` challenge / redirect / passthrough) and `classify_resolution` — must remain free functions so they are unit-tested without a live `Session` or socket.

- [ ] **Step 1: Failing tests** in `src/ingress/pingora/state.rs` (and `proxy.rs` for the pure helpers):

```rust
// Single-flight: N concurrent resolves trigger exactly one activation.
#[tokio::test]
async fn concurrent_resolves_trigger_single_activation() {
    let state = Arc::new(IngressState::default());
    let calls = Arc::new(AtomicUsize::new(0));
    state.set_activator(Arc::new(SlowCountingActivator { state: state.clone(), calls: calls.clone() })).await;
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..16 {
        let s = state.clone();
        set.spawn(async move { s.resolve_or_activate("svc").await });
    }
    while let Some(j) = set.join_next().await { j.unwrap().unwrap(); }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

// Hang → bounded by ACTIVATION_WAIT (uses tokio::test(start_paused = true)).
#[tokio::test(start_paused = true)]
async fn activation_times_out_when_hook_hangs() {
    let state = IngressState::default();
    state.set_activator(Arc::new(HangingActivator)).await;
    assert!(matches!(state.resolve_or_activate("svc").await, Err(ActivationError::Timeout)));
}

// Failure releases the latch so a later request retries fresh.
#[tokio::test]
async fn activation_failure_resets_latch() {
    let state = Arc::new(IngressState::default());
    let hook = Arc::new(FakeActivator::with_first_failure(state.clone()));
    state.set_activator(hook).await;
    assert!(state.resolve_or_activate("svc").await.is_err());
    assert!(state.resolve_or_activate("svc").await.unwrap().is_some());
}

// Pure 503 mapping (no Session needed).
#[test]
fn resolution_maps_to_unavailable_with_correct_retry_hint() {
    assert_eq!(
        classify_resolution(Ok(None)),
        UpstreamChoice::Unavailable { retry_after: None }
    );
    assert_eq!(
        classify_resolution(Err(ActivationError::Timeout)),
        UpstreamChoice::Unavailable { retry_after: None }
    );
    assert_eq!(
        classify_resolution(Err(ActivationError::Failed("x".into()))),
        UpstreamChoice::Unavailable { retry_after: None }
    );
    assert_eq!(
        classify_resolution(Err(ActivationError::Denied("insufficient_capacity".into()))),
        UpstreamChoice::Unavailable { retry_after: Some(Duration::from_secs(5)) }
    );
}
```

- [ ] **Step 2:** Run red.
- [ ] **Step 3: Implement**
  - Add `ActivationHook`/`ActivationError`/`ACTIVATION_WAIT` to `state.rs`.
  - Add the `activator` + `activation_gates` fields, `set_activator`, `resolve_or_activate`.
  - Add `pub enum UpstreamChoice { Uds(PathBuf), ControlBackend, NotFound, Unavailable }` and `pub fn classify_resolution(...)` in `proxy.rs` (kept as a pure free function).
  - In `DeniaProxy::upstream_peer`, after host resolution use `state.resolve_or_activate(&route.service_id)`; `match classify_resolution(...)` to either build `HttpPeer::new_uds` or call `session.respond_error(503).await`.

- [ ] **Step 4:** Run green. Add an end-to-end Pingora test (in `tests/pingora_ingress_e2e.rs` or a new sibling) that drives a request through a real `pingora_proxy::http_proxy_service` instance with an `IngressState` whose activator is a fake controller, and asserts: cold service → activator fires once → 200 from the launched fake workload UDS; hung activator → 503 within `ACTIVATION_WAIT`; 16 concurrent waiters → exactly one activator call. Use `pingora` test helpers as the existing e2e file does.
- [ ] **Step 5:** Commit `feat(ingress): single-flight cold-start activator and 503 mapping`.

> **Audit note to keep in the doc comment of `resolve_or_activate`:** An unauthenticated client on `:80`/`:443` can wake any scaled-to-zero *routed* service — this is the documented unauthenticated-activation posture in ADR-020. Bounded by the per-service single-flight gate and `ACTIVATION_WAIT`; do NOT add cross-service rate limiting here (out of scope).

---

## Phase 9 — Controller loop

### Task 13: `ServiceCatalog` + `ManagedService` + repo-backed catalog

**Files:** Create `src/autoscale/catalog.rs`; tests inline using `SqliteStore::open_in_memory`.

- [ ] **Step 1: Failing tests** — `RepoServiceCatalog::all()` returns only services that are (a) autoscaled, (b) have a promoted deployment, (c) that deployment is linked to an artifact, (d) the project resolves. `resolve(service_id_str)` parses the id and returns the same `ManagedService`. Non-UUID / unknown id → `None`.
- [ ] **Step 2-4:** Implement `ManagedService { service_name, service_id, deployment_id, policy, artifact, internal_port, limits, env, health_check }`, the `ServiceCatalog` trait (`resolve(&self, service_key: &str) -> Option<ManagedService>`, `all(&self) -> Vec<ManagedService>`), and `RepoServiceCatalog::new(services, projects, deployments)`. `resolve` parses `service_key` as a UUID (the activator passes `route.service_id`, which is the F-3 fix). Run red→green.
- [ ] **Step 5:** Commit `feat(autoscale): repo-backed service catalog`.

### Task 14: autoscale controller tick

**Files:** Create `src/autoscale/controller.rs`. A `tick(&[ManagedService], now_s) -> Vec<AutoscaleEvent>` that, per service: reads `ServiceUsage`, computes up/down desired (Task 2), applies cooldown (Task 3), clamps (loop floor `max(min,1)`), reconciles via ledger + launch/drain (Task 10), persists `desired_replicas` to the ACTUAL achieved count (not the target).

> **Event sink decision:** `tick` returns `Vec<AutoscaleEvent>` (variants: `ScaledUp{service, from, to}`, `ScaledDown{..}`, `ScaleUpDenied{service, reason}`, `ScaledToZero{service}`, `RolloutStep{service, to_deployment}`, `Adopted{service, replica_index}`, `OrphanRemoved{service, replica_index}`). Tests assert on the returned vec; the spawn loop also logs each event via `tracing`. Persist the achieved count, not the target, so a capacity-denied partial scale-up reflects reality.

- [ ] **Step 1: Failing test** (fakes for runtime/`IngressState`/usage/store/catalog):
  - High CPU → `ScaledUp`, registry gains a replica (Pending→Healthy), `IngressState.healthy_count` increments by one, `desired_replicas` persisted +1.
  - Sustained low CPU past cooldown → `ScaledDown`, drains one replica, `IngressState.healthy_count` decrements.
  - Host at capacity → `ScaleUpDenied{reason: "insufficient_capacity"}`, no launch, count unchanged.

- [ ] **Step 2-4:** Implement `AutoscaleEvent` enum and `Controller { registry, ledger, runtime, ingress: Arc<IngressState>, health, store, usage, catalog, cooldowns, drain_grace }` with `tick`, plus a `tick_all(now_s)` that pulls `catalog.all()` and delegates to `tick`. Run red→green.
- [ ] **Step 5:** Commit `feat(autoscale): controller tick`.

### Task 15: idle → scale-to-zero

**Files:** Modify `src/autoscale/controller.rs`.

- [ ] **Step 1: Failing test** — service with `min_replicas == 0`, `now - ingress.last_activity > idle_timeout_s`, low CPU → tick drains ALL replicas to zero and persists `desired = 0`; emits `ScaledToZero`. With `min_replicas >= 1`, never zeros even if idle.
- [ ] **Step 2-4:** Add an idle branch in `tick`. The idle key is `ingress.last_activity(&service_id.to_string())`. Memory is scale-up-only, so it must NOT block scale-to-zero. Run red→green.
- [ ] **Step 5:** Commit `feat(autoscale): idle scale-to-zero`.

### Task 16: 0→1 activation entry point (`activate_one` + `SharedController`)

**Files:** Modify `src/autoscale/controller.rs`. Implement `pub async fn activate_one(&mut self, service: &str) -> Result<(), ActivationError>` that:
1. Resolves the `service_key` (which is `service.id.to_string()`) via `self.catalog.resolve`.
2. If `registry.replica_count >= 1`, returns `Ok` (lost the activation race).
3. If `policy.max_replicas == 0`, returns `Ok` (defensive; validation forbids it).
4. Calls `launch_replica` with `replica_index = 0`; maps `LifecycleError::Capacity → ActivationError::Denied("insufficient_capacity")` (retryable — host may free up), `LifecycleError::Health → Failed("health")`, `LifecycleError::Runtime(e) → Failed(e)`.

Then expose `SharedController(pub Arc<tokio::sync::Mutex<Controller>>)` and `impl ActivationHook for SharedController { async fn activate(&self, service: &str) -> Result<(), ActivationError> { self.0.lock().await.activate_one(service).await } }` so cold-start activation and the periodic tick serialize on one lock.

- [ ] **Step 1: Failing test** — `SharedController::activate("<service_id>")` launches exactly one replica and `IngressState.healthy_count` becomes 1; an `IngressState::resolve_or_activate(&service_id)` after the hook returns immediately picks up that replica. Failure modes: capacity denial → `Err(Denied("insufficient_capacity"))`; hung runtime (so health never passes) → mapped to `Err(Failed("health"))`.
- [ ] **Step 2-5:** Implement; run red→green; commit `feat(autoscale): activation hook for scale-from-zero`.

---

## Phase 10 — Rollout + boot reconcile

### Task 17: rolling replace on redeploy (in-tick)

**Files:** Modify `src/autoscale/controller.rs`. Detect in-tick: if any live replica in `registry.replicas(service_id)` runs `deployment_id != ms.deployment_id`, perform exactly one rollout step this tick and `continue` past normal scaling for that service.

- Multi-replica (total > 1): drain-then-launch (maxUnavailable=1, no surge). Drain the oldest old-deployment replica, then launch a new one at `next_replica_index`. Emit `RolloutStep{ to_deployment }`.
- Single replica (total == 1): launch-then-drain (brief +1 surge) so the service never drops to zero capacity mid-rollout.

Persist `desired_replicas` to the ACTUAL post-step count.

- [ ] **Step 1: Failing tests** — three replicas of D1, new active D2 → controller replaces one-at-a-time over multiple ticks; mid-rollout autoscale scaling is deferred (tick early-returns past normal scaling); for a single-replica service it uses launch-then-drain (briefly observes 2 replicas in `IngressState`).
- [ ] **Step 2-4:** Implement; run red→green.
- [ ] **Step 5:** Commit `feat(autoscale): rolling replace on redeploy`.

### Task 18: boot reconcile + orphan adoption

**Files:** Modify `src/autoscale/controller.rs`; add `pub async fn reconcile_boot(&mut self, services: &[ManagedService]) -> Vec<AutoscaleEvent>` and `pub async fn reconcile_boot_all(&mut self) -> Vec<AutoscaleEvent>`.

Algorithm:
1. `let running = self.runtime.list_running().await.unwrap_or_default();` (from Task 8).
2. For each `RuntimeStatus`:
   - If a `ManagedService` matches BY `service_id` AND `deployment_id == ms.deployment_id` AND `ledger.try_reserve(&ms.limits).is_ok()`: register Pending in the registry, set Healthy, call `ingress.add_replica(&ms.service_id.to_string(), id, status.socket_path)` + `set_replica_healthy(..., true)`. Emit `Adopted{ replica_index }`.
   - Otherwise (unknown service, stale deployment, or no budget): build a `RuntimeInstanceId { service_id, service_name, replica_index }` from the status and call `runtime.stop(&instance)`. Emit `OrphanRemoved`. Stale on-disk socket files are not a blocker because the launch path already removes a pre-existing socket file before binding (`socket_proxy::run`).
3. After enumeration, for each managed service: look up persisted `desired_replicas` (default to `policy.min_replicas`), clamp to `[min, max]`, and launch additional replicas to reach the target via `launch_replica`. On `LifecycleError::Capacity` emit `ScaleUpDenied{ reason: "insufficient_capacity" }` and stop launching that service.

- [ ] **Step 1: Failing test** (fakes) — persisted `desired_replicas = 2` for service S; fake `list_running` returns one running replica of S with the current deployment + one orphan from another service. `reconcile_boot_all`: adopts the matching one (Healthy, `IngressState.healthy_count == 1` for S), stops the orphan (calls `runtime.stop` with the right instance id), then tops up to 2.
- [ ] **Step 2-4:** Implement; run red→green.
- [ ] **Step 5:** Commit `feat(autoscale): boot reconcile and orphan adoption`.

---

## Phase 11 — Wiring (REWRITTEN for Pingora)

### Task 19: construct + spawn controller; install activator on `IngressState`; env config

**Files:** Modify `src/app.rs`, `src/main.rs`, `src/config.rs`.

In `src/app.rs::AppState::new`:
- Build `ReplicaRegistry::default()`, `ResourceLedger::new(HostCapacity::detect(), Headroom::from_env())`, `CgroupUsageSource::new(CgroupMetricsReader::new(cgroup_root))`, `RepoServiceCatalog::new(services, projects, deployments)`.
- Build `Controller::new(registry, ledger, runtime.clone(), ingress.clone(), health.clone(), store.clone(), usage, catalog, Duration::from_secs(30))`.
- Wrap in `Arc<tokio::sync::Mutex<Controller>>` and store on `AppState.autoscaler`.
- Expose `pub fn autoscaler_handle(&self) -> Option<(Arc<IngressState>, Arc<Mutex<Controller>>)>` for `main` to wire.

In `src/main.rs`, AFTER the Pingora ingress thread is spawned and BEFORE `axum::serve(...).await`:

```rust
let autoscaler_task = if let Some((ingress, controller)) = state.autoscaler_handle() {
    ingress.set_activator(Arc::new(SharedController(controller.clone()))).await;
    {
        let mut c = controller.lock().await;
        let _ = c.reconcile_boot_all().await;
    }
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let handle = tokio::spawn(autoscale::controller::run_until_shutdown(
        controller,
        Duration::from_secs(config.autoscale_interval_s),
        rx,
    ));
    Some((tx, handle))
} else { None };
```

`run_until_shutdown(controller, interval, shutdown_rx)` lives in `src/autoscale/controller.rs` and mirrors `src/scheduler.rs::run_until_shutdown`: `tokio::time::interval(interval)` calling `controller.lock().await.tick_all(now_s).await` until `shutdown_rx` fires.

On graceful shutdown, send `()` on the autoscaler `tx` and `.await` the task before the process exits, as `main` already does for `scheduler_task`, `pingora_shutdown_tx`, and `acme_task`.

`src/config.rs` adds:
- `DENIA_AUTOSCALE_INTERVAL_S` (default 15)
- `DENIA_AUTOSCALE_HEADROOM_CPU_MILLIS` — reserved CPU for axum control plane + Pingora ingress + ACME renewal task. **Default: `500`** (500 mc ≈ half a core; covers steady-state control plane with headroom for concurrent deploys).
- `DENIA_AUTOSCALE_HEADROOM_MEM_BYTES` — reserved RAM for the same. **Default: `268435456`** (256 MiB; control-plane binary, SQLite cache, in-flight layer staging buffers, Pingora cert store).

- [ ] **Step 1:** Add the `run_until_shutdown` helper and unit-test it minimally (a single tick with a fake controller).
- [ ] **Step 2:** Wire into `AppState::new` and `main.rs`. Call `reconcile_boot_all` once before spawning the loop. Install `SharedController` as the `IngressState` activator via `set_activator`.
- [ ] **Step 3:** Add env parsing in `config.rs` with documented defaults.
- [ ] **Step 4:** `cargo build && cargo test`. If running locally, `cargo run` should boot, `/healthz` should return 200, and there should be no log spam on idle ticks. If on a non-root host, skip privileged tests and rely on `cargo test`.
- [ ] **Step 5:** Commit `feat(app): wire and spawn autoscale controller alongside Pingora ingress`.

### Task 20: observability surface

**Files:** Modify `src/api/observability.rs` (`WorkloadView`) and the handler that builds it (`list_workloads`).

- [ ] **Step 1: Failing test** — with a service that has N registered replicas in the autoscaler, the workloads endpoint returns `replica_count = N` and `healthy_replicas = <healthy count>` (additive fields).
- [ ] **Step 2-4:** Add `replica_count: u32` and `healthy_replicas: u32` to `WorkloadView`. Pull them from `state.autoscaler` (Option). When no autoscaler is wired (test builder path), default to 0. Keep the response backward compatible (only additive). Run red→green.
- [ ] **Step 5:** Commit `feat(api): expose replica count in observability`.

---

## Phase 12 — End-to-end

### Task 21: integration test — full lifecycle through Pingora

**Files:** Create `tests/autoscale_lifecycle.rs` (`FakeRuntime` + an `IngressState::default()` configured with `SharedController` + in-memory `SqliteStore` + fake `UsageSource` driven scripted CPU/mem).

- [ ] **Step 1: Failing test** driving the controller through:
  1. Policy `min=0, max=3`; start idle → zero replicas; `IngressState.healthy_count == 0`.
  2. Simulate a request: call `IngressState::resolve_or_activate(&service_id)` directly (no real Pingora server needed for this test) → activator launches 1, returns the new socket; `healthy_count == 1`.
  3. Drive high CPU metrics through the fake `UsageSource` → scales to 3 (capped at max); `healthy_count == 3`.
  4. Reduce host capacity in the ledger to force denial on a fourth replica → `ScaleUpDenied` emitted, count stays at 3.
  5. Drop CPU past cooldown → scales down to 1.
  6. Idle past `idle_timeout_s` → scales to 0; `ScaledToZero` emitted.
  7. Redeploy mid-load → rolling replace one-at-a-time; mid-rollout autoscaling deferred.

- [ ] **Step 2-4:** Implement; iterate until green.
- [ ] **Step 5:** Commit `test(autoscale): full lifecycle through IngressState`.

### Task 22: final gate

- [ ] `cargo fmt --all`
- [ ] `cargo build`
- [ ] `cargo test`
- [ ] `cargo clippy --all-targets --all-features`
- [ ] On a root host only: `DENIA_RUN_PRIVILEGED_TESTS=1 cargo test --test linux_runtime_privileged -- --ignored`
- [ ] Commit any fmt/clippy fixes: `chore(autoscale): fmt and clippy`.
- [ ] Report exact commands run + results.

---

## Notes for the implementer

- **Pingora is the load balancer.** There is no Traefik file config. There is no `src/ingress/bridge.rs`. There is no TCP listener per service and no `bridge_port`. Workload UDS upstreams are dialed directly by `DeniaProxy::upstream_peer` via `HttpPeer::new_uds`.
- **Pool key is `service.id.to_string()`** — globally unique. Keying by `service_name` was the C1 BLOCKER during the Pingora cutover (`service_name` is project-scoped). The route table carries both `service_id` (pool key) and `service_name` (access-log only).
- **Concurrency model in `IngressState`:**
  - `routes: ArcSwap<RouteTable>` — rare writes (control-plane mutations), reads on every request: lock-free.
  - `certs: ArcSwap<CertStore>` — rare writes (boot load, ACME issuance/renewal), reads on every TLS handshake: lock-free.
  - `pools: Mutex<BTreeMap<String, ServicePool>>` — frequent writes (every health flip, every `next_socket` advances `last_activity` and the cursor): mutex-guarded. Acquire briefly; never hold across awaits to a workload.
- **Activation hook injection.** The activator is installed once during `main` via `IngressState::set_activator`. The controller exposes `SharedController(Arc<Mutex<Controller>>) : ActivationHook` so the cold-start path and the periodic tick serialize on the same lock — no special concurrency model for activation.
- **Unauthenticated activation posture.** An unauthenticated request on `:80`/`:443` can wake a scaled-to-zero routed service. This is accepted by ADR-020 and bounded by per-service single-flight + `ACTIVATION_WAIT`. Do NOT add a cross-service rate limiter in this plan.
- **Reserve ledger capacity for a replica BEFORE spawning it** (Pending counts), and free it ONLY after the workload is killed (post-drain). The `try_reserve` is the first step of `launch_replica`; the `release` is the last step of `drain_replica`, regardless of stop errors.
- **Memory never drives scale-down.** If you find yourself feeding `mem_pct` into the down path, stop.
- **`list_running()` and replica-scoped `stop`** are added in Task 8. Every later phase assumes Task 8 landed first.
- **`ActivationHook` is a trait** so `IngressState` has no compile-time dependency on the autoscaler module; the controller implements it via `SharedController` (Task 16) and the wiring (Task 19) injects it.
- **Pingora ingress thread vs autoscale tokio task.** The Pingora `Server` runs on a dedicated `std::thread` (see `src/main.rs`). The autoscaler runs on the tokio runtime. They share `Arc<IngressState>`. Do not move the autoscaler onto the Pingora thread — Pingora builds its own tokio runtimes for that thread.
- **Boot ordering.** `state.ingress.swap_certs(load_certs_from_disk(...))` must run BEFORE the Pingora server binds `:443` (already enforced in `main`). `controller.reconcile_boot_all()` must run BEFORE `run_until_shutdown` so the first tick sees a primed registry.
