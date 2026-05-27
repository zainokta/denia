# Pingora In-Process Ingress (Replacing Managed Traefik)

- **Date**: 2026-05-28
- **Status**: Draft (pending review)
- **Supersedes**: ADR-016 (Denia-Managed Traefik); amends ADR-007 (Ingress + TLS)
- **New ADR**: ADR-020

## Summary

Replace the supervised Traefik host process and its loopback-bridge transport
with an in-process ingress built on [Pingora](https://github.com/cloudflare/pingora).
Denia becomes its own L7 proxy: it binds `:80`/`:443`, terminates TLS using
certificates it issues itself via ACME (instant-acme, HTTP-01), and proxies
each request directly to a workload's Denia-owned Unix socket.

This is a **big-bang replacement**: Traefik acquisition, supervision, dynamic
file-provider config rendering, and the bridge's UDS→TCP transport are removed
in a single coordinated change. The bridge's *control logic* (replica pools,
health, scale-from-zero activation, idle tracking, access log) is preserved and
re-hosted inside the Pingora proxy.

## Motivation

All four drivers below were confirmed:

1. **Kill the external binary.** No more OCI-pulling and supervising a Go
   process. Removes the ADR-016 consequences entirely: SELinux/AppArmor exec
   denial, `EADDRINUSE` fatal handling, `traefik.log` rotation, restart-drops-
   connections, the digest-cache acquisition path.
2. **Zero-downtime reload.** Route, scale, and cert changes are applied live by
   swapping shared state — no process restart. (Pingora's SIGQUIT graceful
   *binary* upgrade is available as future capability but not wired in v1.)
3. **Delete the bridge transport.** Pingora connects upstream directly to the
   workload Unix socket, removing the UDS→loopback-TCP hop in `bridge.rs`.
4. **Performance / control.** Native Rust request path, custom middleware in the
   `ProxyHttp` impl, no cross-process hop for config.

## Current Architecture (what we're replacing)

- **`src/ingress/traefik.rs`** — `RouteSpec`, `IngressRenderOptions`,
  `render_file_provider_config()` (emits Traefik dynamic YAML). The renderer dies;
  `RouteSpec` stays as the in-memory model.
- **`src/ingress/traefik_supervisor.rs`** — OCI pull of `traefik:v3.3`, static
  config, supervised child, backoff, graceful stop. Deleted entirely.
- **`src/ingress/bridge.rs`** — `LoopbackBridgeSupervisor`/`LoopbackBridge`.
  Holds:
  - `ServicePool` / `ReplicaEndpoint` — per-service replica set + round-robin
    (`next_socket`).
  - Health (`set_replica_healthy`, `healthy_count`).
  - `ActivationHook` — scale-from-zero (ADR-018): a request to a zero-replica
    service fires the activator and waits for a replica.
  - `last_activity` — scale-to-zero idle detection.
  - `AccessLogStore`.
  - `BridgeAllocator` / `BridgeTarget` — loopback port assignment (Traefik compat;
    **removed**).
  - The transport: `serve_one()` accepts on a loopback TCP port and
    `copy_bidirectional` to the workload UDS. **Removed** — Pingora is the
    listener and the upstream connector.
- **`src/ingress/socket_proxy.rs`** — per-workload sidecar; the workload listens
  on loopback inside its netns and the sidecar exposes a host Unix socket.
  **Unchanged.** The host UDS is the ingress upstream endpoint.
- **`AppState`** (`src/app.rs`) — owns `bridge_allocator`, `bridge_manager`,
  `bridge_supervisor`, `routes: SharedRoutes`, `ingress_options`. The bridge
  fields collapse into a single `Arc<IngressState>`.
- **API** — `GET /v1/ingress/routes` (live snapshot, kept) and
  `GET /v1/ingress/config` (Traefik dynamic YAML, **dropped**).

## Decision

### Module layout

New `src/ingress/pingora/`:

| File | Responsibility |
|------|----------------|
| `mod.rs` | Re-exports; module wiring. |
| `state.rs` | `IngressState`: `ArcSwap<RouteTable>`, migrated replica pools / health / activation / activity, `ArcSwap<CertStore>`, `ArcSwap<ChallengeMap>`, `AccessLogStore`. |
| `proxy.rs` | `DeniaProxy: ProxyHttp` — request routing brain. |
| `tls.rs` | `TlsAccept` callback selecting cert by SNI. |
| `acme.rs` | instant-acme order driver + renewal scheduler + challenge publication. |
| `server.rs` | Builds Pingora `Server`, adds `:80`/`:443` services, owns lifecycle/shutdown. |

