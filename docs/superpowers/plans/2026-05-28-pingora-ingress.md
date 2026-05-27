# Pingora In-Process Ingress Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace Denia's supervised Traefik process and loopback-bridge transport with an in-process Pingora L7 proxy that binds `:80`/`:443`, issues its own TLS certs via ACME (instant-acme, HTTP-01), and proxies directly to workload Unix sockets.

**Architecture:** A Pingora `Server` runs on a dedicated OS thread inside the Denia process, sharing `Arc<IngressState>` with the control plane. `IngressState` absorbs the bridge's control brain (replica pools, health, scale-from-zero activation, idle tracking, access log) and adds `ArcSwap<RouteTable>` / `ArcSwap<CertStore>`. Cert *selection* is sync in a `TlsAccept` callback; cert *issuance* is async out-of-band. Challenge paths (`acme-challenge`, `denia-challenge`) on `:80` proxy to axum.

**Tech Stack:** Rust 2024, axum, tokio, `pingora` + `pingora-proxy`, `instant-acme`, `arc-swap`, SQLite. Frontend: TanStack Query + Effect (`web/`).

**Spec:** `docs/superpowers/specs/2026-05-28-pingora-ingress-design.md`

---

## Phase 0 — Gating Spikes (BLOCKING; no plan beyond this until resolved)

These three throwaway spikes decide whether the design holds. Do them first, in a scratch branch/binary. Record findings in `docs/superpowers/specs/2026-05-28-pingora-ingress-spike-notes.md`. **If Spike 0.1 fails, stop and re-brainstorm the embedding model.**

### Task 0.1: Signal-handling spike

**Question:** Can a Pingora `Server` run without trapping SIGTERM/SIGINT/SIGQUIT, so Denia's existing shutdown path stays authoritative?

- [ ] **Step 1:** Add `pingora`, `pingora-proxy` to a scratch `examples/pingora_spike.rs` (or a `cargo new` outside the tree). Pin the version you intend to ship.
- [ ] **Step 2:** Build a minimal `Server` + trivial `ProxyHttp` bound to `127.0.0.1:18080`, started from a `std::thread::spawn`, while the main thread installs its own `tokio::signal::ctrl_c` handler.
- [ ] **Step 3:** Send SIGTERM; observe whether Pingora intercepts it or the main handler fires. Try `Server::new` config knobs / `RunArgs` for disabling daemon + signal handling.
- [ ] **Step 4:** Record: can signals be left to Denia? If not, what does Pingora force? Decision: dedicated-thread model OK / needs redesign.

### Task 0.2: UDS-upstream spike

**Question:** Can `HttpPeer`/the connector dial a Unix-domain-socket upstream?

- [ ] **Step 1:** In the scratch proxy, bind a `tokio::net::UnixListener` echoing HTTP, then try to construct an `HttpPeer` (or `Peer`) targeting that UDS path in `upstream_peer`.
- [ ] **Step 2:** Issue a request through the proxy; confirm it reaches the UDS backend.
- [ ] **Step 3:** Record: UDS upstream supported? If **yes** → bridge transport fully deleted, `bridge_start_port` removed, `RouteView.bridge_port` dropped. If **no** → keep a thin per-replica UDS→loopback-TCP shim; `bridge_start_port` and `bridge_port` survive.

### Task 0.3: Dynamic per-SNI cert-callback spike

**Question:** Exact API for serving a cert chosen at handshake from in-memory state, and behavior when no cert exists for the SNI.

- [ ] **Step 1:** Build a `:443` listener via `TlsSettings` + the `TlsAccept` (or equivalent) callback on the boringssl backend.
- [ ] **Step 2:** Serve a self-signed cert from an `ArcSwap` keyed by SNI; confirm swap takes effect without restart.
- [ ] **Step 3:** Request an SNI with no cert; confirm the callback can decline and the handshake fails cleanly (no panic, no default cert leak).
- [ ] **Step 4:** Record the exact trait/method signatures to use in `tls.rs`.

- [ ] **Commit** the spike notes.

```bash
git add docs/superpowers/specs/2026-05-28-pingora-ingress-spike-notes.md
git commit -m "docs(ingress): record Pingora pre-implementation spike findings"
```

