# Autoscaling (HPA-like) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add per-service, CPU/memory-triggered replica autoscaling (including scale-to-zero) with Denia-bridge load balancing and host resource accounting to the single-node control plane.

**Architecture:** A new `src/autoscale/` module holds pure decision logic (policy, scale math, resource ledger, in-memory replica registry) plus a control loop on the existing scheduler pattern. The Denia loopback bridge becomes a per-service replica pool + cold-start activator. The Linux runtime gains a per-replica cgroup/socket discriminator. `desired_replicas` is persisted in SQLite; live replica handles stay in memory.

**Tech Stack:** Rust 2024, axum, tokio, rusqlite (SQLite), cgroup v2 + procfs, Traefik file provider (unchanged config).

Spec: `docs/superpowers/specs/2026-05-27-autoscaling-design.md`

---

## Conventions for every task

- TDD: write the failing test, run it red, implement minimal, run green, commit.
- All persisted/keyed UUIDs use `uuid::Uuid::now_v7()` (project rule).
- Typed errors with `thiserror` at boundaries; no panics for expected failures.
- Verify per task: `cargo test <module>` then `cargo fmt --all`. Full gate before final commit of each phase: `cargo build && cargo test && cargo clippy --all-targets --all-features`.
- Commit message format: `<type>(<scope>): message`.
- Line numbers in this plan are hints from authoring time and **drift** — locate by symbol name (the type/fn referenced), not by line.
- Do NOT run privileged tests unless on a root host: `DENIA_RUN_PRIVILEGED_TESTS=1 cargo test --test linux_runtime_privileged -- --ignored`.

## File structure

**New module `src/autoscale/`:**
- `mod.rs` — module exports.
- `policy.rs` — `AutoscalePolicy`, `validate()` (or place policy type in `src/domain/`; see Task 1).
- `scaler.rs` — pure desired-replica math + cooldown state machine. No I/O.
- `ledger.rs` — `ResourceLedger`: committed vs. host capacity + headroom. No I/O.
- `registry.rs` — `Replica`, `ReplicaState`, `ReplicaRegistry` (in-memory).
- `usage.rs` — `ServiceUsage` + aggregation from per-replica `MetricSnapshot`s.
- `controller.rs` — control loop wiring sampler + scaler + ledger + registry + launcher + bridge; `run_until_shutdown`.

**Modified:**
- `src/domain/service.rs` — add `autoscale: Option<AutoscalePolicy>` to `ServiceConfig` (+ field threading in `new`/serde).
- `src/ingress/bridge.rs` — replica pool fan-out, round-robin, `last_activity`, single-flight activator.
- `src/observability/metrics.rs` — per-replica cgroup read.
- `src/runtime/linux.rs`, `src/runtime/plan.rs`, `src/domain/deployment.rs` (`RuntimeStartRequest`) — per-replica cgroup + socket discriminator.
- `src/repo/sqlite/` (new `autoscale.rs` query file) + `src/repo/sqlite/pool.rs` (migration) + `src/state.rs` (facade methods) — persist `desired_replicas`.
- `src/app.rs` / `src/main.rs` — construct registry/ledger/controller, spawn loop, env config.
- `src/api/observability.rs` — expose replica count/per-replica metrics.

**Docs:** `docs/adr/016-autoscaling.md` + `docs/adr/README.md` row.

---

## Phase 0 — ADR

### Task 0: ADR-016

**Files:**
- Create: `docs/adr/016-autoscaling.md`
- Modify: `docs/adr/README.md`

