# Ingress + TLS Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add opt-in per-service TLS via Traefik ACME, a routable control-plane domain, and read-only ingress config endpoints, keeping ACME issuance in Traefik's operator-owned static config.

**Architecture:** Extend the Traefik dynamic-file renderer to emit TLS routers (`websecure` entrypoint + `tls.certResolver`) and HTTP->HTTPS redirect routers for services with `tls_enabled`, plus a control-plane router from `DENIA_CONTROL_DOMAIN`. Node-wide ingress settings come from config via an `IngressRenderOptions`. New `/v1/ingress/{routes,config}` endpoints expose the live promoted routing snapshot, not a route list rebuilt from stored services.

**Tech Stack:** Rust 2024, axum 0.8, rusqlite, serde, thiserror. Spec: `docs/superpowers/specs/2026-05-25-ingress-tls.md`.

---

## File Structure

- `src/config.rs` — ingress config fields.
- `src/traefik.rs` — `RouteSpec.route_key`, `RouteSpec.tls`, `IngressRenderOptions`, safe route-key/domain validation, TLS + redirect + control-plane rendering, `MissingResolver`.
- `src/domain.rs` — `ServiceConfig.tls_enabled`.
- `src/state.rs` — migration backfilling `tls_enabled`.
- `src/deploy.rs` — promoted route snapshot shared with `AppState` and used by the writer.
- `src/app.rs` — `/v1/ingress/routes` + `/v1/ingress/config` handlers; render from live promoted route snapshot + config.
- `docs/adr/007-ingress-tls.md` + `docs/adr/README.md`, `AGENTS.md` note.
- Tests colocated + `tests/backend_contract.rs`.

Commit after each task.

---

## Task 1: Ingress config fields

**Files:**
- Modify: `src/config.rs`
- Test: `src/config.rs`

- [ ] **Step 1: Write failing test** — `from_env` defaults: `acme_resolver == "le"`, `control_domain == None`, `control_tls == false`, entrypoints `web`/`websecure`.
- [ ] **Step 2: Run** `cargo test config` → FAIL.
- [ ] **Step 3: Implement** — add the five fields (see spec) to `AppConfig`, parse in `from_env`, mirror in `for_test`.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** — `git commit -m "feat(config): ingress + TLS settings"`

---

## Task 2: `RouteSpec.route_key` + `RouteSpec.tls` + `IngressRenderOptions`

**Files:**
- Modify: `src/traefik.rs`
- Test: `src/traefik.rs`

- [ ] **Step 1: Write failing tests**

```rust
#[test]
fn renders_tls_router_with_resolver_and_redirect() {
    let routes = vec![RouteSpec {
        route_key: "svc-web".into(),
        service_name: "web".into(),
        domains: vec!["app.example.com".into()],
        bridge_port: 9000,
        tls: true,
    }];
    let opts = IngressRenderOptions::test_defaults(); // resolver "le", web/websecure, no control
    let yaml = render_file_provider_config(&routes, &opts).unwrap();
    assert!(yaml.contains("certResolver: le"));
    assert!(yaml.contains("websecure"));
    assert!(yaml.contains("redirectScheme"));
}

#[test]
fn renders_plain_router_without_tls() {
    let routes = vec![RouteSpec {
        route_key: "svc-web".into(),
        service_name: "web".into(),
        domains: vec!["app".into()],
        bridge_port: 9000,
        tls: false,
    }];
    let yaml = render_file_provider_config(&routes, &IngressRenderOptions::test_defaults()).unwrap();
    assert!(!yaml.contains("certResolver"));
}

#[test]
fn tls_without_resolver_errors() {
    let routes = vec![RouteSpec {
        route_key: "svc-web".into(),
        service_name: "web".into(),
        domains: vec!["a".into()],
        bridge_port: 1,
        tls: true,
    }];
    let mut opts = IngressRenderOptions::test_defaults();
    opts.acme_resolver = String::new();
    assert_eq!(render_file_provider_config(&routes, &opts).unwrap_err(), TraefikError::MissingResolver);
}
```

- [ ] **Step 2: Run** → FAIL (signature/field/variant missing).
- [ ] **Step 3: Implement**
  - Add generated/sanitized `pub route_key: String` and `pub tls: bool` to `RouteSpec`; add `IngressRenderOptions` struct + a `test_defaults()` ctor under `#[cfg(test)]`.
  - Change `render_file_provider_config(routes, opts)`; for TLS routers add `entryPoints: [websecure]`, `tls: { certResolver: <resolver> }`, a redirect middleware + companion `web` router; non-TLS keeps `[web]`.
  - Use `route_key` for all Traefik router/service/middleware YAML object names. Never use raw `service_name` or domain text as YAML keys.
  - Validate domains before writing `Host()` rules: reject empty values, backticks/control characters, and unsafe hostname syntax.
  - Add `TraefikError::MissingResolver`.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** — `git commit -m "feat(traefik): per-service TLS + redirect rendering"`