> **Decision gate — RESOLVED 2026-05-28 (GO).** Spiked on **pingora 0.8.0 (boringssl)**. Outcomes locked:
> - **Signal: GREEN** — inject `RunArgs.shutdown_signal: Box<dyn ShutdownSignalWatch>`, call `Server::run(..)` (NOT `run_forever()`), run on a dedicated `std::thread`, no daemon/upgrade mode.
> - **UDS: YES** — use `HttpPeer::new_uds(path, tls, sni)`. **`bridge_port`, `BridgeAllocator`, `bridge_start_port` are DELETED** (all "if Spike 0.2 = UDS" branches below are now active).
> - **Cert callback** — `pingora::listeners::TlsAccept::certificate_callback(&self, ssl: &mut TlsRef)` + `ext::ssl_use_certificate`/`ssl_use_private_key`, via `TlsSettings::with_callbacks`. Decline = no cert installed → clean `TLSHandshakeFailure`.
> - **Cargo: pin `pingora`/`pingora-proxy = "0.8"` and ENABLE the `boringssl` feature** (no TLS in default features; `rustls` path is a stub).

---

## Phase 1 — Pingora server skeleton

### Task 1.1: Add dependencies

**Files:** Modify `Cargo.toml`

- [ ] **Step 1:** Add `pingora`, `pingora-proxy` (pinned to the spiked version), `instant-acme`, and `arc-swap` (if not already present). Keep the boringssl default TLS backend unless Spike 0.3 dictates `rustls`.
- [ ] **Step 2:** Run `cargo build`. Expected: compiles (no usage yet).
- [ ] **Step 3:** Commit.

```bash
git add Cargo.toml Cargo.lock
git commit -m "feat(ingress): add pingora, instant-acme deps"
```

### Task 1.2: `src/ingress/pingora/` module skeleton + static server

**Files:**
- Create: `src/ingress/pingora/mod.rs`, `src/ingress/pingora/server.rs`
- Modify: `src/ingress/mod.rs`

- [ ] **Step 1:** Create `mod.rs` re-exporting `server`. Wire `pub mod pingora;` in `src/ingress/mod.rs`.
- [ ] **Step 2:** In `server.rs`, write `build_server(state: Arc<IngressState>, cfg: &IngressServerConfig) -> pingora::server::Server` that adds a TCP `:80` service and (later) a TLS `:443` service, using a placeholder `DeniaProxy`. Gate behind `#[allow(dead_code)]` until wired.
- [ ] **Step 3:** `cargo build`. Expected: compiles.
- [ ] **Step 4:** Commit.

---

## Phase 2 — `IngressState` + route table + migrated bridge brain

### Task 2.1: Move `RouteSpec` and define `RouteTable`

**Files:**
- Create: `src/ingress/pingora/state.rs`
- Modify: `src/ingress/traefik.rs` (will be deleted in Phase 5; for now re-export `RouteSpec` from `state.rs`)
- Test: inline `#[cfg(test)]` in `state.rs`

- [ ] **Step 1: Write failing test** — host lookup, exact + control domain match.

```rust
// Real RouteSpec fields (src/ingress/traefik.rs): route_key, service_name, domains, bridge_port, tls.
#[test]
fn route_table_resolves_host_to_service() {
    let mut t = RouteTable::default();
    t.upsert(RouteSpec {
        route_key: "svc-1".into(),
        service_name: "api".into(),
        domains: vec!["api.example.com".into()],
        bridge_port: 0, // field dropped entirely if Spike 0.2 = UDS
        tls: true,
    });
    assert_eq!(t.resolve("api.example.com").map(|r| r.service_name.as_str()), Some("api"));
    assert!(t.resolve("nope.example.com").is_none());
}
```

- [ ] **Step 2:** Run `cargo test route_table_resolves_host_to_service`. Expected: FAIL (no `RouteTable`).
- [ ] **Step 3:** Implement `RouteSpec` (moved, exact current fields) + `RouteTable { by_host: BTreeMap<String, RouteSpec> }` with `upsert`, `remove`, `resolve`. Drop `bridge_port` only if Spike 0.2 = UDS (then update every reference — see Tasks 5.6, 6.2, 6.3, 6.4).

> **Testability mandate:** put resolution/redirect/cert-selection logic in **free functions or `IngressState` methods** (e.g. `IngressState::resolve_host`, `classify_request`, `select_cert`). The `ProxyHttp`/`TlsAccept` trait methods must be thin wrappers, so Phase 3/4 "failing test first" steps can test the logic without a live Pingora `Session`.
- [ ] **Step 4:** Run test. Expected: PASS.
- [ ] **Step 5:** Commit.

