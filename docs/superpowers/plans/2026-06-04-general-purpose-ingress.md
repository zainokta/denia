# General-Purpose Ingress Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add the safe foundation for TCP/UDP/WebSocket/gRPC-aware ingress without changing the high-risk runtime launch path in the first slice.

**Architecture:** Keep Pingora HTTP/WebSocket routing unchanged. Add endpoint domain types and L4 port allocation/config as passive foundations, then defer runtime socket/listener work to a later slice because `RuntimeStartRequest` impact is CRITICAL.

**Tech Stack:** Rust 2024, serde, tokio, axum, Pingora, SQLite, GitNexus impact analysis, Superpowers worktrees.

---

### Task 1: Isolated Worktree and Baseline

**Files:**
- No tracked file changes.

- [x] **Step 1: Create isolated worktree**

Run:

```bash
git worktree add .worktrees/feat-general-purpose-ingress -b feat/general-purpose-ingress
```

Expected: worktree created on `feat/general-purpose-ingress`.

- [x] **Step 2: Build web assets required by Rust embed**

Run:

```bash
cd web
pnpm install --offline
pnpm build
```

Expected: `web/dist/client` exists.

- [x] **Step 3: Baseline build**

Run:

```bash
cargo build
```

Expected: PASS.

- [x] **Step 4: Baseline tests**

Run:

```bash
cargo test
```

Expected: current baseline has one unrelated scheduler failure:
`scheduler::tests::tick_primes_then_skips_when_active_and_records_skipped`.

### Task 2: ADR and Design Spec

**Files:**
- Create: `docs/adr/036-general-purpose-protocol-ingress.md`
- Modify: `docs/adr/README.md`
- Create: `docs/superpowers/specs/2026-06-04-general-purpose-ingress-design.md`

- [x] **Step 1: Write ADR**

Document TCP/UDP public port allocation, WebSocket over HTTP/1.1, native gRPC
deferral under ADR-032, and always-on TCP/UDP in v1.

- [x] **Step 2: Update ADR index**

Add ADR-036 to `docs/adr/README.md`.

- [x] **Step 3: Write design spec**

Record the implementation slice and explicitly defer runtime launch changes.

### Task 3: Endpoint Domain Foundation

**Files:**
- Modify: `src/domain/error.rs`
- Modify: `src/domain/service.rs`

- [x] **Step 1: Add endpoint validation tests**

Tests cover:

```rust
effective_endpoints_maps_legacy_port_to_default_http_endpoint
validate_accepts_tcp_and_udp_endpoints_without_public_ports
validate_rejects_invalid_endpoint_shape
```

- [x] **Step 2: Implement endpoint types**

Add:

```rust
pub enum ServiceEndpointProtocol { Http, Tcp, Udp }
pub struct ServiceEndpoint {
    pub name: String,
    pub protocol: ServiceEndpointProtocol,
    pub internal_port: u16,
    pub public_port: Option<u16>,
}
```

- [x] **Step 3: Preserve legacy behavior**

`ServiceConfig::effective_endpoints()` returns `http:internal_port` when
`endpoints` is empty.

- [x] **Step 4: Verify**

Run:

```bash
cargo test domain::service::tests:: --lib
```

Expected: PASS.

### Task 4: L4 Port Range and Allocator

**Files:**
- Create: `src/ingress/l4.rs`
- Modify: `src/ingress/mod.rs`

- [x] **Step 1: Add allocator tests**

Tests cover inclusive parsing, malformed ranges, first-free allocation,
exhaustion, and release/reuse.

- [x] **Step 2: Implement `PortRange` and `PortAllocator`**

Use a small deterministic in-memory allocator over a `BTreeSet<u16>`.

- [x] **Step 3: Verify**

Run:

```bash
cargo test ingress::l4::tests --lib
```

Expected: PASS.

### Task 5: TCP/UDP Port Range Config

**Files:**
- Modify: `src/config.rs`

- [x] **Step 1: Add config tests**

Tests cover default generated config values, env/file precedence, and invalid
range rejection.

- [x] **Step 2: Implement config fields**

Add `tcp_port_range` and `udp_port_range` to `AppConfig` and `FileConfig`.
Defaults:

```text
DENIA_TCP_PORT_RANGE=20000-29999
DENIA_UDP_PORT_RANGE=30000-39999
```

- [x] **Step 3: Verify**

Run:

```bash
cargo test config:: --lib
```

Expected: PASS.

### Task 6: Next Runtime Slice

**Files:**
- To be planned separately after this foundation lands.

- [ ] **Step 1: Run fresh impact analysis**

Required targets before editing:

```text
RuntimeStartRequest
LinuxRuntime::plan
DeploymentCoordinator::deploy
launch_replica
IngressState
```

- [ ] **Step 2: Add persistence and listeners**

Add SQLite endpoint rows, allocated public ports, TCP listener tasks, UDP
datagram transport, and WebSocket E2E coverage.

- [ ] **Step 3: Verify privileged/runtime paths**

Run normal Rust tests plus gated privileged tests when root/runtime isolation is
available.