`traefik.rs` and `traefik_supervisor.rs` are deleted. The transport half of
`bridge.rs` is deleted; its control types (`ServicePool`, `ReplicaEndpoint`,
`ActivationHook`, health/activity) move into `state.rs` (renamed as needed).
`RouteSpec` moves to `state.rs` (or a small `model.rs`) since `traefik.rs` is gone.

### Request flow

`DeniaProxy` implements `ProxyHttp` with a per-request `CTX` carrying the
resolved service name and chosen replica.

**Port 80 (`web`) `request_filter` — challenge paths win unconditionally, before
host resolution or any 404:**
1. If path starts with `/.well-known/acme-challenge/` **or**
   `/.well-known/denia-challenge/` → route to the **control-plane backend**
   (`bind_addr`, default `127.0.0.1:7180`), bypassing host routing. Set a ctx flag
   so `upstream_peer` returns the control peer. Denia's axum already serves
   `denia-challenge` (ADR-013); add an axum handler for `acme-challenge` backed by
   the in-process ACME driver. This keeps challenge serving in one place (axum) and
   keeps Pingora dumb — **no async work in any TLS callback**.
2. Else resolve `Host` in `RouteTable`. If the matched service has
   `tls_enabled` → respond `308` redirect to `https://<host><path>`. Return
   `Ok(true)`.
3. Else fall through (plain-HTTP service) to `upstream_peer`.

> **Why proxy challenges to axum instead of answering in Pingora:** ACME order
> state and the `denia-challenge` token store live in the control plane. Proxying
> avoids duplicating that state into the proxy and sidesteps doing async lookups
> inside Pingora's sync handshake path. The control backend is always up
> (failure-isolation invariant), so this hop is safe.

**Port 443 (`websecure`) `request_filter`:** no ACME/redirect special-casing;
fall through to `upstream_peer`. (TLS already terminated by the listener.)

**`upstream_peer` (shared):**
1. Resolve `Host` → service in `RouteTable`. Unknown host → `respond_error(404)`.
2. Pick a healthy replica via round-robin from the migrated `ServicePool`.
3. If zero healthy replicas → fire `ActivationHook` (scale-from-zero) and await a
   replica up to a deadline; on timeout → `respond_error(503)`.
4. Bump `last_activity` for idle tracking.
5. Return an `HttpPeer` targeting the replica's **Unix socket** (plain HTTP, no
   upstream TLS — the workload is loopback/UDS-local).

**Access log:** `logging()` phase writes to the migrated `AccessLogStore`
(preserving ADR-009 observability).

### Upstream transport — UDS vs fallback

**Open item to verify first in implementation:** confirm `HttpPeer` (or the
underlying `Peer`/connector) supports a **Unix-domain-socket upstream** in the
pinned Pingora version. Pingora's documented `HttpPeer::new((host, port), ...)`
is TCP-first.

- **If UDS upstream is supported:** Pingora connects directly to the workload
  host UDS. The bridge transport is fully deleted. Preferred.
- **If not:** retain a minimal per-replica UDS→loopback-TCP shim (a slimmed
  reuse of the existing `socket_proxy`/`serve_one` hop) and have Pingora connect
  to `127.0.0.1:<port>`. The bridge's *control brain* still moves to
  `IngressState`; only the thin transport hop survives. The ADR documents this as
  the fallback and the reason.

This decision is isolated to `proxy.rs::upstream_peer` + a small connector helper;
it does not change the rest of the design.

### TLS / ACME (instant-acme + HTTP-01)

**Strict separation: cert *selection* is sync and in-process; cert *issuance* is
async and fully out-of-band.** The `TlsAccept` callback never does async work and
never issues — it only serves already-issued certs.

- **Listener:** `:443` built via `TlsSettings` plus a `TlsAccept` callback. The
  callback reads `ArcSwap<CertStore>` (SNI → parsed cert/key) synchronously at
  handshake. Renewal/issuance swaps the `ArcSwap` with zero restart and zero
  dropped connections.