### Task 2.2: Migrate replica pool + health + activation + activity

**Files:**
- Modify: `src/ingress/pingora/state.rs`
- Reference: `src/ingress/bridge.rs` (`ServicePool`, `ReplicaEndpoint`, `ActivationHook`, `healthy_count`, `next_socket`, `last_activity`, `set_replica_healthy`, `add_replica`, `remove_replica`, `AccessLogStore`)

- [ ] **Step 1: Write failing tests** — round-robin over healthy replicas; zero-healthy fires activation; `last_activity` bumps.

```rust
#[tokio::test]
async fn next_socket_round_robins_healthy_replicas() { /* add 2 replicas, mark healthy, assert alternation */ }

#[tokio::test]
async fn zero_replicas_invokes_activation_hook() { /* fake hook records call */ }
```

- [ ] **Step 2:** Run tests. Expected: FAIL.
- [ ] **Step 3:** Port `ServicePool`/`ReplicaEndpoint`/`ActivationHook` from `bridge.rs` into `state.rs` under `IngressState`, preserving method signatures (`add_replica`, `remove_replica`, `set_replica_healthy`, `healthy_count`, `next_socket`, `last_activity`, `set_last_activity`, `set_activator`). Embed `AccessLogStore`.
- [ ] **Step 4:** Run tests. Expected: PASS.
- [ ] **Step 5:** Commit.

### Task 2.3: Add `CertStore`, `ChallengeMap` placeholders + `IngressState` assembly

**Files:** Modify `src/ingress/pingora/state.rs`

- [ ] **Step 1:** Define `CertStore` (SNI → parsed cert/key) behind `ArcSwap`, and the `IngressState` struct holding `ArcSwap<RouteTable>`, the pool map, `ArcSwap<CertStore>`, `AccessLogStore`. (Challenge state lives in axum, not here — see Phase 4.)
- [ ] **Step 2:** `cargo build`. Commit.

---

## Phase 3 — `DeniaProxy` (`ProxyHttp`)

### Task 3.1: Route resolution + unknown-host 404

**Files:** Create `src/ingress/pingora/proxy.rs`; Test: inline.

- [ ] **Step 1: Write failing test** — `upstream_peer` resolves Host → replica UDS peer; unknown host → 404. (Use Pingora's test harness or a thin unit around the resolution helper extracted from the trait method so it's unit-testable without a live socket.)
- [ ] **Step 2:** Run. Expected: FAIL.
- [ ] **Step 3:** Implement `DeniaProxy { state: Arc<IngressState> }` with `CTX` carrying resolved service + replica. `upstream_peer`: resolve host → pick replica via `next_socket` → return `HttpPeer` to the UDS (or loopback-TCP shim if Spike 0.2 = no UDS). Zero healthy → fire activation, await, else 503.
- [ ] **Step 4:** Run. Expected: PASS. Commit.

### Task 3.2: `:80` challenge interception + HTTP→HTTPS redirect

**Files:** Modify `src/ingress/pingora/proxy.rs`; Test: inline.

- [ ] **Step 1: Write failing tests** — request to `/.well-known/acme-challenge/x` and `/.well-known/denia-challenge/x` route to the control backend (ctx flag set), unconditionally, before host 404; a `tls_enabled` host on `:80` returns 308 to `https://`.
- [ ] **Step 2:** Run. Expected: FAIL.
- [ ] **Step 3:** Implement `request_filter` (port-80 service): challenge-path check first → set ctx `to_control_backend`; else host lookup → if `tls`, write 308 redirect and return `Ok(true)`; else fall through. In `upstream_peer`, honor `to_control_backend` by returning an `HttpPeer` to `bind_addr`.
- [ ] **Step 4:** Run. Expected: PASS. Commit.

### Task 3.3: Access-log `logging()` phase

**Files:** Modify `src/ingress/pingora/proxy.rs`; Test: inline.

- [ ] **Step 1: Write failing test** — after a request, an `AccessEntry` with `status`/`bytes`/`duration_ms`/host/path is recorded in `AccessLogStore`.
- [ ] **Step 2-4:** Implement `logging()` mapping Pingora `Session` → `AccessEntry`; test PASS; commit. (Preserves ADR-009.)

