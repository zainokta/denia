# Domain Support With HTTP File Verification

- **Status**: Proposed
- **Date**: 2026-05-25
- **Related ADRs**: 001 (initial backend), 007 (ingress + TLS), 008 (RBAC)

## Summary

Add per-service domain management with Dokploy-style HTTP file verification.
Domains become a first-class entity stored in SQLite. A domain must be
verified before Traefik routes it. Verification works via a global path router
(`/.well-known/denia-challenge/<token>`) served by the Denia control plane and
exposed by Traefik on the `web` entrypoint for any host. Verification is
operator-triggered (manual), not automatic.

## Motivation

`ServiceConfig.domains: Vec<String>` is a flat list with no ownership proof.
Any operator can attach any hostname to a service, and Traefik renders it
immediately — including issuing an ACME cert for a host the operator may not
control. We need a verification step before traffic and certs.

## Decisions

### Verification method

HTTP file challenge only. DNS A record is already required for Traefik to
receive traffic, so requiring the same prerequisite for verification adds no
operator burden. No DNS TXT path. No mixed-mode picker.

### Routing of unverified domains

Pending and failed domains are blocked entirely from Traefik. No plain HTTP,
no TLS, no ACME request. They appear in API responses with status only.

### Challenge endpoint

A single global Traefik router catches `PathPrefix(/.well-known/denia-challenge)`
for any host on the `web` entrypoint with priority `1000`. It forwards to the
Denia control backend (reusing `IngressRenderOptions.control_backend_addr`).
Denia serves the challenge file from a public route that does not require
auth. Token lookup hits the same SQLite table.

### Data model

New SQLite table `service_domains`:

```sql
CREATE TABLE service_domains (
    id BLOB PRIMARY KEY,
    service_id BLOB NOT NULL REFERENCES services(id) ON DELETE CASCADE,
    hostname TEXT NOT NULL UNIQUE,
    status TEXT NOT NULL,
    challenge_token TEXT NOT NULL,
    verified_at INTEGER,
    last_check_at INTEGER,
    last_error TEXT,
    created_at INTEGER NOT NULL
);
CREATE INDEX idx_service_domains_service ON service_domains(service_id);
CREATE INDEX idx_service_domains_status ON service_domains(status);
```

Domain types in `src/domain.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DomainStatus { Pending, Verified, Failed }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceDomain {
    pub id: Uuid,
    pub service_id: Uuid,
    pub hostname: String,
    pub status: DomainStatus,
    pub challenge_token: String,
    pub verified_at: Option<DateTime<Utc>>,
    pub last_check_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
}
```

`ServiceConfig.domains: Vec<String>` is retained for one release as a
read-only API field, populated at serialization time from
`service_domains WHERE status='verified'`. New code does not write to it.
A future ADR removes the field outright.

### Migration

Migration `0008_service_domains.sql` creates the table and backfills: for
each existing service, insert one row per existing `domains[]` entry with
`status='verified'`, a freshly generated token, and `verified_at = NOW()`.
Operators added these manually before this ADR, so they are trusted.

### HTTP API

Nested under service, matching the existing nested style.

```
POST   /v1/services/:service_id/domains
       body: { "hostname": "app.example.com" }
       201 -> ServiceDomain (includes challenge_token, verify_path)
       409 on hostname conflict

GET    /v1/services/:service_id/domains
       200 -> [ServiceDomain]

POST   /v1/services/:service_id/domains/:domain_id/verify
       200 -> ServiceDomain (status updated)
       409 if verification already in flight

DELETE /v1/services/:service_id/domains/:domain_id
       204
```

Hostname validation: non-empty, no backtick/CR/LF (existing `traefik` rule),
plus RFC-1035 label regex
`^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)+$`.
Parsed via `url::Host::parse` to reject IPs, ports, and path injections.

RBAC: viewer can read, operator can create/verify/delete (matches existing
service mutation auth).

Public challenge route on the Denia router (no auth):
`GET /.well-known/denia-challenge/:token` → `200 text/plain` with body
`{token}` if the token exists in any row, else `404`. Constant-time lookup
not required for the lookup itself but the body comparison performed during
verification uses `subtle::ConstantTimeEq`.