- [ ] **Step 1:** Write ADR-016 following the format of `docs/adr/015-streaming-oci-layer-staging.md`. Capture: context (TODO #14), the locked decisions table from the spec, consequences (bridge becomes stateful LB + activator; runtime gains per-replica cgroup; scale-to-zero cold-start latency; single-node headroom protection). Status: Accepted.
- [ ] **Step 2:** Add the ADR-016 row to `docs/adr/README.md`.
- [ ] **Step 3:** Commit.

```bash
git add docs/adr/016-autoscaling.md docs/adr/README.md
git commit -m "docs(adr): ADR-016 autoscaling"
```

---

## Phase 1 — Domain: AutoscalePolicy

### Task 1: AutoscalePolicy type + validation

**Files:**
- Modify: `src/domain/service.rs` (add type near `ResourceLimits` at line 11; add field to `ServiceConfig` at line 149)
- Test: inline `#[cfg(test)]` in `src/domain/service.rs`

- [ ] **Step 1: Failing test**

```rust
#[test]
fn autoscale_policy_validates_bounds() {
    let ok = AutoscalePolicy { min_replicas: 0, max_replicas: 3, target_cpu_pct: 80, target_mem_pct: Some(75), scale_down_cooldown_s: 300, idle_timeout_s: 600 };
    assert!(ok.validate().is_ok());
    // max < min
    let bad = AutoscalePolicy { min_replicas: 5, max_replicas: 2, ..ok.clone() };
    assert!(bad.validate().is_err());
    // idle_timeout < cooldown
    let bad2 = AutoscalePolicy { idle_timeout_s: 100, scale_down_cooldown_s: 300, ..ok.clone() };
    assert!(bad2.validate().is_err());
    // target pct out of range
    let bad3 = AutoscalePolicy { target_cpu_pct: 0, ..ok.clone() };
    assert!(bad3.validate().is_err());
}
```

- [ ] **Step 2:** Run red: `cargo test -p denia autoscale_policy_validates_bounds` → fails (type missing).
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

impl AutoscalePolicy {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.max_replicas < 1 || self.min_replicas > self.max_replicas {
            return Err(DomainError::InvalidAutoscale("replica bounds".into()));
        }
        let pct_ok = |p: u8| (1..=100).contains(&p);
        if !pct_ok(self.target_cpu_pct) || self.target_mem_pct.is_some_and(|p| !pct_ok(p)) {
            return Err(DomainError::InvalidAutoscale("target percent".into()));
        }
        if self.idle_timeout_s < self.scale_down_cooldown_s {
            return Err(DomainError::InvalidAutoscale("idle_timeout < cooldown".into()));
        }
        Ok(())
    }
}
```

Add `InvalidAutoscale(String)` variant to `DomainError`. Add field to `ServiceConfig`: `#[serde(default)] pub autoscale: Option<AutoscalePolicy>`, default `None` in `ServiceConfig::new`, and call `policy.validate()?` in `new` when `Some`.

- [ ] **Step 4:** Run green. Confirm existing `ServiceConfig` tests still pass (serde `default` keeps backward compat).
- [ ] **Step 5: Commit**

```bash
git add src/domain/service.rs
git commit -m "feat(domain): add AutoscalePolicy to ServiceConfig"
```

---

## Phase 2 — Pure scale math

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
    let d = desired_up(2, 90, 80, Some(50), Some(75));
    assert_eq!(d, 3);
}
#[test]
fn scale_down_ignores_memory() {
    // cpu 20/target80 => ceil(2*20/80)=1 ; mem high must NOT keep it up on the down path
    assert_eq!(desired_down(2, 20, 80), 1);
}
#[test]
fn clamp_respects_bounds_never_zero_from_loop() {
    assert_eq!(clamp_loop(0, 1, 5), 1); // loop floor is 1
    assert_eq!(clamp_loop(9, 1, 5), 5);
}
```

- [ ] **Step 2:** Run red.
- [ ] **Step 3: Implement** pure fns:

```rust
fn ceil_div(a: u64, b: u64) -> u64 { (a + b - 1) / b.max(1) }

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