---

## Phase 4 — ACME (instant-acme) + TLS

### Task 4.1: ACME account + order driver

**Files:** Create `src/ingress/pingora/acme.rs`; Test: inline + integration against pebble/staging.

- [ ] **Step 1: Write failing test** — given a mock/staging directory, `AcmeDriver::issue("example.test")` drives an order to `valid` and returns a cert chain + key. (Use LE staging or pebble via `DENIA_ACME_DIRECTORY_URL`; gate the network test behind an env flag like the privileged tests.)
- [ ] **Step 2:** Run. Expected: FAIL.
- [ ] **Step 3:** Implement `AcmeDriver` with `instant-acme`: load/create account key at `<tls_dir>/account.key` (0600), create order, expose `token → key_authorization` to the challenge handler, poll, finalize, fetch chain.
- [ ] **Step 4:** Run (flagged). Expected: PASS. Commit.

### Task 4.2: axum challenge handlers on the control plane

**Files:**
- Modify: `src/api/...` (wherever `denia-challenge` is currently served) + add `acme-challenge` handler
- Reference: `src/verification/http.rs`, `build_router`
- Test: existing domain-verification tests + new acme-challenge test.

- [ ] **Step 1: Write failing test** — `GET /.well-known/acme-challenge/<token>` returns the registered key authorization; unknown token → 404.
- [ ] **Step 2:** Run. Expected: FAIL.
- [ ] **Step 3:** Add the acme-challenge route backed by a shared challenge map owned by `AcmeDriver`. Confirm `denia-challenge` route still served (unchanged).
- [ ] **Step 4:** Run. Expected: PASS. Commit.

> **Verifier path is unchanged:** `src/verification/http.rs` still fetches
> `http://<host>/.well-known/denia-challenge/<token>` against the public host.
> Previously Traefik routed that to Denia; now Pingora's `:80` `request_filter`
> (Task 3.2) intercepts the path and proxies to the control backend, so external
> verification still reaches axum. No change to the verifier code itself — the
> regression risk is purely in Pingora forwarding the path, which Task 3.2 covers.

### Task 4.3: Cert persistence + boot load + `CertStore` swap

**Files:** Modify `src/ingress/pingora/acme.rs`, `state.rs`; Test: inline.

- [ ] **Step 1: Write failing tests** — `persist_cert` writes `<tls_dir>/<domain>/{fullchain.pem,key.pem}` at mode `0600` atomically (temp+rename); `load_certs_from_disk` populates `CertStore`; swap is observable.
- [ ] **Step 2-4:** Implement; tests PASS; commit.

### Task 4.4: `TlsAccept` per-SNI callback

**Files:** Create `src/ingress/pingora/tls.rs`; Test: inline (using the API confirmed in Spike 0.3).

- [ ] **Step 1: Write failing test** — callback returns the cert for a known SNI from `CertStore`; declines for unknown SNI.
- [ ] **Step 2-4:** Implement using the exact signatures from Spike 0.3; wire into `server.rs` `:443` listener; tests PASS; commit.

### Task 4.5: Renewal task + email validation

**Files:** Modify `src/ingress/pingora/acme.rs`, `src/config.rs`.

- [ ] **Step 1: Write failing test** — a cert within the renewal window is selected for re-order; `ConfigError::AcmeEmailRequired` raised when a TLS service exists but `DENIA_ACME_EMAIL` is unset.
- [ ] **Step 2-4:** Implement renewal scan loop + email gate (preserve existing `AcmeEmailRequired` semantics at startup + service create/update); tests PASS; commit.

---

## Phase 5 — Cutover (wire in, delete Traefik + bridge transport)

> Do this as one coordinated branch section; the build stays red between sub-steps until the last task. Commit per task anyway (atomic, even if intermediate `cargo build` warns about deletions wired later).

### Task 5.1: Build `IngressState` in `app.rs`; remove bridge fields

**Files:** Modify `src/app.rs`
- Real reference lines: `ingress_options` field 49 + bridge fields 46-55; `new` at 59; `LoopbackBridgeSupervisor` build ~84; `Controller::new` 109; `autoscaler_handle` 128; the **two** generic constructors `new_with_deploy_dependencies` (140) and `new_with_deploy_dependencies_and_log` (169); `IngressRenderOptions` built in two bodies (186 and 278 in `AppStateBuilder::build`); `AppStateBuilder` 244-320.