### Verification flow

1. Load `(service_id, domain_id)`. 404 if missing.
2. If `status=verified`, return current row (idempotent).
3. Single in-flight verify per domain enforced via
   `state.verifying_domains: Arc<Mutex<HashSet<Uuid>>>`; second concurrent
   request returns `409 domain verification already in progress`.
4. Build URL `http://{hostname}/.well-known/denia-challenge/{token}`.
5. `reqwest` GET, 5s connect + 5s read timeout, `redirect::Policy::none()`,
   `Accept: text/plain`, `User-Agent: denia-verifier/1`, response body
   capped at 1 KiB via `take(1024)`.
6. Trim single trailing newline, constant-time compare body to token.
7. On match: `status=verified`, `verified_at=now`, `last_error=NULL`,
   acquire shared routes lock, re-render Traefik dynamic config.
8. On mismatch / network error: `status=failed`, `last_error=<short reason>`,
   `last_check_at=now`. Traefik untouched.
9. Re-verify on a `failed` row resets to `pending` then runs the same flow.

Failure strings surfaced to user, kept short:
`"dns lookup failed"`, `"connection refused"`, `"connection timeout"`,
`"http {status}"`, `"body mismatch"`, `"body too large"`.

Resolved IP is not range-restricted; verifying internal hostnames is a
legitimate use case for single-node Denia. Operators are trusted.

Tokens are 32 bytes from `OsRng`, hex-encoded (64 chars). Single-shot.

### Traefik integration

`RouteSpec` shape unchanged. The producer (state → render call) reads
verified rows from `service_domains` instead of `ServiceConfig.domains`.
Service with zero verified domains → no `RouteSpec` emitted, service runs
but is unrouted.

Global challenge router added once per render:

```yaml
http:
  routers:
    denia-challenge:
      rule: "PathPrefix(`/.well-known/denia-challenge`)"
      entryPoints:
        - web
      priority: 1000
      service: denia-challenge
  services:
    denia-challenge:
      loadBalancer:
        servers:
          - url: "{control_backend_addr}"
```

Re-render triggers:
- Domain verified → re-render.
- Domain deleted while verified → re-render.
- Service deleted (cascade) → re-render if any were verified.
- Domain added (pending) → no re-render.

## Testing

Unit tests:
- `ServiceDomain` and `DomainStatus` serde round-trip.
- Hostname validation accepts valid FQDNs, rejects IPs, ports, raw IDN,
  embedded paths, backticks.
- `render_file_provider_config` emits `denia-challenge` router exactly once,
  with priority `1000`, only when at least one service exists.
- Pending/failed domains never appear in router rules.
- Service with all-pending domains emits no router.
- Verification module: token format, body trim/compare, body-size cap,
  short error strings for each network failure mode, mocked via `httpmock`.

Integration tests (`tests/backend_contract.rs` or new
`tests/domain_verification.rs`, reusing the existing `TestServer` harness):
- `POST .../domains` → 201 with token.
- Duplicate hostname → 409.
- `POST .../verify` against mocked endpoint serving correct token → 200,
  status `verified`, Traefik file on disk contains the host.
- Mismatched body → status `failed`, Traefik file unchanged.
- `DELETE` verified domain → removed from Traefik file.
- `GET /.well-known/denia-challenge/{token}` → 200 with body; unknown → 404.
- RBAC: viewer 403 on create/verify/delete; operator 200/204.

Privileged tests are not required for this work; verification is pure
control-plane.

## Dependencies

- `reqwest` (already present via `oci`).
- `subtle` (`ConstantTimeEq`) — add if not already in tree.
- `httpmock` as dev-dependency for verifier tests.
- `url` crate for `Host::parse` (already present via `reqwest`).

## Open Questions / Deferred

- DNS TXT verification, wildcard domains, automatic retry loop, per-domain
  TLS override, and forced re-verification of already-verified domains are
  out of scope. Each can be its own follow-up ADR.
- Frontend changes (console UI for adding/verifying domains) are not
  designed here; they belong in a sibling spec.

## References

- `docs/adr/007-ingress-tls.md`
- `docs/adr/008-rbac.md`
- `src/traefik.rs`
- `src/domain.rs`