- **Mid-issuance / missing-cert handshake behavior (was undefined):** if a SNI has
  no cert in `CertStore` yet, the callback declines (no cert set) and the handshake
  fails. There is **no on-demand/blocking issuance** in the handshake. The client
  retries once issuance completes. The HTTP→HTTPS redirect on `:80` only fires for
  a `tls_enabled` service whose cert is present; until issuance completes, `:80`
  continues to serve (or the operator hits a TLS failure — documented).
- **Boot-time load:** on startup, scan `<data_dir>/tls/*/` and load all existing
  certs into `CertStore` **before** binding `:443`. Without this, a restart would
  re-order every cert and risk Let's Encrypt rate limits.
- **Issuance (`acme.rs`, background task on the tokio runtime):** on a
  `tls_enabled` service create/update or successful domain verification (ADR-013),
  enqueue an ACME order against the configured directory (LE prod default; LE
  staging via env for tests). HTTP-01:
  - Register `token → key_authorization` with the axum acme-challenge handler
    (served on `:80` via the challenge proxy hop, see request flow).
  - On `valid`, fetch the cert chain and persist **atomically** (write temp +
    `rename`) to `<data_dir>/tls/<domain>/{fullchain.pem,key.pem}` at mode `0600`,
    then swap into `CertStore`.
- **ACME account key:** persisted at `<data_dir>/tls/account.key` mode `0600`;
  loaded (or created) at startup. Never logged.
- **Renewal:** a background task scans certs and re-orders when within the renewal
  window (e.g. 30 days before `notAfter`).
- **Control domain:** `DENIA_CONTROL_DOMAIN` gets the same ACME treatment when
  `control_tls` is set; its upstream peer is the control-plane bind addr.
- **`DENIA_ACME_EMAIL`** stays required iff any service (or the control domain)
  has TLS enabled — keep `ConfigError::AcmeEmailRequired`, validated at startup
  and at service create/update.
- **Secrets discipline:** never log key authorizations, private keys, or ACME
  account keys (CLAUDE.md). The ACME account key is stored under `<data_dir>/tls/`
  at `0600`.

### Embedding & lifecycle (primary risk)

Pingora's `Server` owns its own tokio runtimes and installs SIGQUIT/SIGTERM
handlers, which conflicts with Denia's existing tokio + axum runtime and signal
handling.

- Start the Pingora `Server` on a **dedicated OS thread** spawned from `main`
  after `AppState` is built. The proxy services share `Arc<IngressState>` with the
  control plane.
- **Signal coordination:** Denia keeps ownership of process signals. Pingora is
  driven via an explicit shutdown channel (`ShutdownWatch` / a `tokio::sync::watch`
  the server loop observes) triggered from Denia's existing shutdown path, so the
  two do not both trap SIGTERM. Verify whether Pingora's server can be constructed
  without installing its own signal handlers in the pinned version; if not,
  isolate it so Denia's handler is authoritative.