- [ ] **Step 1:** Replace `bridge_allocator`/`bridge_manager`/`bridge_supervisor`/`ingress_options` with `ingress: Arc<IngressState>`. Remove the `IngressRenderOptions` import (28) and both construction blocks (186-191, 278-283).
- [ ] **Step 2:** Update `Controller::new` (109) to take `Arc<IngressState>` (or the narrowed activation/pool trait) instead of `LoopbackBridgeSupervisor`. Update `autoscaler_handle` (128) return type to `(Arc<IngressState>, controller)`.
- [ ] **Step 3:** Remove the `M: BridgeManager` / `B: Into<BridgeAllocator>` generics from **both** `new_with_deploy_dependencies` (140) and `new_with_deploy_dependencies_and_log` (169), and the `AppStateBuilder::build` body (278+). Replace `FakeBridgeManager` usage with a test `IngressState`/fake `ActivationHook` (see Task 5.6 test seam).
- [ ] **Step 4:** `cargo build` (may fail until 5.2-5.4). Commit.

### Task 5.2: Rewire `src/deploy/coordinator.rs`

**Files:** Modify `src/deploy/coordinator.rs`
- Reference: `bridge`/`traefik_config_path` fields ~52-112; `write_routing_config` ~311 (`bridge.assign`+`manager.activate`+`render_file_provider_config`+`fs::write`); stop path ~292.

- [ ] **Step 1:** Remove `bridge`/`traefik_config_path` constructor params and fields.
- [ ] **Step 2:** `write_routing_config` → `IngressState::add_replica(service, replica_id, socket_path)` + `RouteTable` upsert. No YAML, no port assign (unless shim fallback).
- [ ] **Step 3:** Stop path → `remove_replica` + route table update.
- [ ] **Step 4:** `cargo build`. Commit.

### Task 5.3: Rewire `src/deploy/routes.rs`

**Files:** Modify `src/deploy/routes.rs` (`rerender_traefik` → `apply_routes`; `default_ingress_options` at line 11; callers `verify_service_domain`, `delete_service_domain_handler`).

> **Key reconciliation (real bug surfaced):** `routes.rs` keys the routes map by `svc.name` (line 33/38), but `coordinator.rs` keys by `service.id.to_string()` (line 324, deliberately, comment F-3: names only unique per project). Standardize **both** on `service.id` when building the `RouteTable` so two projects' same-named services don't collide. The route table's host index is by domain, but the underlying entry key must be `service.id`.

- [ ] **Step 1:** Remove `default_ingress_options` (11) and the `IngressRenderOptions` import (7).
- [ ] **Step 2:** Replace `render_file_provider_config` + file write with a `RouteTable` rebuild from `list_services`/`list_verified_hostnames` into `IngressState`, keyed by `service.id`.
- [ ] **Step 3:** Rename `rerender_traefik` → `apply_routes`; update call sites (`verify_service_domain`, `delete_service_domain_handler`).
- [ ] **Step 4:** `cargo build`. Commit.

### Task 5.4: Rewire `src/main.rs`; spawn Pingora + ACME tasks

**Files:** Modify `src/main.rs`
- Real reference: `traefik_supervisor` import line 7; the existing `traefik_shutdown_tx`/`rx` mpsc (59) + supervisor task (~81) + `traefik_shutdown_tx.send` (126); `autoscaler_handle`/`set_activator` (~92-94); graceful shutdown via `tokio::signal::ctrl_c` (119-122) + `shutdown_tx.send` (124).

- [ ] **Step 1:** Remove the `traefik_supervisor` import (7) and the supervisor task (~80-82).
- [ ] **Step 2:** **Model Pingora shutdown on the existing `traefik_shutdown` mpsc pattern**: create a `pingora_shutdown` channel, spawn the Pingora `Server` on a dedicated thread (per Spike 0.1), and send on it from the same place `traefik_shutdown_tx.send()` was (126), driven by the existing `ctrl_c` graceful path. Spawn the ACME issuance + renewal tasks. **Boot-load certs before binding `:443`.**
- [ ] **Step 3:** Update `set_activator` wiring (92-94) to the new `IngressState` owner.
- [ ] **Step 4:** Add failure isolation: Pingora bind failure logs a clear `:80`/`:443`-in-use message and the control plane keeps serving `bind_addr` (axum `serve` at 119 unaffected).
- [ ] **Step 5:** `cargo build`. Commit.

