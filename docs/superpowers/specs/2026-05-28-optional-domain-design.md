# Optional Domain at Service Creation

**Date:** 2026-05-28
**Status:** Approved (design)
**Scope:** small — backend validation relaxation + API normalize + frontend form

## Problem

Service creation currently requires at least one domain. Dokploy lets you create
a service with no domain and attach one later. We want the same: create a
domainless service that runs but receives no public ingress until a domain is
added via the existing `POST /v1/services/{id}/domains` flow.

## Goal / non-goals

- **Goal:** `domains` becomes optional (empty `Vec`) at service create/update.
- **Goal:** a domainless service is not routable (Dokploy-style) — no ingress
  entry until a domain is added and verified.
- **Non-goal:** auto-generated subdomains, internal-only routing, ingress
  changes, ADR, wire-schema changes.

## Current behavior (grounded)

- `src/domain/service.rs:249-251` — `validate()` returns
  `DomainError::MissingDomain` when `domains.is_empty()`.
- `src/api/services.rs:79` `put_service` — single create/update path
  (`POST /v1/services`). Deserializes `ServiceConfig`, calls `validate()`
  (line 89), then `require_acme_email(service.tls_enabled)` (line 105).
- `web/src/components/ServiceForm.tsx` — `valid` requires
  `parsedDomains.length > 0` (line 133); domains label marked `req` /
  `aria-required` (237, 243); TLS checkbox always enabled (530-538).
- `src/deploy/routes.rs` — only routes services with verified domains, so a
  domainless service already produces no route. No ingress change needed.

## Decisions

1. **No-domain service = not routable** (Dokploy-style). Free, since routing
   already skips services without verified domains.
2. **TLS without domain → force off.** ACME is gated to verified domains, so
   `tls_enabled` is meaningless with zero domains.
3. **TLS coercion location: normalize in API handler.** `validate(&self)` cannot
   mutate, so coerce `tls_enabled = false` in `put_service` before `validate()`
   and `require_acme_email`. Frontend disabling the toggle is the primary UX;
   handler coercion is defense-in-depth. Silent — no error, no friction.

## Changes

### 1. `src/domain/service.rs` — relax validate()

Delete the empty-domains guard (lines 249-251):

```rust
if self.domains.is_empty() {
    return Err(DomainError::MissingDomain);
}
```

Keep the per-domain `validate_hostname` loop (validates any domains present).
If `DomainError::MissingDomain` is unused after this, remove the variant
(grep/verify during implementation).

### 2. `src/api/services.rs` — normalize in `put_service`

After `ensure_role` (line 84), before `validate()` (line 88):

```rust
if service.domains.is_empty() {
    service.tls_enabled = false; // ACME can't run without a verified domain
}
```

Coercing before `require_acme_email(service.tls_enabled)` (line 105) means a
domainless service never trips the ACME-email gate.

### 3. `web/src/components/ServiceForm.tsx`

- `valid` (line 133): remove `parsedDomains.length > 0 &&`.
- `missing` (line 141): remove `if (parsedDomains.length === 0) missing.push('domains')`.
- Domains label (line 237): `kicker req` → `kicker`; drop `aria-required="true"`
  (line 243); indicate optional (e.g. label text or placeholder).
- TLS checkbox (lines 530-538): `disabled` when `parsedDomains.length === 0`;
  force unchecked in that state; hint "add a domain to enable TLS".
  (Submitted `tls_enabled` already follows `tlsEnabled` state.)

## Testing

**Backend (`src/api/services.rs` tests):**
- Create service with `domains: []` → 200; persisted config has empty domains.
- Create with `domains: []` and `tls_enabled: true` → 200 and stored
  `tls_enabled == false` (coerced); does not hit the ACME-email 400.
- Existing tests with domains still pass (regression).

**Frontend (`ServiceForm` tests, if present):**
- Form submittable with blank domains field (button enabled, `domains: []`).
- TLS toggle disabled when domains blank.

## Verification commands

- `cargo build`
- `cargo test`
- `cargo fmt --all`
- `cargo clippy --all-targets --all-features`
- `cd web && pnpm typecheck && pnpm test`

## Out of scope

- ADR (no architecture/ingress/runtime change).
- Wire schema (`domains: Array(String)` already accepts `[]`).
- Add-domain-later UI on service detail page (already exists).