- **Failure isolation (preserve ADR-016 property):** if Pingora fails to bind
  `:80`/`:443`, the control plane keeps serving on `bind_addr` (`IP:7180`). A
  proxy failure never deadlocks management API access. Surface a clear
  `EADDRINUSE`-style error ("`:80`/`:443` already in use — stop any external proxy,
  Denia owns these ports").
- Route/scale/cert changes never restart the process; they swap `ArcSwap` state.

### State wiring (`app.rs`) — full call-site list

- Remove fields `bridge_allocator`, `bridge_manager`, `bridge_supervisor`,
  `ingress_options` (render flavor). Add `ingress: Arc<IngressState>`.
- `AppState::new` (and the privileged/full constructor at ~line 84) currently
  builds `LoopbackBridgeSupervisor::with_access_log`, a `BridgeAllocator`, and
  passes `supervisor.clone()` into `Controller::new` (~line 109) as the
  `BridgeManager`. Replace with `IngressState` construction; `Controller::new`
  takes `Arc<IngressState>` (or the narrowed `ActivationHook`/pool trait) instead
  of the supervisor.
- `autoscaler_handle()` (~line 128) returns `(Arc<LoopbackBridgeSupervisor>,
  controller)`. Change its return type to `(Arc<IngressState>, controller)` (or
  just the controller if the activator binds internally).
- The generic `M: BridgeManager` / `B: Into<BridgeAllocator>` plumbing on the two
  test/deploy constructors (`new_with_deploy_dependencies` and the
  `AppStateBuilder::build` path, ~lines 146–311) is removed. `FakeBridgeManager`
  is replaced by a test `IngressState` (or a fake `ActivationHook`/pool) — see
  Testing.
- `SharedRoutes` (`Arc<Mutex<BTreeMap<String, RouteSpec>>>`) stays for
  `GET /v1/ingress/routes`; `IngressState` is the live routing source. Deploy/
  stop/scale paths update both (or `IngressState` derives the snapshot).
- The autoscaler (ADR-018) rebinds its activator hook and idle reaper to
  `IngressState` — same `ActivationHook` trait + `SharedController`, new owner.
  `next_socket`, `healthy_count`, `last_activity`, `set_replica_healthy`,
  `add_replica`, `remove_replica` keep their signatures so call sites change
  minimally.

### Deploy / routes integration — full call-site list

- `src/deploy/routes.rs::rerender_traefik` and `render_file_provider_config`
  callers (`verify_service_domain`, `delete_service_domain_handler`) no longer
  render YAML. They update `RouteTable` in `IngressState` (and the API snapshot).
  Rename `rerender_traefik` → `apply_routes` (or similar).
- **`src/deploy/coordinator.rs`** (missed in the first draft): the
  `DeployCoordinator` holds `bridge: Arc<Mutex<BridgeAllocator>>` and
  `traefik_config_path`. Two sites change:
  - `write_routing_config` (~line 311) calls `bridge.assign` +
    `manager.activate` + `render_file_provider_config` + `std::fs::write(traefik_config_path)`.
    Replace with `IngressState::add_replica` (register the workload UDS) and a
    `RouteTable` update — no port allocation (unless the UDS-upstream fallback is
    chosen), no YAML.
  - The stop path (~line 292) re-renders YAML on teardown → replace with
    `remove_replica` + route table update.
  - Remove the `bridge` and `traefik_config_path` constructor params (~lines
    52–112).
- **`src/main.rs`** (missed in the first draft): remove the
  `ingress::traefik_supervisor` import (line 7) and the Traefik supervisor task
  (~lines 80–82). Add: spawn the Pingora `Server` (on its dedicated thread) and
  the ACME issuance/renewal task. Update the `autoscaler_handle()`/`set_activator`
  wiring (~lines 92–94) to the new owner type.
- `traefik_config_path` / `traefik_dynamic_config_path` removed from
  `coordinator.rs` and `config.rs`.

### API surface

- `GET /v1/ingress/routes` — **kept**, unchanged body.
- `GET /v1/ingress/config` — **dropped** (was Traefik dynamic YAML). Breaking
  change; documented in ADR-020 and README.

### Config / dependencies — full field list (`config.rs`)

- **Remove fields + their env + `for_test` defaults:** `traefik_dynamic_config_path`
  (`DENIA_TRAEFIK_DYNAMIC_CONFIG`), `traefik_image` (`DENIA_TRAEFIK_IMAGE`),
  `traefik_dir`, `acme_resolver` (`DENIA_ACME_RESOLVER`) + the
  `ingress_resolver_name()` method (now meaningless — Traefik's certResolver name).
- **`bridge_start_port` (`DENIA_BRIDGE_START_PORT`):** fate depends on the
  UDS-upstream spike. Removed if Pingora connects to UDS directly; kept if the
  loopback-TCP fallback is used.
- **Remove the `managed_traefik_tests` module** in `config.rs` (asserts
  `traefik_dir`/`traefik_dynamic_config_path`); replace with TLS-dir assertions.
- **Keep env:** `DENIA_HTTP_PORT` (80), `DENIA_HTTPS_PORT` (443),
  `DENIA_ACME_EMAIL`, `DENIA_CONTROL_DOMAIN`, control TLS flag.
- **Add env:** `DENIA_ACME_DIRECTORY_URL` (default LE prod; LE staging for tests),
  `DENIA_TLS_DIR` (default `<data_dir>/tls`).
- **Cargo.toml:** add `pingora` + `pingora-proxy` (pinned), `instant-acme`. Pick
  Pingora's TLS backend (boringssl default vs `rustls` feature) — boringssl is the
  default and supports the `TlsAccept` cert callback; confirm during the spike.
  Keep the OCI puller (still used for workload images).

## ADR-020

Author `docs/adr/020-pingora-ingress.md`:
- Status: Accepted (or Proposed, per maintainer).
- Supersede ADR-016 (mark it Superseded in `docs/adr/README.md`).
- **Supersede the ingress mechanism of ADR-007** (which is still *Proposed*):
  ingress no longer uses a file provider / dynamic YAML; routing is in-memory;
  `GET /v1/ingress/config` removed; the in-memory snapshot remains canonical for
  `/v1/ingress/routes`. (ADR-020 replaces it rather than "amending" an
  unaccepted ADR; note ADR-007's TLS-toggle data model — `tls_enabled` — is
  retained.)
- Update CLAUDE.md ingress paragraph: Denia owns `:80`/`:443` via in-process
  Pingora; ACME is in-process (instant-acme, HTTP-01); workload upstreams are
  Denia-owned Unix sockets (no loopback bridge, or thin shim if UDS-upstream
  unsupported).

## Testing

- **Unit:** `RouteTable` host match (exact + control domain); replica round-robin;
  zero-replica activation path (mock `ActivationHook`); idle `last_activity`
  bumps; ACME challenge publish/serve/clear; `CertStore` SNI selection + swap;
  `denia-challenge` + `acme-challenge` path interception precedes unknown-host
  404.
- **Test seam:** replace `FakeBridgeManager` with a fake `IngressState`/
  `ActivationHook` double; update `AppStateBuilder::build` and
  `new_with_deploy_dependencies` (no more `M: BridgeManager` generic). Audit
  `tests/backend_contract.rs` and `tests/domain_verification.rs` instantiations.
- **Access log fidelity (ADR-009):** map Pingora `Session` → `AccessEntry` in the
  `logging()` phase — `status`, `bytes`, `duration_ms`, host, path — to preserve
  what the current byte-parsing `tee_proxy` populated.
- **ACME integration:** run against [pebble](https://github.com/letsencrypt/pebble)
  or a mock CA over HTTP-01; assert cert persisted at `0600` and served via SNI.
- **Contract:** rewrite `tests/backend_contract.rs::traefik_config_*` to assert the
  in-memory route table (no YAML).
- **Privileged (`linux_runtime_privileged`, opt-in):** real bind on `:80`/`:443`,
  end-to-end request → UDS upstream → workload response; HTTP→HTTPS redirect;
  unknown-host 404; scale-from-zero 503-then-200.
- `cargo build`, `cargo test`, `cargo fmt --all`, `cargo clippy --all-targets
  --all-features`.

## Risks & Mitigations

| Risk | Mitigation |
|------|------------|
| Pingora `Server` signal handling fights Denia's | Dedicated thread + explicit shutdown channel; verify no-signal-handler construction; Denia stays authoritative. |
| UDS upstream unsupported by `HttpPeer` | Fallback thin UDS→loopback-TCP shim; isolated to `upstream_peer`. |
| `TlsAccept` dynamic-cert callback shape differs from docs in pinned version | Verify the callback API before building `tls.rs`; boringssl backend is the documented path. |
| ACME HTTP-01 needs `:80` reachable publicly | Same requirement as today's Traefik HTTP-01; no regression. |
| Loss of Traefik's built-in features (middlewares, dashboard) | Out of scope; Denia only used routing + ACME + redirect, all reimplemented. |
| Big-bang landing has no mid-flight rollback | Land behind a feature branch; full test matrix (incl. privileged) green before merge. |

## Out of Scope

- Pingora SIGQUIT graceful *binary* upgrade wiring (future).
- Per-service certResolver override (ADR-007 already defers this).
- HTTP/3, advanced middlewares, rate limiting, WAF.
- Multi-node control plane.

## Gating Pre-Implementation Spikes (must resolve BEFORE the plan, not during)

These three determine whether the design holds. Run a throwaway spike against the
pinned Pingora version before writing the implementation plan:

1. **Signal handling.** Can Pingora's `Server` be constructed/run without installing
   its own SIGTERM/SIGINT/SIGQUIT handlers (so Denia stays authoritative)? Pingora
   has historically trapped these unconditionally. If it cannot be suppressed, the
   "dedicated thread + explicit shutdown channel" lifecycle must be redesigned.
   **This gates the whole embedding model.**
2. **UDS upstream.** Does `HttpPeer`/the connector support a Unix-domain-socket
   upstream? Decides full bridge deletion vs. the thin loopback-TCP shim, and
   whether `bridge_start_port` survives. Two of the four motivations hinge on this.
3. **Dynamic per-SNI cert callback.** Confirm the exact `TlsAccept` (or equivalent)
   API shape in the pinned version and that boringssl supports declining (no-cert)
   for an unknown SNI.
