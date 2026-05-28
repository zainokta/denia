# Optional Domain at Service Creation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Allow creating a service with zero domains (Dokploy-style); a domainless service runs but is not routable until a domain is added later.

**Architecture:** Relax `ServiceConfig::validate()` to accept empty `domains`. In the `put_service` API handler, coerce `tls_enabled = false` when `domains` is empty (ACME can't run without a verified domain). Relax the frontend `ServiceForm` so the domains field is optional and the TLS toggle disables when no domain is present. Ingress/routing needs no change — `deploy/routes.rs` already skips services without verified domains.

**Tech Stack:** Rust 2024, axum, SQLite control plane; frontend TanStack Start + React 19 + TypeScript + Vitest.

**Spec:** `docs/superpowers/specs/2026-05-28-optional-domain-design.md`

---

## File Structure

- `src/domain/service.rs` — `ServiceConfig::validate()`; possibly `DomainError` enum (remove `MissingDomain` if unused).
- `src/api/services.rs` — `put_service` handler; add normalize + tests.
- `web/src/components/ServiceForm.tsx` — form validation, domains label, TLS toggle.

---

## Task 1: Relax backend domain validation

**Files:**
- Modify: `src/domain/service.rs` (`validate()`, ~lines 247-271; `DomainError` enum)
- Test: `src/domain/service.rs` (existing `#[cfg(test)] mod tests`, or wherever `ServiceConfig` unit tests live)

- [ ] **Step 1: Write failing test**

Add a unit test near the existing `ServiceConfig` tests. Builds a config with empty domains and asserts `validate()` is `Ok`.

```rust
#[test]
fn validate_allows_empty_domains() {
    let cfg = ServiceConfig::new(
        uuid::Uuid::now_v7(),
        "no-domain-svc",
        vec![], // empty domains
        ServiceSource::ExternalImage(ExternalImageSource {
            image: "nginx".into(),
            credential: None,
            registry_id: None,
            image_ref: None,
        }),
        80,
        HealthCheck::new("/health", 5),
        None,
        Vec::new(),
    );
    assert!(cfg.is_ok(), "empty domains must be valid: {cfg:?}");
}
```

> Note: `ServiceConfig::new` calls `validate()` internally (service.rs:241). If
> `new`'s signature differs from the snippet, match the real signature — copy an
> existing `ServiceConfig::new(...)` call from `src/api/services.rs` tests.

- [ ] **Step 2: Run test, verify it fails**

Run: `cargo test validate_allows_empty_domains`
Expected: FAIL — `new` returns `Err(MissingDomain)`.

- [ ] **Step 3: Remove the empty-domains guard**

In `ServiceConfig::validate()` (src/domain/service.rs), delete:

```rust
if self.domains.is_empty() {
    return Err(DomainError::MissingDomain);
}
```

Keep the per-domain `validate_hostname` loop that follows.

- [ ] **Step 4: Remove `MissingDomain` variant if unused**

Search for remaining uses: `grep -rn "MissingDomain" src/` (the index/codedb may be stale — use the actual grep result). If zero references remain, delete the `MissingDomain` variant from the `DomainError` enum. If any reference remains, leave it.

- [ ] **Step 5: Run test, verify it passes**

Run: `cargo test validate_allows_empty_domains`
Expected: PASS

- [ ] **Step 6: Build + commit**

```bash
cargo build
git add src/domain/service.rs
git commit -m "feat(services): allow empty domains in ServiceConfig validation"
```

---

## Task 2: Coerce TLS off for domainless services in the API handler

**Files:**
- Modify: `src/api/services.rs` (`put_service`, after line 84 / before `validate()` at line 88)
- Test: `src/api/services.rs` (`#[cfg(test)] mod tests`)

- [ ] **Step 1: Write failing test**

Add to the services tests module. Mirror `put_service_with_tls_and_no_acme_email_returns_400` but with empty domains — assert 200 and that the stored config has `tls_enabled == false`.

```rust
#[tokio::test]
async fn put_service_no_domain_coerces_tls_off() {
    use crate::domain::{ExternalImageSource, HealthCheck, Project, ServiceConfig, ServiceSource};
    let state = test_state(); // acme_email is None in for_test
    let project = Project::new("team-nodomain", None).unwrap();
    state.projects.put_project(project.clone()).unwrap();

    let body = serde_json::to_vec(&serde_json::json!({
        "project_id": project.id,
        "name": "nodomain",
        "domains": [],
        "source": { "type": "external_image", "image": "nginx", "credential": null },
        "internal_port": 80,
        "health_check": { "path": "/health", "timeout_seconds": 5 },
        "tls_enabled": true,
    })).unwrap();

    let resp = build_router(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/services")
                .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                .header("Content-Type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK, "no-domain service must be accepted");
    let cfg: ServiceConfig = serde_json::from_str(&body_string(resp).await).unwrap();
    assert!(cfg.domains.is_empty());
    assert!(!cfg.tls_enabled, "tls must be coerced off when no domain");
}
```

> Verify `ServiceConfig` exposes a public `tls_enabled` field for the assertion
> (it does per spec). Adjust the JSON if `source` requires more fields — copy
> from the existing `service_create_body` helper at services.rs:551.

- [ ] **Step 2: Run test, verify it fails**

Run: `cargo test put_service_no_domain_coerces_tls_off`
Expected: FAIL — currently returns 400 (`DENIA_ACME_EMAIL` gate) because
`tls_enabled` is still true.

- [ ] **Step 3: Add the coercion**

In `put_service` (src/api/services.rs), immediately after the `ensure_role(...)` call (line 84) and before `service.validate()` (line 88):

```rust
    // A service with no domain cannot have ACME-issued TLS (ACME is gated to
    // verified domains), so TLS is meaningless here — coerce it off rather than
    // tripping the ACME-email requirement below.
    if service.domains.is_empty() {
        service.tls_enabled = false;
    }
```

- [ ] **Step 4: Run test, verify it passes**

Run: `cargo test put_service_no_domain_coerces_tls_off`
Expected: PASS

- [ ] **Step 5: Run the services test module (regression)**

Run: `cargo test --lib api::services`
Expected: all PASS (existing domain-bearing tests unaffected).

- [ ] **Step 6: Commit**

```bash
git add src/api/services.rs
git commit -m "feat(services): coerce tls_enabled off when service has no domain"
```

---

## Task 3: Make the domains field optional in ServiceForm

**Files:**
- Modify: `web/src/components/ServiceForm.tsx`
- Test: `web/src/components/ServiceForm.test.tsx` (create if absent; check first)

- [ ] **Step 1: Check for an existing test file**

Run: `ls web/src/components/ServiceForm.test.tsx 2>/dev/null || echo missing`
If present, add cases there. If missing AND no sibling component tests exist as a
pattern, skip the automated frontend test and rely on `pnpm typecheck` + manual
browser verification (Step 6) — note this in the commit.

- [ ] **Step 2: Write failing test (if test file pattern exists)**

Render `ServiceForm`, fill name + port + image, leave domains blank, assert the
submit button is enabled and `onSubmit` receives `domains: []`. Also assert the
TLS checkbox is disabled while domains is blank.

```tsx
// pseudostructure — match existing test util/render patterns in web/
it('submits with empty domains and disables TLS when no domain', async () => {
  const onSubmit = vi.fn()
  render(<ServiceForm projects={[{ id: 'p1', name: 'proj' }]} onSubmit={onSubmit} />)
  // fill name, internal port, image (external_image is default)
  // leave domains blank
  // expect submit button enabled
  // expect TLS checkbox disabled
  // click submit -> onSubmit called with domains: []
})
```

- [ ] **Step 3: Run test, verify it fails**

Run: `cd web && pnpm test ServiceForm`
Expected: FAIL — submit disabled (domains required), TLS not disabled.

- [ ] **Step 4: Edit ServiceForm.tsx**

1. `valid` (line ~131-136): remove the `parsedDomains.length > 0 &&` line.
2. `missing` (line ~141): remove `if (parsedDomains.length === 0) missing.push('domains')`.
3. Domains label (line ~237): change `className="kicker req"` to `className="kicker"`; on the input (line ~243) remove `aria-required="true"`. Indicate optional, e.g. label text `domains (optional)` or placeholder.
4. TLS checkbox block (lines ~530-538): add `disabled={parsedDomains.length === 0}` to the `<input type="checkbox">`, and force unchecked when disabled — set `checked={tlsEnabled && parsedDomains.length > 0}`. Add a muted hint shown when `parsedDomains.length === 0`, e.g. "add a domain to enable TLS".

> `tls_enabled` in the submitted body (line ~178) already reads `tlsEnabled`
> state. With the `checked` expression above driving display only, also guard the
> submit payload: `tls_enabled: parsedDomains.length > 0 && tlsEnabled`. Backend
> coerces too (Task 2) — this keeps the wire value honest.

- [ ] **Step 5: Run test + typecheck, verify pass**

Run: `cd web && pnpm test ServiceForm && pnpm typecheck`
Expected: PASS / no type errors.

- [ ] **Step 6: Manual browser verification**

Run: `cd web && pnpm build && cd .. && cargo run` (serves UI + API on `DENIA_BIND_ADDR`, default `127.0.0.1:7180`). In the browser:
- Open the create-service form. Leave domains blank → submit button enabled.
- TLS toggle disabled while domains blank; hint visible.
- Type a domain → TLS toggle enables.
- Create with no domain → service created, no ingress route.

- [ ] **Step 7: Commit**

```bash
git add web/src/components/ServiceForm.tsx web/src/components/ServiceForm.test.tsx
git commit -m "feat(web): make domain optional in service form, gate TLS on domain"
```

---

## Task 4: Full verification + index refresh

- [ ] **Step 1: Backend gates**

```bash
cargo build
cargo test
cargo fmt --all
cargo clippy --all-targets --all-features
```
Expected: build clean, tests pass, no clippy warnings introduced.

- [ ] **Step 2: Frontend gates**

```bash
cd web && pnpm typecheck && pnpm test
```
Expected: no type errors, tests pass.

- [ ] **Step 3: Refresh GitNexus index (post-commit hook may handle this)**

```bash
npx gitnexus analyze
```

- [ ] **Step 4: Report verification commands + results before finishing** (per project CLAUDE.md).

---

## Notes

- DRY/YAGNI: no ADR, no wire-schema change, no ingress change — all out of scope per spec.
- Backend coercion + frontend gating are intentionally redundant (defense-in-depth): the form is primary UX, the handler is the trust boundary.
