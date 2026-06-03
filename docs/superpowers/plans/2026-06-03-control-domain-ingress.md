# Control Domain Over Ingress Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Serve one operator-configured hostname (`control_domain`) over Denia's Pingora ingress on `:443` (ACME TLS), reverse-proxied to the existing axum control backend, exposing console + `/v1` + `/v2` registry over HTTPS, with the per-IP auth rate limiter made effective via real client-IP forwarding.

**Architecture:** The Pingora proxy special-cases `Host == control_domain` ahead of workload routing and dials the loopback control backend (`HttpPeer::new`). The control domain's TLS cert is issued/renewed by the existing ACME orchestration as a distinct branch (it has no service row). An `upstream_request_filter` overwrites `X-Forwarded-For` with the real downstream peer so the existing loopback-trusting rate limiter keys on the true client IP. `:7180` stays loopback-bound.

**Tech Stack:** Rust 2024, axum, Pingora 0.8 (boringssl), instant-acme (HTTP-01).

**Spec:** `docs/superpowers/specs/2026-06-03-control-domain-ingress-design.md`

---

## Environment Notes (this worktree)

- **Build/test prefix:** the repo's `./target` is root-owned here; build and test with a writable target dir:
  `CARGO_TARGET_DIR=/tmp/denia-verify cargo build` / `... cargo test`.
- `web/dist` is symlinked from the main tree (gitignored SPA build) so `rust-embed` compiles. Frontend is out of scope.
- All commands run from the worktree root: `/home/rakei/Project/denia/.worktrees/control-domain-ingress`.

## File Structure (what changes and why)

| File | Responsibility / change |
|------|--------------------------|
| `src/config.rs` | Validate + normalize `control_domain` (already a field) at load; new `ConfigError::InvalidControlDomain`. |
| `src/ingress/pingora/proxy.rs` | `DeniaProxy` carries `control_domain`/`control_tls`; pure helpers (`control_tls_for_host`, `is_control_host`, `forwarded_for`); control-domain routing in `request_filter` + `upstream_peer`; new `upstream_request_filter` (XFF overwrite). |
| `src/ingress/pingora/server.rs` | `IngressServerConfig` carries `control_domain`/`control_tls`; `from_ports` signature; thread into `DeniaProxy::http`/`https`. |
| `src/daemon.rs` | Pass new fields to `IngressServerConfig::from_ports`; issue the control-domain cert at boot + each renewal tick (distinct branch). |
| `src/api/domains.rs` | Reject creating/verifying a service domain equal to `control_domain`. |
| `src/rate_limit.rs` | Comment cleanup (Traefik → Pingora); add `extract_client_ip` XFF test. |
| `docs/adr/035-control-domain-ingress.md` (new) | ADR for public control-plane exposure (extends ADR-020). |
| `docs/adr/README.md` | Index the new ADR. |

> Note: ADR number `034` assumes max committed ADR on this branch is `033`. If a merge collides, renumber.

---

### Task 1: Validate `control_domain` at config load

**Files:**
- Modify: `src/config.rs` (add `ConfigError::InvalidControlDomain`; validate in `from_env` after `control_domain` is resolved, ~line 454-456; store normalized form)
- Test: `src/config.rs` (`mod tests`)

- [ ] **Step 1: Write the failing test**

Add to `src/config.rs` `mod tests` (uses the existing `EnvGuard` + `isolated_config_file` helpers and `FROM_ENV_LOCK`):