/// Loop-side clamp: floor is max(min,1) because 1->0 is owned by the activator/idle path.
pub fn clamp_loop(desired: u32, min: u32, max: u32) -> u32 {
    desired.clamp(min.max(1), max)
}
```

- [ ] **Step 4:** Run green. **Step 5:** Commit `feat(autoscale): pure desired-replica math`.

### Task 3: cooldown / stabilization state machine

**Files:** Modify `src/autoscale/scaler.rs`; tests inline.

- [ ] **Step 1: Failing test** — scale-down only applies after CPU stayed below target for `cooldown_s`; scale-up applies immediately.

```rust
#[test]
fn cooldown_gates_scale_down_only() {
    let mut st = CooldownState::default();
    let t0 = 0u64;
    // below target at t0, want down — not yet allowed
    assert!(!st.scale_down_allowed(t0, 300));
    // still below at t0+299 — not allowed
    assert!(!st.scale_down_allowed(299, 300));
    // below for full window — allowed
    assert!(st.scale_down_allowed(300, 300));
    // a breach resets the timer
    st.note_above_target(310);
    assert!(!st.scale_down_allowed(320, 300));
}
```

- [ ] **Step 2-4:** Implement `CooldownState { below_since: Option<u64> }` with `note_above_target(now)` clearing `below_since`, and `scale_down_allowed(now, cooldown_s)` returning true once `now - below_since >= cooldown_s` (setting `below_since` on first below call). Run red→green.
- [ ] **Step 5:** Commit `feat(autoscale): scale-down cooldown state machine`.

---

## Phase 3 — ResourceLedger

### Task 4: ResourceLedger accounting

**Files:** Create `src/autoscale/ledger.rs`; export in `mod.rs`; tests inline.

- [ ] **Step 1: Failing tests** (spec Component 5: units millicores+bytes; Pending+Healthy+Draining all count; headroom subtracted).

```rust
#[test]
fn ledger_denies_when_exceeding_capacity_minus_headroom() {
    // host: 4000 millicores, 4 GiB ; headroom 1000 mc, 1 GiB => allocatable 3000 mc, 3 GiB
    let mut l = ResourceLedger::new(HostCapacity { cpu_millis: 4000, mem_bytes: 4<<30 },
                                    Headroom { cpu_millis: 1000, mem_bytes: 1<<30 });
    let lim = ResourceLimits { cpu_millis: 1000, memory_bytes: 1<<30 };
    assert!(l.try_reserve(lim).is_ok()); // 1000/1GiB committed
    assert!(l.try_reserve(lim).is_ok()); // 2000/2GiB
    assert!(l.try_reserve(lim).is_ok()); // 3000/3GiB == allocatable
    assert!(l.try_reserve(lim).is_err()); // would exceed
    l.release(lim);
    assert!(l.try_reserve(lim).is_ok());  // freed, fits again
}
```

- [ ] **Step 2-4:** Implement `HostCapacity`, `Headroom`, `ResourceLedger { committed_cpu, committed_mem, allocatable_cpu, allocatable_mem }`. `try_reserve(ResourceLimits) -> Result<(), LedgerError::InsufficientCapacity>` reserving (used for Pending); `release(ResourceLimits)`. Reservation precedes spawn so concurrent scale-ups can't double-spend. Run red→green.
- [ ] **Step 5:** Commit `feat(autoscale): resource ledger with headroom`.

### Task 5: HostCapacity from sysinfo

**Files:** Modify `src/autoscale/ledger.rs`; reuse host reading from `src/observability/node_metrics.rs` if it already exposes total CPU/RAM, else add a small reader. Tests: a unit test that `HostCapacity::detect()` returns non-zero cpu_millis (= num_cpus*1000) and non-zero mem_bytes on the test host.

- [ ] **Step 0 (verify):** read `src/observability/node_metrics.rs` (it exists, 7.6K) and confirm whether it already parses `/proc/meminfo` MemTotal and CPU count. Reuse those parsers; do NOT add a new dependency or a duplicate parser.
- [ ] Implement `HostCapacity::detect()` → `cpu_millis = <cpu count> * 1000`, `mem_bytes` from MemTotal (via the reused `node_metrics` parser). Commit `feat(autoscale): detect host capacity`.

---

## Phase 4 — Replica registry

### Task 6: Replica + ReplicaState + ReplicaRegistry

**Files:** Create `src/autoscale/registry.rs`; export; tests inline.

- [ ] **Step 1: Failing tests** — add/transition/remove; count Healthy; round-robin pick over Healthy only.

```rust
#[test]
fn registry_round_robin_over_healthy_only() {
    let mut reg = ReplicaRegistry::default();
    let svc = Uuid::now_v7();
    let r1 = reg.add(svc, /*deployment*/ Uuid::now_v7(), 0, "/run/denia/s-0.sock".into());
    let r2 = reg.add(svc, Uuid::now_v7(), 1, "/run/denia/s-1.sock".into());
    reg.set_state(r1, ReplicaState::Healthy);
    reg.set_state(r2, ReplicaState::Draining);
    // only r1 is selectable
    assert_eq!(reg.next_healthy(svc).map(|r| r.id), Some(r1));
    assert_eq!(reg.healthy_count(svc), 1);
}
```

- [ ] **Step 2-4:** Implement `Replica { id, service_id, deployment_id, index, socket_path, state, started_at }` (id via `Uuid::now_v7()`), `ReplicaState { Pending, Healthy, Draining, Stopped }`, `ReplicaRegistry` keyed by `service_id -> Vec<Replica>` with a per-service round-robin cursor; `next_healthy` skips non-Healthy. Run red→green.
- [ ] **Step 5:** Commit `feat(autoscale): in-memory replica registry`.

---

## Phase 5 — Persist desired_replicas

### Task 7: SQLite migration + repo + state facade

**Files:**
- Create: `src/repo/sqlite/autoscale.rs` (query fns, follow `services.rs` shape)
- Modify: `src/repo/sqlite/pool.rs` (add migration), `src/repo/sqlite/mod.rs` (mod line), `src/state.rs` (facade `get_desired_replicas` / `set_desired_replicas`)
- Test: `src/repo/sqlite/autoscale.rs` inline using an in-memory pool like other repo tests.

- [ ] **Step 1: Failing test** — round-trip desired count keyed by service_id; default 0/absent.

```rust
#[test]
fn desired_replicas_round_trip() {
    let pool = test_pool();
    let svc = Uuid::now_v7();
    assert_eq!(get_desired_q(&conn, svc).unwrap(), None);
    set_desired_q(&conn, svc, 3).unwrap();
    assert_eq!(get_desired_q(&conn, svc).unwrap(), Some(3));
}
```

- [ ] **Step 2:** Run red.
- [ ] **Step 3: Implement** migration `CREATE TABLE IF NOT EXISTS autoscale_desired (service_id BLOB PRIMARY KEY, desired_replicas INTEGER NOT NULL)` in `pool.rs` migration list (match existing migration registration style). Add `get_desired_q`/`set_desired_q` (upsert) in `autoscale.rs`. Add `SqliteStore::get_desired_replicas`/`set_desired_replicas` in `state.rs`.
- [ ] **Step 4:** Run green. **Step 5:** Commit `feat(repo): persist desired_replicas`.

---

## Phase 6 — Runtime replica identity + per-replica metrics

> **Critical prerequisite (verified against code):** the runtime is currently per-service. `LinuxRuntime::plan` (`src/runtime/linux.rs:101`) derives the host socket from a fixed guest path `GUEST_SERVICE_SOCKET` joined onto `rootfs_path`; `start` tracks children in a map **keyed by `service_name`** (`linux.rs:289`); `stop(service_name)` removes by service name (`linux.rs:427`); the `Runtime` trait (`src/runtime/runtime_trait.rs`) has **no enumeration** and **no per-replica stop**. Running multiple replicas of one service therefore requires a runtime identity rework FIRST. Tasks 10 (launch/drain) and 17 (boot reconcile) depend on this.

### Task 8: replica-scoped runtime identity, stop, and enumeration

**Files:**
- Modify: `src/domain/deployment.rs` — add `replica_index: u32` to `RuntimeStartRequest` (`#[serde(default)]`).
- Modify: `src/runtime/runtime_trait.rs` — change `stop` to a replica-scoped key and add enumeration:
  - `async fn stop(&self, instance: &RuntimeInstanceId) -> Result<(), RuntimeError>;`
  - `async fn list_running(&self) -> Result<Vec<RuntimeStatus>, RuntimeError>;` (default `Ok(vec![])` for back-compat).
  - Introduce `RuntimeInstanceId { service_name: String, replica_index: u32 }` (or reuse `(service_id, replica_index)`) — pick one and use it for the children-map key.