### Task 5.5: Delete Traefik + bridge transport; drop `/v1/ingress/config`

**Files:**
- Delete: `src/ingress/traefik.rs`, `src/ingress/traefik_supervisor.rs`
- Modify: `src/ingress/bridge.rs` (remove transport: `LoopbackBridge`, `serve_one`, `BridgeAllocator`, `BridgeTarget`, `BridgeManager`, `FakeBridgeManager`, `tee_proxy`; keep nothing if all moved — delete file if empty), `src/ingress/mod.rs`
- Modify: `src/api/ingress.rs` (remove the `config` handler/route), `src/config.rs`

- [ ] **Step 1:** Remove `GET /v1/ingress/config` route + handler. Keep `/v1/ingress/routes`.
- [ ] **Step 2:** Delete `traefik.rs`, `traefik_supervisor.rs`; remove their `mod` lines + `oci`-traefik pull path.
- [ ] **Step 3:** Remove bridge transport types now unused; delete `bridge.rs` if fully migrated.
- [ ] **Step 4:** `config.rs`: remove `traefik_dynamic_config_path`, `traefik_image`, `traefik_dir`, `acme_resolver` + `ingress_resolver_name()`, the `managed_traefik_tests` module; remove `bridge_start_port` if Spike 0.2 = UDS. Add `DENIA_ACME_DIRECTORY_URL`, `DENIA_TLS_DIR`. Update `for_test` defaults.
- [ ] **Step 5:** `cargo build && cargo test`. Expected: PASS (after contract-test rewrite in 5.6).
- [ ] **Step 6:** Commit.

### Task 5.6: Rewrite backend contract tests

**Files:** Modify `tests/backend_contract.rs` (`traefik_config_*` tests), `tests/domain_verification.rs` (sets `traefik_dynamic_config_path` + `bridge_port` RouteSpec ~339-353), `tests/deploy_orchestration.rs` (`new_with_routing(... FakeBridgeManager ...)` ~186 + `coordinator_writes_traefik_config_on_promotion`).

- [ ] **Step 1:** Replace `traefik_config_routes_domains_to_loopback_bridge_ports` and siblings with route-table assertions (host → service, tls flag) via `IngressState`/`/v1/ingress/routes`.
- [ ] **Step 2:** Fix **all three** test files' `FakeBridgeManager`/`AppStateBuilder`/`new_with_routing`/`traefik_dynamic_config_path` instantiations to the new test seam. Rewrite `coordinator_writes_traefik_config_on_promotion` to assert route-table/replica registration instead of a YAML file.
- [ ] **Step 3:** `cargo test`. Expected: PASS. Commit.

---

## Phase 6 — Frontend (drop `/v1/ingress/config`)

### Task 6.1: Remove `getIngressConfig` from the API client

**Files:** Modify `web/src/effect/api-client.ts`
- Reference: interface decl line 138; impl 605-610; export 804; `parseTextResponse` def 283.

- [ ] **Step 1:** Remove the `getIngressConfig` interface field (138), implementation (605-610), and export (804).
- [ ] **Step 2:** If `parseTextResponse` (283) is now unused, remove it. Run `pnpm typecheck` to confirm.
- [ ] **Step 3:** Commit.

```bash
cd web && git add src/effect/api-client.ts && git commit -m "refactor(web): drop getIngressConfig (endpoint removed)"
```

### Task 6.2: Remove raw-config UI from the ingress route

**Files:** Modify `web/src/routes/ingress.tsx`

- [ ] **Step 1:** Remove `getConfig` effect (13-16), the `config` `useQuery` (31-35), `showConfig`/`copied` state, `handleToggleConfig`/`handleCopy`, and the entire raw-config `<section>` (122-162).
- [ ] **Step 2:** If Spike 0.2 = UDS (bridge_port dropped): replace the table's `port` column + `r.bridge_port` (86, 97-99) — show service port or remove the column. Otherwise leave as-is.
- [ ] **Step 3:** `pnpm typecheck`. Commit.

### Task 6.3: Fix ingress route tests

**Files:** Modify `web/src/routes/-ingress.test.tsx`