```rust
#[test]
fn invalid_control_domain_is_rejected() {
    let _lock = FROM_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (_cfg_dir, _cfg_file) = isolated_config_file();
    let _admin = EnvGuard::set("DENIA_ADMIN_TOKEN", "x".repeat(64));
    let _key_file = EnvGuard::set("DENIA_AGE_KEY_FILE", "/nonexistent/denia-test/age.key");
    let _cd = EnvGuard::set("DENIA_CONTROL_DOMAIN", "has space.example.com");
    assert!(matches!(
        AppConfig::from_env(),
        Err(ConfigError::InvalidControlDomain(_))
    ));
}

#[test]
fn valid_control_domain_is_lowercased() {
    let _lock = FROM_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (_cfg_dir, _cfg_file) = isolated_config_file();
    let _admin = EnvGuard::set("DENIA_ADMIN_TOKEN", "x".repeat(64));
    let _key_file = EnvGuard::set("DENIA_AGE_KEY_FILE", "/nonexistent/denia-test/age.key");
    let _cd = EnvGuard::set("DENIA_CONTROL_DOMAIN", "Denia.Example.COM");
    let cfg = AppConfig::from_env().expect("valid control domain");
    assert_eq!(cfg.control_domain.as_deref(), Some("denia.example.com"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `CARGO_TARGET_DIR=/tmp/denia-verify cargo test --lib config::tests::invalid_control_domain_is_rejected config::tests::valid_control_domain_is_lowercased`
Expected: FAIL (variant `InvalidControlDomain` does not exist / not lowercased).

- [ ] **Step 3: Implement**

In `src/config.rs`, add the error variant to `ConfigError`:

```rust
    #[error("invalid DENIA_CONTROL_DOMAIN: {0}")]
    InvalidControlDomain(String),
```

In `from_env`, replace the `control_domain` binding (currently lines ~454-456) with a validated/normalized version:

```rust
        let control_domain = env::var("DENIA_CONTROL_DOMAIN")
            .ok()
            .or_else(|| file_cfg.control_domain.clone())
            .filter(|v| !v.trim().is_empty())
            .map(|d| {
                crate::ingress::pingora::state::validate_domain(&d)
                    .map_err(|e| ConfigError::InvalidControlDomain(e.to_string()))
            })
            .transpose()?;
```

(`validate_domain` lowercases and rejects whitespace/control/wildcard/non-ASCII; it returns the normalized `String`.)

- [ ] **Step 4: Run tests to verify they pass**

Run: `CARGO_TARGET_DIR=/tmp/denia-verify cargo test --lib config::tests::invalid_control_domain_is_rejected config::tests::valid_control_domain_is_lowercased`
Expected: PASS (2 passed).

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat(config): validate and normalize control_domain at load"
```

---

### Task 2: `X-Forwarded-For` overwrite in the proxy (rate-limiter correctness)

**Files:**
- Modify: `src/ingress/pingora/proxy.rs` (add pure `forwarded_for` helper + `upstream_request_filter`)
- Test: `src/ingress/pingora/proxy.rs` (`mod classify_tests`)

- [ ] **Step 1: Write the failing test**

Add to `mod classify_tests` in `proxy.rs`:

```rust
#[test]
fn forwarded_for_uses_client_ip_only() {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    let client = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7)), 54321);
    assert_eq!(forwarded_for(Some(client)).as_deref(), Some("203.0.113.7"));
    assert_eq!(forwarded_for(None), None);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `CARGO_TARGET_DIR=/tmp/denia-verify cargo test --lib ingress::pingora::proxy::classify_tests::forwarded_for_uses_client_ip_only`
Expected: FAIL (`forwarded_for` not found).

- [ ] **Step 3: Implement**

Add the pure helper near `strip_port` in `proxy.rs`:

```rust
/// The `X-Forwarded-For` value for a proxied request: the client IP only (no
/// port). Overwriting with this (not appending) prevents a downstream client
/// from spoofing the value the loopback-trusting rate limiter keys on.
fn forwarded_for(client: Option<std::net::SocketAddr>) -> Option<String> {
    client.map(|addr| addr.ip().to_string())
}
```

Add the trait method inside `impl ProxyHttp for DeniaProxy` (after `upstream_peer`):

```rust
    async fn upstream_request_filter(
        &self,
        session: &mut Session,
        upstream_request: &mut pingora::http::RequestHeader,
        _ctx: &mut Self::CTX,
    ) -> pingora::Result<()> {
        let client = session.client_addr().and_then(|a| a.as_inet()).copied();
        if let Some(value) = forwarded_for(client) {
            // Overwrite (not append): strip any client-supplied X-Forwarded-For
            // so the rate-limit key cannot be spoofed.
            let _ = upstream_request.insert_header("X-Forwarded-For", &value);
        }
        Ok(())
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `CARGO_TARGET_DIR=/tmp/denia-verify cargo test --lib ingress::pingora::proxy::classify_tests::forwarded_for_uses_client_ip_only`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/ingress/pingora/proxy.rs
git commit -m "feat(ingress): overwrite X-Forwarded-For with real client IP"
```

---

### Task 3: Thread `control_domain`/`control_tls` into ingress config + proxy, with routing

**Files:**
- Modify: `src/ingress/pingora/server.rs` (`IngressServerConfig` fields + `from_ports` signature + `test_defaults` + pass to `DeniaProxy`)
- Modify: `src/ingress/pingora/proxy.rs` (`DeniaProxy` fields; `http`/`https` ctors; pure `is_control_host` + `control_tls_for_host`; `request_filter` + `upstream_peer` branches)
- Test: both files' test modules

- [ ] **Step 1: Write the failing tests**

In `proxy.rs` `mod classify_tests`:

```rust
#[test]
fn is_control_host_matches_exact_lowercased() {
    assert!(is_control_host("denia.example.com", Some("denia.example.com")));
    assert!(!is_control_host("other.example.com", Some("denia.example.com")));
    assert!(!is_control_host("denia.example.com", None));
}

#[test]
fn control_tls_for_host_overrides_route_lookup() {
    // Control host with tls -> Some(true) (so classify_port80 redirects).
    assert_eq!(
        control_tls_for_host("denia.example.com", Some("denia.example.com"), true, None),
        Some(true)
    );
    // Control host without tls -> Some(false) (passthrough to backend on :80).
    assert_eq!(
        control_tls_for_host("denia.example.com", Some("denia.example.com"), false, None),
        Some(false)
    );
    // Non-control host falls back to the route's tls flag.
    assert_eq!(
        control_tls_for_host("svc.example.com", Some("denia.example.com"), true, Some(true)),
        Some(true)
    );
    assert_eq!(
        control_tls_for_host("nope.example.com", Some("denia.example.com"), true, None),
        None
    );
}
```

In `server.rs` `mod tests`:

```rust
#[test]
fn from_ports_carries_control_domain() {
    let backend = SocketAddr::from(([127, 0, 0, 1], 7180));
    let cfg = IngressServerConfig::from_ports(80, 443, backend, Some("denia.example.com".into()), true);
    assert_eq!(cfg.control_domain.as_deref(), Some("denia.example.com"));
    assert!(cfg.control_tls);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `CARGO_TARGET_DIR=/tmp/denia-verify cargo test --lib ingress::pingora`
Expected: FAIL to compile (`is_control_host`/`control_tls_for_host` missing; `from_ports` arity).

- [ ] **Step 3: Implement**

**`proxy.rs`** — add fields to `DeniaProxy`:

```rust
pub struct DeniaProxy {
    state: Arc<IngressState>,
    control_backend: SocketAddr,
    is_http: bool,
    control_domain: Option<String>,
    control_tls: bool,
}
```

Update both constructors:

```rust
    pub fn http(
        state: Arc<IngressState>,
        control_backend: SocketAddr,
        control_domain: Option<String>,
        control_tls: bool,
    ) -> Self {
        Self { state, control_backend, is_http: true, control_domain, control_tls }
    }

    pub fn https(
        state: Arc<IngressState>,
        control_backend: SocketAddr,
        control_domain: Option<String>,
        control_tls: bool,
    ) -> Self {
        Self { state, control_backend, is_http: false, control_domain, control_tls }
    }
```

Add pure helpers near `classify_port80`:

```rust
/// Whether `host` is the configured control domain (exact, already-lowercased
/// match; both sides are lowercased at their sources — `request_host` lowercases
/// the request Host, config lowercases `control_domain`).
pub fn is_control_host(host: &str, control_domain: Option<&str>) -> bool {
    control_domain == Some(host)
}

/// The effective `tls_for_host` fed to [`classify_port80`]: the control domain
/// uses `control_tls`; everything else uses its route's tls flag.
pub fn control_tls_for_host(
    host: &str,
    control_domain: Option<&str>,
    control_tls: bool,
    route_tls: Option<bool>,
) -> Option<bool> {
    if is_control_host(host, control_domain) {
        Some(control_tls)
    } else {
        route_tls
    }
}
```

In `request_filter`, replace the `tls_for_host` computation:

```rust
        let route_tls = self.state.routes().resolve(&host).map(|r| r.tls);
        let tls_for_host = control_tls_for_host(
            &host,
            self.control_domain.as_deref(),
            self.control_tls,
            route_tls,
        );
```

In `upstream_peer`, add the control-domain branch immediately after the `ctx.to_control_backend` block and before workload resolution:

```rust
        let host = request_host(session);
        if is_control_host(&host, self.control_domain.as_deref()) {
            return Ok(Box::new(HttpPeer::new(
                self.control_backend,
                false,
                self.control_backend.ip().to_string(),
            )));
        }
```

(Remove the now-duplicate `let host = request_host(session);` that previously preceded route resolution.)

**`server.rs`** — extend `IngressServerConfig`:

```rust
pub struct IngressServerConfig {
    pub http_addr: SocketAddr,
    pub https_addr: SocketAddr,
    pub control_backend: SocketAddr,
    pub control_domain: Option<String>,
    pub control_tls: bool,
}
```

Update `from_ports`:

```rust
    pub fn from_ports(
        http_port: u16,
        https_port: u16,
        control_backend: SocketAddr,
        control_domain: Option<String>,
        control_tls: bool,
    ) -> Self {
        Self {
            http_addr: SocketAddr::from(([0, 0, 0, 0], http_port)),
            https_addr: SocketAddr::from(([0, 0, 0, 0], https_port)),
            control_backend,
            control_domain,
            control_tls,
        }
    }
```

Update `test_defaults` to set `control_domain: None, control_tls: false`.

In `build_server`, pass the fields to both proxies:

```rust
    let mut http_service = http_proxy_service(
        &conf,
        DeniaProxy::http(state.clone(), cfg.control_backend, cfg.control_domain.clone(), cfg.control_tls),
    );
    ...
    let mut https_service = http_proxy_service(
        &conf,
        DeniaProxy::https(state.clone(), cfg.control_backend, cfg.control_domain.clone(), cfg.control_tls),
    );
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `CARGO_TARGET_DIR=/tmp/denia-verify cargo test --lib ingress::pingora`
Expected: PASS (existing `classify_tests` still pass; new tests pass).

- [ ] **Step 5: Commit**

```bash
git add src/ingress/pingora/proxy.rs src/ingress/pingora/server.rs
git commit -m "feat(ingress): route control_domain to the control backend over TLS"
```

---

### Task 4: Issue/renew the control-domain cert at boot

**Files:**
- Modify: `src/daemon.rs` (pass new fields to `from_ports`; add a control-domain issuance helper + call it in the ACME task)
- Test: `src/daemon.rs` (`mod tests` — add if absent)

- [ ] **Step 1: Write the failing test**

Add a `#[cfg(test)] mod tests` to `daemon.rs` (or extend it):

```rust
#[cfg(test)]
mod tests {
    use super::control_domain_to_issue;

    #[test]
    fn control_domain_issued_only_when_tls_enabled() {
        assert_eq!(control_domain_to_issue(Some("denia.example.com"), true), Some("denia.example.com"));
        assert_eq!(control_domain_to_issue(Some("denia.example.com"), false), None);
        assert_eq!(control_domain_to_issue(None, true), None);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `CARGO_TARGET_DIR=/tmp/denia-verify cargo test --lib daemon::tests::control_domain_issued_only_when_tls_enabled`
Expected: FAIL (`control_domain_to_issue` not found).

- [ ] **Step 3: Implement**

Add the pure helper to `daemon.rs` (near `issue_missing_certs`):

```rust
/// The control domain to ACME-issue, if TLS is enabled for it. Renewal is
/// automatic once the cert is in the store (`select_renewals` covers any SNI);
/// only the initial issuance needs this branch (the control domain has no
/// service row, so `issue_missing_certs` does not cover it).
fn control_domain_to_issue(control_domain: Option<&str>, control_tls: bool) -> Option<&str> {
    if control_tls { control_domain } else { None }
}
```

Update the `IngressServerConfig::from_ports` call (line ~144-145):

```rust
    let pingora_cfg = IngressServerConfig::from_ports(
        config.http_port,
        config.https_port,
        config.bind_addr,
        config.control_domain.clone(),
        config.control_tls,
    );
```

In the `acme_task` closure setup, clone the control-domain config before the closure:

```rust
        let control_domain = config.control_domain.clone();
        let control_tls = config.control_tls;
```

(add these to the `let` bindings just above `let handle = tokio::spawn(async move { ... })`, and they will be moved into the closure).

Inside the spawned async block, after the initial `issue_missing_certs(...)` call AND inside the renewal tick branch after its `issue_missing_certs(...)` call, add:

```rust
                if let Some(cd) = control_domain_to_issue(control_domain.as_deref(), control_tls)
                    && ingress.certs().get(cd).is_none()
                {
                    reissue(&driver, &ingress, &tls_dir, cd).await;
                }
```

(Factor into a tiny local async closure if preferred to avoid duplication; both the initial pass and the tick need it.)

- [ ] **Step 4: Run test + build to verify**

Run: `CARGO_TARGET_DIR=/tmp/denia-verify cargo test --lib daemon::tests::control_domain_issued_only_when_tls_enabled`
Then: `CARGO_TARGET_DIR=/tmp/denia-verify cargo build`
Expected: test PASS; build OK.

- [ ] **Step 5: Commit**

```bash
git add src/daemon.rs
git commit -m "feat(ingress): issue and renew the control-domain TLS cert"
```

---

### Task 5: Guardrail — reject a service domain equal to `control_domain`

**Files:**
- Modify: `src/api/domains.rs` (`create_service_domain`, `verify_service_domain`; pure `is_reserved_control_hostname`)
- Test: `src/api/domains.rs` (`mod tests`)

- [ ] **Step 1: Write the failing test**

Add to `domains.rs` `mod tests`:

```rust
#[test]
fn control_hostname_is_reserved() {
    use super::is_reserved_control_hostname;
    assert!(is_reserved_control_hostname("denia.example.com", Some("denia.example.com")));
    assert!(!is_reserved_control_hostname("svc.example.com", Some("denia.example.com")));
    assert!(!is_reserved_control_hostname("denia.example.com", None));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `CARGO_TARGET_DIR=/tmp/denia-verify cargo test --lib api::domains::tests::control_hostname_is_reserved`
Expected: FAIL (`is_reserved_control_hostname` not found).

- [ ] **Step 3: Implement**

Add the pure helper to `domains.rs`:

```rust
/// A service may not claim the control-plane hostname. Both sides are
/// lowercased at their sources (`validate_hostname` and config validation).
pub(crate) fn is_reserved_control_hostname(hostname: &str, control_domain: Option<&str>) -> bool {
    control_domain == Some(hostname)
}
```

In `create_service_domain`, right after the `validate_hostname` line (~102):

```rust
    if is_reserved_control_hostname(&hostname, state.config.control_domain.as_deref()) {
        return Err(ApiError::Conflict("hostname is reserved for the control plane".into()));
    }
```

In `verify_service_domain`, after fetching `d` and the `service_id` check (~160), before starting verification:

```rust
    if is_reserved_control_hostname(&d.hostname, state.config.control_domain.as_deref()) {
        return Err(ApiError::Conflict("hostname is reserved for the control plane".into()));
    }
```

- [ ] **Step 4: Run test + build**

Run: `CARGO_TARGET_DIR=/tmp/denia-verify cargo test --lib api::domains::tests::control_hostname_is_reserved`
Then: `CARGO_TARGET_DIR=/tmp/denia-verify cargo build`
Expected: PASS; build OK.

- [ ] **Step 5: Commit**

```bash
git add src/api/domains.rs
git commit -m "feat(api): reject service domains that collide with control_domain"
```

---

### Task 6: Rate-limiter comment cleanup + client-IP test

**Files:**
- Modify: `src/rate_limit.rs` (comment in `extract_client_ip`; add test)
- Test: `src/rate_limit.rs` (`mod tests` — add if absent)

- [ ] **Step 1: Write the failing test**

Add to `rate_limit.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::ConnectInfo;
    use std::net::SocketAddr;

    fn req_with(peer: &str, xff: Option<&str>) -> Request {
        let mut b = Request::builder().uri("/");
        if let Some(v) = xff {
            b = b.header("x-forwarded-for", v);
        }
        let mut req = b.body(axum::body::Body::empty()).unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
        req
    }

    #[test]
    fn loopback_peer_trusts_forwarded_for() {
        let req = req_with("127.0.0.1:5000", Some("203.0.113.9, 10.0.0.1"));
        assert_eq!(extract_client_ip(&req), "203.0.113.9");
    }

    #[test]
    fn non_loopback_peer_ignores_forwarded_for() {
        let req = req_with("198.51.100.4:5000", Some("203.0.113.9"));
        assert_eq!(extract_client_ip(&req), "198.51.100.4");
    }
}
```

- [ ] **Step 2: Run test to verify it fails/compiles**

Run: `CARGO_TARGET_DIR=/tmp/denia-verify cargo test --lib rate_limit::tests`
Expected: FAIL only if `extract_client_ip` is private to the module — it is `fn` (module-private) so the in-module test compiles; this test should pass immediately once added (it documents existing behavior the feature relies on). If it fails, fix the test, not the code.

- [ ] **Step 3: Implement (comment cleanup only)**

In `extract_client_ip`, update the stale comment:

```rust
    // Only trust X-Forwarded-For when the TCP peer is loopback (our own
    // in-process Pingora ingress, which overwrites the header with the real
    // client IP). A directly-connected client could otherwise spoof the header
    // and evade or poison the rate-limit buckets.
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `CARGO_TARGET_DIR=/tmp/denia-verify cargo test --lib rate_limit::tests`
Expected: PASS (2 passed).

- [ ] **Step 5: Commit**

```bash
git add src/rate_limit.rs
git commit -m "test(rate-limit): cover loopback X-Forwarded-For trust; refresh comment"
```

---

### Task 7: ADR for public control-plane exposure

**Files:**
- Create: `docs/adr/035-control-domain-ingress.md`
- Modify: `docs/adr/README.md` (add to the index)

- [ ] **Step 1: Write the ADR**

Create `docs/adr/035-control-domain-ingress.md`:

```markdown
# ADR-035: Control Domain Over Ingress

- **Status**: Accepted
- **Date**: 2026-06-03
- **Extends**: ADR-020 (Pingora ingress). **References**: ADR-004 (web console),
  ADR-008 (RBAC), ADR-031 (hosted registry), ADR-013 (domain verification).

## Context

The control plane (console `/`, management API `/v1`, hosted registry `/v2`,
`/healthz`) is one axum server bound to `bind_addr` (default `127.0.0.1:7180`),
reachable only on loopback. Operators cannot serve the console on a domain, and
`docker push` cannot authenticate over the loopback HTTP endpoint (OCI clients
refuse Basic auth over plaintext HTTP). The `control_domain`/`control_tls`
config fields existed but were never routed.

## Decision

When `control_domain` is set, the in-process Pingora ingress serves it on
`:443` (ACME TLS) and reverse-proxies to the loopback control backend, exposing
console + `/v1` + `/v2` on that hostname. `:7180` stays loopback-bound.

- Routing: `DeniaProxy` matches `Host == control_domain` ahead of workload
  routing and dials the control backend via `HttpPeer::new`. On `:80` the
  control domain redirects to `https://` when `control_tls` (ACME challenge
  interception still wins first).
- TLS: the control domain is issued/renewed by the existing ACME orchestration
  as a distinct branch (it has no service row); HTTP-01 proves DNS control.
- Client IP: the proxy overwrites `X-Forwarded-For` with the real downstream
  peer, so the loopback-trusting per-IP rate limiter (login 5/min, admin
  300/min) keys on the true client IP. Overwrite (not append) prevents
  spoofing.
- Guardrail: a workload service domain cannot equal `control_domain`.

## Consequences

- The control plane becomes internet-facing when `control_domain` is set.
  Mitigations: argon2id passwords, 64-hex bearer/API tokens, per-IP login
  throttle. `/v2` registry auth stays unthrottled (tokens unbruteforceable).
- `docker push <control_domain>/<project>/<service>` works natively over HTTPS.
- No second proxy; ADR-020's "Denia owns `:80`/`:443`" stands.
- `control_tls=false` with `control_domain` set serves the domain on `:80`
  plaintext only (local/testing).

## Alternatives Considered

- External reverse proxy / second TLS terminator: rejected — ADR-020 reserves
  `:80`/`:443` for Denia's ingress.
- Binding the control backend directly to `:443`: rejected — conflicts with the
  ingress listener.
- IP allowlist / distributed-brute-force backstop: deferred (YAGNI); per-IP
  limits only.

## References

- `docs/superpowers/specs/2026-06-03-control-domain-ingress-design.md`
- `docs/superpowers/plans/2026-06-03-control-domain-ingress.md`
- ADR-020, ADR-004, ADR-008, ADR-031, ADR-013.
```

- [ ] **Step 2: Add to the ADR index**

Add the ADR-035 row to the table/list in `docs/adr/README.md` following the existing format.

- [ ] **Step 3: Commit**

```bash
git add docs/adr/035-control-domain-ingress.md docs/adr/README.md
git commit -m "docs(adr): ADR-035 control domain over ingress"
```

---

### Task 8: Full verification

**Files:** none (verification only)

- [ ] **Step 1: Format**

Run: `cargo fmt --all`
Then stage/commit if anything changed: `git commit -am "style: cargo fmt"` (skip if clean).

- [ ] **Step 2: Build**

Run: `CARGO_TARGET_DIR=/tmp/denia-verify cargo build`
Expected: `Finished` with 0 errors.

- [ ] **Step 3: Test suite**

Run: `CARGO_TARGET_DIR=/tmp/denia-verify cargo test`
Expected: all pass (privileged + ACME net tests remain gated/ignored).

- [ ] **Step 4: Clippy**

Run: `CARGO_TARGET_DIR=/tmp/denia-verify cargo clippy --all-targets --all-features`
Expected: no warnings introduced by this change (fix any that are).

- [ ] **Step 5: Manual smoke (optional, requires a real domain + DNS)**

With `control_domain` + `control_tls=true` + `acme_email` set, A record → host, `:80`/`:443` open:
- `https://<control_domain>/healthz` → 200.
- `docker login <control_domain> -u denia -p <API_TOKEN>` → Login Succeeded; `docker push <control_domain>/<project>/<service>` → succeeds.
- Repeated bad logins from one IP → 429 after 5/min.

- [ ] **Step 6: Final commit (if fmt/clippy required edits)**

```bash
git commit -am "chore: fmt + clippy for control-domain ingress" || true
```

---

## Notes for the Implementer

- **TDD discipline:** write each test, see it fail, implement minimally, see it pass, commit. Pure helpers (`forwarded_for`, `is_control_host`, `control_tls_for_host`, `control_domain_to_issue`, `is_reserved_control_hostname`) are the unit-test seams; the async proxy/daemon wiring is exercised by build + the gated e2e.
- **No frontend changes** (AGENTS.md). The SPA is served as-is via the existing fallback; it is same-origin under the control domain.
- **Secrets discipline:** never log key authorizations, private keys, tokens, or credentials (AGENTS.md).
- **Host comparison is exact + lowercased** on both sides; do not add `www.`/port normalization beyond what `request_host` (proxy) and `validate_domain`/`validate_hostname` already do.
- **Renewal is automatic** once the control-domain cert is in the store; only initial issuance needs the explicit branch (Task 4).