- Modify: `src/runtime/linux.rs` — make `rootfs_path` (and thus the in-rootfs socket), the cgroup path, the children-map key, and `stop`/`list_running` all keyed by `replica_index`. Give each replica its own rootfs leaf (`…/{service_id}/{deployment_id}/{replica_index}`) so the in-rootfs socket and log paths become unique. The cgroup path is derived **independently** from `cgroup_root/service_id/deployment_id`, so thread `replica_index` into the cgroup path **separately** (→ `cgroup_root/service_id/deployment_id/replica_index`) — it does not follow from the rootfs change. The host-side socket path is returned in `RuntimeStatus.socket_path` — consumers (bridge/registry) read it from there, they don't recompute it.
- Modify: `src/runtime/fake.rs` — mirror the new key + `list_running`.
- Update all existing `stop(...)`/start call sites (deploy coordinator, scheduler/job path, tests) to pass the new key with `replica_index: 0`.

- [ ] **Step 1: Failing test** in `src/runtime/linux.rs` (fixtures live near `linux.rs:529`): `LinuxRuntime::plan` for the same service+deployment with `replica_index: 0` vs `1` yields distinct `rootfs_path`, host `socket_path`, and cgroup path.
- [ ] **Step 2:** Run red.
- [ ] **Step 3:** Thread `replica_index` through `plan`; key children map by `RuntimeInstanceId`; implement `list_running` (enumerate the children map → `RuntimeStatus`); update `stop` signature + body. Update `fake.rs` and call sites.
- [ ] **Step 4:** Run green; `cargo build` to catch all call-site breaks; fix them.
- [ ] **Step 5:** Commit `refactor(runtime): replica-scoped identity, stop, and enumeration`.