- [ ] **Step 1:** Remove/replace the `shows raw YAML config on expand` test (line 91) and the `raw config` toggle assertion (98).
- [ ] **Step 2:** If Spike 0.2 = UDS (`bridge_port` dropped): update the `FIXTURE_ROUTES` fixture's `bridge_port` (lines ~18-27) and the "renders bridge ports in table" test (~82-89) — every `bridge_port` reference in this file.
- [ ] **Step 3:** `pnpm test`. Expected: PASS. Commit.

### Task 6.4: Update `RouteView` schema if `bridge_port` dropped

**Files:** Modify `web/src/effect/schema.ts` (`RouteView.bridge_port` line 130)

- [ ] **Step 1:** Only if Spike 0.2 = UDS and the API no longer returns `bridge_port`: remove the field (and any consumer). Run `pnpm test && pnpm typecheck`. Commit.

---

## Phase 7 — Docs

### Task 7.1: ADR-020 + supersede ADR-016

**Files:** Create `docs/adr/020-pingora-ingress.md`; Modify `docs/adr/README.md`, `docs/adr/016-managed-traefik.md` (Status → Superseded).

- [ ] **Step 1:** Write ADR-020 per the spec's "ADR-020" section (supersede ADR-016; replace ingress mechanism of ADR-007; document UDS-vs-shim outcome from spikes; `/v1/ingress/config` removed). Add the table row in README; mark ADR-016 Superseded.
- [ ] **Step 2:** Commit.

### Task 7.2: Update `CLAUDE.md` + README

**Files:** Modify root `CLAUDE.md` (Traefik paragraph under Project Conventions), `README.md`.

- [ ] **Step 1:** Rewrite the ingress paragraph: Denia owns `:80`/`:443` via in-process Pingora; ACME in-process (instant-acme, HTTP-01); workload upstreams are Denia-owned Unix sockets (no bridge, or thin shim). Remove Traefik mentions. Update README ingress/autoscaling notes.
- [ ] **Step 2:** Commit.

---

## Phase 8 — Verification

### Task 8.1: Privileged end-to-end ingress test

**Files:** Modify/create `tests/linux_runtime_privileged.rs` (opt-in via `DENIA_RUN_PRIVILEGED_TESTS=1`).

> **Prerequisite (BLOCKER if skipped):** the Rust binary embeds `web/dist/client` via `rust-embed` (`src/web.rs`); a release build *requires* it to exist, and the Phase 6 frontend edits invalidate the existing build. Run `cd web && pnpm build` **before** booting Denia in this test or any release `cargo build`.

- [ ] **Step 0:** `cd web && pnpm build` (regenerate `dist/client` after Phase 6).
- [ ] **Step 1: Write failing test** — boot Denia with Pingora; deploy a service; `GET http://<host>:80/` proxies to the workload UDS and returns 200; unknown host → 404; `tls_enabled` host on `:80` → 308; scale-from-zero → 503-then-200 after activation.
- [ ] **Step 2:** Run `DENIA_RUN_PRIVILEGED_TESTS=1 cargo test --test linux_runtime_privileged -- --ignored`. Expected: PASS.
- [ ] **Step 3:** Commit.

### Task 8.2: Full verification sweep

- [ ] **Step 1:** `cd web && pnpm typecheck && pnpm test && pnpm build` (frontend first — backend release embed needs fresh `dist/client`).
- [ ] **Step 2:** `cargo fmt --all`
- [ ] **Step 3:** `cargo clippy --all-targets --all-features` — fix lints.
- [ ] **Step 4:** `cargo build && cargo test`
- [ ] **Step 5:** `DENIA_RUN_PRIVILEGED_TESTS=1 cargo test --test linux_runtime_privileged -- --ignored`
- [ ] **Step 6:** Report exact commands + results. Commit any fixes.

---

## Notes for the implementer

- **TDD:** every logic task writes the failing test first (Phase 2-4, 6, 8). Phase 0 spikes and Phase 5 cutover are mechanical/integration — test coverage comes from the contract + privileged tests.
- **UUIDv7:** any new persisted IDs use `Uuid::now_v7()` (CLAUDE.md).
- **Secrets:** never log key authorizations, private keys, the ACME account key, or decrypted SOPS payloads.
- **Typed errors at boundaries:** add `IngressError`/`AcmeError` variants; no panics for expected failures.
- **The UDS-vs-shim branch** (Spike 0.2) threads through Tasks 2.1, 3.1, 5.2, 5.5, 6.2, 6.4 — resolve it before starting Phase 2.