---

## Task 3: Control-plane route rendering

**Files:**
- Modify: `src/traefik.rs`
- Test: `src/traefik.rs`

- [ ] **Step 1: Write failing test** — with `opts.control_domain = Some("denia.example.com")`, `control_tls = true`, `control_backend_addr = "http://127.0.0.1:7180"`, the YAML has a router for that host -> that backend with TLS.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** — when `control_domain` is set, append a router + service (`denia-control`) to `control_backend_addr`; TLS per `control_tls`. Build `control_backend_addr` from parsed `AppConfig.bind_addr`, using loopback when the bind IP is unspecified.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** — `git commit -m "feat(traefik): control-plane domain route"`

---

## Task 4: `ServiceConfig.tls_enabled` + migration

**Files:**
- Modify: `src/domain.rs`, `src/state.rs`
- Test: `src/domain.rs`, `src/state.rs`

- [ ] **Step 1: Write failing tests** — `ServiceConfig` deserializes without `tls_enabled` (defaults false); after migrate, an existing service row reads `tls_enabled == false`.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement** — add `#[serde(default)] pub tls_enabled: bool` to `ServiceConfig` + `new` param (or setter); add a migration step that ensures the column/JSON default. Reuse the shared versioned-migration infra from sub-project B; if that infra is absent, introduce the shared ledger here first instead of adding an incompatible ad-hoc migration.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** — `git commit -m "feat(domain): per-service tls_enabled"`

---

## Task 5: Ingress endpoints + live route snapshot

**Files:**
- Modify: `src/app.rs`, `src/deploy.rs`
- Test: `tests/backend_contract.rs`

- [ ] **Step 1: Write failing tests**
  - `GET /v1/ingress/routes` returns active promoted service routes (with `tls` reflecting `tls_enabled`) + the control route when configured.
  - Undeployed services are absent; stopped services disappear after lifecycle stop.
  - Read-only ingress requests do not call `BridgeAllocator.assign`, allocate new ports, or mutate the route snapshot.
  - `GET /v1/ingress/config` returns `text/plain` containing `http:` and the routers.
- [ ] **Step 2: Run** → FAIL.
- [ ] **Step 3: Implement**
  - Move the route map currently owned inside `DeploymentCoordinator::RoutingState` into shared `AppState` (for example `Arc<Mutex<BTreeMap<String, RouteSpec>>>`) and pass it into the coordinator, so promotion writer and ingress API read the same snapshot.
  - Route insertion during promotion uses a generated safe key (service id or project-qualified key once Projects has landed), domains, assigned bridge port, and `tls_enabled`.
  - A helper reads the live route snapshot and builds `IngressRenderOptions` from `state.config`. Do not assemble ingress routes from `store.list_services()` because stored services do not contain live bridge ports.
  - `GET /v1/ingress/routes` -> JSON of typed `RouteView` rows (`kind`, `route_key`, `service_name`, `domains`, `bridge_port`, `tls` for services; `kind`, `domains`, `backend_url`, `tls` for control); include the control pseudo-route.
  - `GET /v1/ingress/config` -> `render_file_provider_config(...)` body as `text/plain`.
  - Add routes to the `protected` router.
- [ ] **Step 4: Run** → PASS.
- [ ] **Step 5: Commit** — `git commit -m "feat(api): /v1/ingress routes and config"`

---

## Task 6: ADR + docs

**Files:**
- Create: `docs/adr/007-ingress-tls.md`
- Modify: `docs/adr/README.md`, `AGENTS.md`

- [ ] **Step 1:** ADR-007 (Proposed): annotate-only ACME boundary; opt-in per-service TLS + redirect; control-plane domain; ingress viewer endpoints. Alternatives: Denia owns Traefik static; global TLS toggle.
- [ ] **Step 2:** Index row + AGENTS.md note (new env vars; operator must configure Traefik static ACME + `acme.json`).
- [ ] **Step 3: Commit** — `git commit -m "docs: ADR-007 ingress and TLS"`

---

## Final Verification

- [ ] `cargo build`, `cargo fmt --all`, `cargo clippy --all-targets --all-features`.
- [ ] `cargo test` — traefik render (tls/redirect/control/missing-resolver), config, state migration, ingress API all green.
- [ ] Manual: enable `tls_enabled` on a service, set `DENIA_CONTROL_DOMAIN`; `GET /v1/ingress/config` shows the TLS router, redirect, and control route; verify Traefik (with operator ACME configured) serves HTTPS.

## Notes

- ACME issuance/storage stays in Traefik's static config (operator/installer). The
  installer sub-project should seed `certificatesResolvers.<name>.acme` + `acme.json`.
- Single source of truth: the API renders from the same live promoted route
  snapshot and renderer the writer uses.
- Frontend is the companion plan `2026-05-25-ingress-tls-frontend.md`.