### Task 9: ServiceUsage aggregation + sampler with prev-tick state

**Files:** Create `src/autoscale/usage.rs`; modify `src/observability/metrics.rs`. Tests inline.

> **Verify first:** `CgroupMetricsReader` (`src/observability/metrics.rs`) currently exposes `read_service(service_name, deployment_id)` and `read_by_id(service_name, service_id, deployment_id)` returning `MetricSnapshot { cpu_usage_usec, memory_current_bytes }`. CPU is a **cumulative** counter, so a percentage needs a delta between ticks. The prev-tick state lives in a `UsageSampler` struct in `usage.rs` (owned by the controller), NOT in the stateless reader.

- [ ] **Step 1: Failing tests** — two parts, both pure:

```rust
// pure CPU% from a delta
#[test]
fn cpu_pct_from_delta() {
    // 800ms CPU over a 1s window at a 1000m limit => 80%
    assert_eq!(cpu_pct(/*prev*/0, /*cur*/800_000, /*window_us*/1_000_000, /*cpu_millis*/1000), 80);
}
// aggregation across replicas
#[test]
fn usage_aggregates_avg_cpu_and_mem() {
    // A: 80% cpu, 50% mem ; B: 40% cpu, 75% mem => avg_cpu 60, avg_mem 62, max_mem 75
    let u = ServiceUsage::aggregate(&[(80, 50), (40, 75)]);
    assert_eq!((u.avg_cpu_pct, u.max_mem_pct), (60, 75));
}
```

- [ ] **Step 2:** Run red.
- [ ] **Step 3:** Implement pure `cpu_pct(prev_usec, cur_usec, window_us, cpu_millis) -> u32`; `ServiceUsage { avg_cpu_pct, avg_mem_pct, max_mem_pct, replica_count }` + `aggregate(&[(cpu_pct, mem_pct)])`; `UsageSampler { prev: HashMap<Uuid /*replica_id*/, (u64 /*cpu_usec*/, Instant)> }` with `sample(replicas, reader, limits) -> ServiceUsage` that reads each replica's cgroup, computes the per-replica `cpu_pct` against stored prev, and aggregates. Add `read_replica(service_name, service_id, deployment_id, replica_index)` to the reader matching the Task 8 cgroup path.
- [ ] **Step 4:** Run green. **Step 5:** Commit `feat(autoscale): metric aggregation + usage sampler`.

---

## Phase 7 — Launch / drain primitives

### Task 10: replica launch + drain via runtime

**Files:** Modify `src/autoscale/controller.rs` (new) or a `src/autoscale/lifecycle.rs`; reuse `RuntimeStartRequest` and the existing runtime trait (`src/runtime/runtime_trait.rs`) + `workload_launcher.rs`. Use the bridge/registry. Test with `FakeRuntime` + `FakeBridgeManager`.

- [ ] **Step 1: Failing test** — `launch_replica(svc, deployment, index)` reserves in the ledger, builds a `RuntimeStartRequest` with `replica_index`, calls `runtime.start`, takes the returned `RuntimeStatus.socket_path`, registers a `Pending` replica + adds its socket to the bridge pool; on health pass → `Healthy`. `drain_replica(id)` sets `Draining` (bridge skips it), waits (mocked) for in-flight=0 or grace, calls `runtime.stop(RuntimeInstanceId)` (Task 8), releases ledger, removes from bridge + registry.
- [ ] **Step 2-4:** Implement using the fakes (`FakeRuntime` now has `list_running` + replica-scoped stop from Task 8); keep the real health-check call behind the existing `HealthCheck`. Run red→green.
- [ ] **Step 5:** Commit `feat(autoscale): replica launch and drain primitives`.

---

## Phase 8 — Bridge fan-out

### Task 11: bridge holds a replica pool + round-robin + last_activity

**Files:** Modify `src/ingress/bridge.rs`. The supervisor must hold, per service, a set of `(replica_id, socket_path, state)` and a `last_activity: Instant`. `serve_one` picks the next Healthy replica's socket. `tee_proxy` updates `last_activity`.

- [ ] **Step 1: Failing test** — register two replica sockets for a service; two sequential connections hit different sockets (round-robin); a `Draining` socket is skipped; `last_activity` advances after a proxied request. Use Unix socket fixtures like existing bridge tests.
- [ ] **Step 2-4:** Extend `LoopbackBridgeSupervisor`/`BridgeTask` to own a pool keyed by service rather than a single `socket_path`. Add `add_replica(service, replica_id, socket_path)`, `set_replica_state`, `remove_replica`, `last_activity(service)`. Keep one TCP listener per service (Traefik config unchanged). Run red→green.
- [ ] **Step 5:** Commit `feat(ingress): bridge replica pool with round-robin`.

### Task 12: single-flight cold-start activator

**Files:** Modify `src/ingress/bridge.rs`. On a connection when `healthy_count(service)==0`, trigger a single launch via a callback/channel to the controller, hold the connection until a Healthy replica exists (bounded timeout), then proxy; on failure/timeout return HTTP 503 to the connection and release the latch.

- [ ] **Step 1: Failing test** — concurrent N connections at zero trigger exactly one launch (count the callback invocations); when the launch callback reports failure, all held connections receive a 503 and the latch resets so a later connection triggers a fresh launch.
- [ ] **Step 2-4:** Implement a per-service single-flight latch (`tokio::sync::Mutex<Option<Shared<...>>>` or a `Notify` + in-flight flag). The activator calls an injected `ActivationHook` (trait) so tests use a fake and production wires the controller. Write a minimal 503 response to the TCP stream on failure. Run red→green.
- [ ] **Step 5:** Commit `feat(ingress): single-flight cold-start activator`.

---

## Phase 9 — Controller loop

### Task 13: autoscale controller tick

**Files:** Modify `src/autoscale/controller.rs`. A `tick(now)` that, per service with a policy: reads `ServiceUsage`, computes up/down desired (Task 2), applies cooldown (Task 3), clamps (loop floor), reconciles via ledger + launch/drain (Task 10), persists `desired_replicas` (Task 7).

> **Event sink decision:** `tick(now)` **returns `Vec<AutoscaleEvent>`** (e.g. `ScaledUp{service,from,to}`, `ScaledDown{..}`, `ScaleUpDenied{service,reason}`, `ScaledToZero{service}`). Tests assert on the returned vec — no global event store needed. The spawn loop (Task 18) also logs each event via `tracing`. This keeps the load-bearing assertions concrete and testable.

- [ ] **Step 1: Failing test** (fakes for runtime/bridge/metrics/store):
  - high CPU → tick returns `ScaledUp`, registry gains a replica (Pending→Healthy), `desired_replicas` persisted +1;
  - sustained low CPU past cooldown → tick returns `ScaledDown`, drains one;
  - host at capacity → tick returns `ScaleUpDenied{reason: "insufficient_capacity"}`, no launch, count unchanged.
- [ ] **Step 2-4:** Implement `AutoscaleEvent` enum and `Controller { registry, ledger, store, runtime, bridge, sampler, cooldown_states }` with `tick(now) -> Vec<AutoscaleEvent>`. Run red→green.
- [ ] **Step 5:** Commit `feat(autoscale): controller tick`.

### Task 14: idle → scale-to-zero

**Files:** Modify `src/autoscale/controller.rs`.

- [ ] **Step 1: Failing test** — service with `min_replicas==0`, `now - last_activity > idle_timeout_s`, low metrics → tick drains ALL replicas to zero and persists desired=0. With `min_replicas>=1`, never zeros.
- [ ] **Step 2-4:** Add the idle branch reading `bridge.last_activity(service)`. Run red→green.
- [ ] **Step 5:** Commit `feat(autoscale): idle scale-to-zero`.

### Task 15: 0→1 activation entry point

**Files:** Modify `src/autoscale/controller.rs` to implement the `ActivationHook` from Task 12: launch one replica (through the ledger, respecting `max_replicas`, bypassing cooldown), return Ok once Healthy or Err on timeout.

- [ ] **Step 1: Failing test** — activation hook launches exactly one replica and returns Ok when it becomes Healthy; returns Err when the (fake) runtime never reports Healthy before timeout; respects `max_replicas==0`? (N/A: max>=1) and ledger capacity (denied → Err).
- [ ] **Step 2-5:** Implement, run red→green, commit `feat(autoscale): activation hook for scale-from-zero`.

---

## Phase 10 — Rollout + boot reconcile

### Task 16: rolling replace on redeploy

**Files:** Modify `src/autoscale/controller.rs` (+ a hook from the deploy path `src/deploy/coordinator.rs` to notify the controller of a new active deployment).

- [ ] **Step 1: Failing test** — service at 3 replicas of deployment D1; new active D2 → controller replaces one-at-a-time drain-then-launch (maxUnavailable=1, no surge); during rollout autoscale scale events are deferred; for a single-replica service it uses launch-then-drain to avoid a zero gap.
- [ ] **Step 2-4:** Implement a `rollout` state per service; `tick` advances rollout before evaluating scaling. Run red→green.
- [ ] **Step 5:** Commit `feat(autoscale): rolling replace on redeploy`.

### Task 17: boot reconcile + orphan adoption

**Files:** Modify `src/autoscale/controller.rs`; add `reconcile_boot()`.

- [ ] **Step 1: Failing test** (fakes) — given persisted `desired_replicas=2` and leftover runtime workloads/cgroups (fake lists 1 matching D-active + 1 stale), `reconcile_boot` adopts the matching one into the registry, kills+cleans the stale one, launches to reach 2. Stale socket files are cleaned (the launch path already removes a pre-existing socket — assert no error on re-bind).
- [ ] **Step 2-4:** Implement using `runtime.list_running()` (added in Task 8): adopt entries whose deployment matches the active one into the registry, `stop` the rest (replica-scoped key) and clean their cgroup/socket, then top up to desired (floor `max(min,1)` unless policy allows zero and the service was idle). Run red→green.
- [ ] **Step 5:** Commit `feat(autoscale): boot reconcile and orphan adoption`.

---

## Phase 11 — Wiring + API

### Task 18: construct + spawn controller; env config

**Files:** Modify `src/app.rs` (build `ReplicaRegistry`, `ResourceLedger::new(HostCapacity::detect(), Headroom::from_env())`, `Controller`; pass registry handle to the bridge supervisor; store controller in `AppState`), `src/main.rs` (spawn `autoscale::run_until_shutdown(controller, shutdown_rx)` mirroring `scheduler::run_until_shutdown`), `src/config.rs` (env: `DENIA_AUTOSCALE_INTERVAL_S` default 15, `DENIA_AUTOSCALE_HEADROOM_CPU_MILLIS`, `DENIA_AUTOSCALE_HEADROOM_MEM_BYTES`).

- [ ] **Step 1:** Add `run_until_shutdown(controller: Arc<Controller>, shutdown)` with a `tokio::time::interval(interval_s)` loop calling `controller.tick(Utc::now())` (pattern: `src/scheduler.rs:65`).
- [ ] **Step 2:** Wire into `AppState` builder and `main.rs` startup; call `controller.reconcile_boot()` once before the loop. Connect the bridge `ActivationHook` to the controller.
- [ ] **Step 3:** Add env parsing in `config.rs` with documented defaults.
- [ ] **Step 4:** Build + run the existing app once to confirm startup is clean (`cargo run` boots, `/healthz` ok). If you cannot run a privileged host, state that and rely on `cargo build` + unit tests.
- [ ] **Step 5:** Commit `feat(app): wire and spawn autoscale controller`.

### Task 19: observability surface

**Files:** Modify `src/api/observability.rs` (`WorkloadView` at line 23 — add `replica_count` and optional per-replica metrics), and the handler that builds it (pull from `ReplicaRegistry`).

- [ ] **Step 1: Failing test** — the observability handler returns `replica_count` for a service with N registered replicas (use the existing API test harness).
- [ ] **Step 2-4:** Add the field + populate from the registry. Keep response backward compatible (additive). Run red→green.
- [ ] **Step 5:** Commit `feat(api): expose replica count in observability`.

---

## Phase 12 — End-to-end

### Task 20: integration test — full lifecycle

**Files:** Create `tests/autoscale_lifecycle.rs` using `FakeRuntime` + `FakeBridgeManager` + in-memory store.

- [ ] **Step 1: Failing test** driving the controller through:
  1. policy `min=0,max=3`; start idle → zero replicas;
  2. simulate a connection at zero → activator launches 1, request proxied;
  3. drive high CPU metrics → scales to 3 (capped at max);
  4. capacity denial at host limit → stays, event emitted;
  5. drop CPU past cooldown → scales down to 1;
  6. idle past `idle_timeout` → scales to 0;
  7. redeploy mid-load → rolling replace one-at-a-time.
- [ ] **Step 2-4:** Implement; iterate until green.
- [ ] **Step 5:** Commit `test(autoscale): full lifecycle integration`.

### Task 21: final gate

- [ ] `cargo fmt --all`
- [ ] `cargo build`
- [ ] `cargo test`
- [ ] `cargo clippy --all-targets --all-features`
- [ ] On a root host only: `DENIA_RUN_PRIVILEGED_TESTS=1 cargo test --test linux_runtime_privileged -- --ignored`
- [ ] Commit any fmt/clippy fixes: `chore(autoscale): fmt and clippy`.
- [ ] Report exact commands run + results.

---

## Notes for the implementer

- The Traefik file config (`src/ingress/traefik.rs`) does **not** change — one server (one stable bridge port) per service throughout. Do not add per-replica Traefik servers.
- Memory never drives scale-down (Component 4). If you find yourself feeding `mem_pct` into the down path, stop.
- Reserve ledger capacity for a replica **before** spawning it (Pending counts), and free it only after the workload is killed (post-drain).
- `list_running()` and replica-scoped `stop` are added in Task 8 (the trait currently has neither). Everything in Phases 7+ assumes Task 8 landed first.
- The bridge `ActivationHook` (Task 12) is a trait so the bridge has no compile-time dependency on the controller; the controller implements it (Task 15) and is injected during wiring (Task 18).
