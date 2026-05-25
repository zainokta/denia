# Domain Support With HTTP File Verification — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add per-service domain entities with HTTP file verification. Pending and failed domains never reach Traefik; only verified domains route traffic and can request ACME certs.

**Architecture:** New SQLite table `service_domains` is the source of truth. Each verification challenge is served by Denia's control plane at `GET /.well-known/denia-challenge/:token` (public, no auth), exposed via a global Traefik path router on the `web` entrypoint at priority `1000`. Verification is operator-triggered (`POST /v1/services/:service_id/domains/:domain_id/verify`); Denia fetches `http://hostname/.well-known/denia-challenge/{token}`, constant-time compares the body, and re-renders Traefik on success. A `DomainVerifier` trait abstracts the HTTP fetch so tests can substitute a fake.

**Tech Stack:** Rust 2024, axum 0.8, rusqlite 0.39, reqwest 0.12 (new dep), subtle 2 (new dep), uuid v7, chrono, async-trait.

**Spec:** `docs/superpowers/specs/2026-05-25-domain-verification.md`.

---

## File Map

**Create:**
- `src/domains.rs` — `ServiceDomain`, `DomainStatus`, hostname validation, token generation, `DomainVerifier` trait + `HttpDomainVerifier` impl.
- `tests/domain_verification.rs` — integration tests against the API.

**Modify:**
- `Cargo.toml` — add `reqwest`, `subtle`; add `httpmock` to `[dev-dependencies]`.
- `src/lib.rs` — `pub mod domains;`.
- `src/domain.rs` — re-export `ServiceDomain`/`DomainStatus` from `domains` module via type alias, OR add types inline; choose inline to keep one place for domain types. (Plan picks inline.)
- `src/state.rs` — migration 5; CRUD: `put_service_domain`, `list_service_domains`, `list_service_domains_by_service`, `get_service_domain`, `get_service_domain_by_token`, `update_service_domain_status`, `delete_service_domain`; cascade in `delete_service`.
- `src/app.rs` — `AppState` gains `domain_verifier: Arc<dyn DomainVerifier>` and `verifying_domains: Arc<Mutex<HashSet<Uuid>>>`; `build_router` mounts `/.well-known/denia-challenge/:token` outside `/v1` and nested domain routes inside `/v1`.
- `src/traefik.rs` — emit `denia-challenge` global router; render filter still operates on `RouteSpec.domains` (no schema change to `RouteSpec`).
- `src/deploy.rs` — when promoting a route, read verified domain hostnames from `service_domains` instead of `service.domains`; helper `verified_domains_for(state, service_id) -> Vec<String>`.
- `README.md`, `TODO.md` — domain section.
- `docs/adr/README.md` and new `docs/adr/012-domain-verification.md` — ADR accepted.

**Test:**
- Unit tests inline in `src/domains.rs` and `src/traefik.rs`.
- New integration tests in `tests/domain_verification.rs`.

---

## Task 1: Add Dependencies

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Edit Cargo.toml**

Add to `[dependencies]`:

```toml
reqwest = { version = "0.12", default-features = false, features = ["http2"] }
subtle = "2"
```

Add to `[dev-dependencies]`:

```toml
httpmock = "0.7"
```

- [ ] **Step 2: Verify build**

Run: `cargo build`
Expected: PASS (compiles with new deps, no code uses them yet).

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "feat(deps): add reqwest, subtle, httpmock for domain verification"
```

---

## Task 2: Add `DomainStatus` and `ServiceDomain` Types

**Files:**
- Modify: `src/domain.rs`
- Test: `src/domain.rs` (inline `#[cfg(test)] mod tests`)

- [ ] **Step 1: Write failing tests in `src/domain.rs`**

Add to existing `mod tests`:

```rust
#[test]
fn domain_status_snake_case_serde() {
    let pending = serde_json::to_string(&DomainStatus::Pending).unwrap();
    assert_eq!(pending, "\"pending\"");
    let verified: DomainStatus = serde_json::from_str("\"verified\"").unwrap();
    assert_eq!(verified, DomainStatus::Verified);
    let failed: DomainStatus = serde_json::from_str("\"failed\"").unwrap();
    assert_eq!(failed, DomainStatus::Failed);
}

#[test]
fn service_domain_round_trip() {
    let id = Uuid::now_v7();
    let service_id = Uuid::now_v7();
    let d = ServiceDomain {
        id,
        service_id,
        hostname: "app.example.com".into(),
        status: DomainStatus::Pending,
        challenge_token: "abc123".into(),
        verified_at: None,
        last_check_at: None,
        last_error: None,
        created_at: Utc::now(),
    };
    let json = serde_json::to_string(&d).unwrap();
    let back: ServiceDomain = serde_json::from_str(&json).unwrap();
    assert_eq!(back, d);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --lib domain::tests::domain_status_snake_case_serde domain::tests::service_domain_round_trip`
Expected: FAIL with "cannot find type `DomainStatus`".

- [ ] **Step 3: Add types to `src/domain.rs`**

Add near other enums (e.g., after `DeploymentStatus`):

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DomainStatus {
    Pending,
    Verified,
    Failed,
}

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

Also extend `DomainError`:

```rust
#[error("invalid hostname: {0}")]
InvalidHostname(String),
```

- [ ] **Step 4: Run tests**

Run: `cargo test --lib domain::tests`
Expected: PASS for the two new tests plus existing ones.

- [ ] **Step 5: Commit**

```bash
git add src/domain.rs
git commit -m "feat(domain): add ServiceDomain and DomainStatus types"
```

---

## Task 3: Create `src/domains.rs` With Hostname Validation and Token Generator

**Files:**
- Create: `src/domains.rs`
- Modify: `src/lib.rs`
- Test: `src/domains.rs` (inline)

- [ ] **Step 1: Write failing unit tests in a new `src/domains.rs`**

Create `src/domains.rs`:

```rust
use rand::RngCore;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HostnameError {
    #[error("hostname is empty")]
    Empty,
    #[error("hostname contains invalid character")]
    InvalidCharacter,
    #[error("hostname is not a fully qualified domain name")]
    NotFqdn,
    #[error("hostname looks like an IP address or includes a port or path")]
    NotHostname,
}

pub fn validate_hostname(input: &str) -> Result<String, HostnameError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(HostnameError::Empty);
    }
    if trimmed.contains('`') || trimmed.contains('\n') || trimmed.contains('\r') {
        return Err(HostnameError::InvalidCharacter);
    }
    if trimmed.contains('/') || trimmed.contains(':') || trimmed.contains(' ') {
        return Err(HostnameError::NotHostname);
    }
    if trimmed.parse::<std::net::IpAddr>().is_ok() {
        return Err(HostnameError::NotHostname);
    }
    let labels: Vec<&str> = trimmed.split('.').collect();
    if labels.len() < 2 {
        return Err(HostnameError::NotFqdn);
    }
    for label in &labels {
        if label.is_empty() || label.len() > 63 {
            return Err(HostnameError::InvalidCharacter);
        }
        let bytes = label.as_bytes();
        if !bytes[0].is_ascii_alphanumeric() || !bytes[bytes.len() - 1].is_ascii_alphanumeric() {
            return Err(HostnameError::InvalidCharacter);
        }
        for b in bytes {
            let ok = b.is_ascii_alphanumeric() || *b == b'-';
            if !ok {
                return Err(HostnameError::InvalidCharacter);
            }
            if b.is_ascii_uppercase() {
                return Err(HostnameError::InvalidCharacter);
            }
        }
    }
    Ok(trimmed.to_string())
}

pub fn generate_token() -> String {
    let mut buf = [0u8; 32];
    rand::rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_hostname_accepts_fqdn() {
        assert_eq!(
            validate_hostname("app.example.com").unwrap(),
            "app.example.com"
        );
        assert_eq!(validate_hostname("a.b.co").unwrap(), "a.b.co");
    }

    #[test]
    fn validate_hostname_rejects_empty_and_single_label() {
        assert_eq!(validate_hostname("").unwrap_err(), HostnameError::Empty);
        assert_eq!(
            validate_hostname("localhost").unwrap_err(),
            HostnameError::NotFqdn
        );
    }

    #[test]
    fn validate_hostname_rejects_ip_port_path() {
        assert_eq!(
            validate_hostname("127.0.0.1").unwrap_err(),
            HostnameError::NotHostname
        );
        assert_eq!(
            validate_hostname("example.com:8080").unwrap_err(),
            HostnameError::NotHostname
        );
        assert_eq!(
            validate_hostname("example.com/path").unwrap_err(),
            HostnameError::NotHostname
        );
    }

    #[test]
    fn validate_hostname_rejects_uppercase_and_specials() {
        assert!(validate_hostname("APP.example.com").is_err());
        assert!(validate_hostname("app.example.com`").is_err());
        assert!(validate_hostname("app..example.com").is_err());
        assert!(validate_hostname("-leading.example.com").is_err());
        assert!(validate_hostname("trailing-.example.com").is_err());
    }

    #[test]
    fn generate_token_is_64_hex() {
        let t = generate_token();
        assert_eq!(t.len(), 64);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        let u = generate_token();
        assert_ne!(t, u, "tokens should be random");
    }
}
```

- [ ] **Step 2: Register the module**

Edit `src/lib.rs`, add `pub mod domains;` in alphabetical position near other `pub mod` declarations.

- [ ] **Step 3: Run tests**

Run: `cargo test --lib domains::tests`
Expected: PASS for all 5 tests.

- [ ] **Step 4: Commit**

```bash
git add src/lib.rs src/domains.rs
git commit -m "feat(domains): add hostname validation and token generator"
```

---

## Task 4: SQLite Migration for `service_domains`

**Files:**
- Modify: `src/state.rs` (in `migrate`)
- Test: `src/state.rs` (inline)

- [ ] **Step 1: Write failing test in `src/state.rs`**

Add to existing tests module:

```rust
#[test]
fn migrate_creates_service_domains_table() {
    let store = SqliteStore::open_in_memory().unwrap();
    store.migrate().unwrap();
    assert_eq!(store.schema_version().unwrap(), 5);
    let conn = store.connection().unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='service_domains'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}
```

If `connection()` is private, add a `#[cfg(test)] fn connection_for_tests(&self) -> ...` or use an existing public helper to count tables.

- [ ] **Step 2: Run test to confirm failure**

Run: `cargo test --lib state::tests::migrate_creates_service_domains_table`
Expected: FAIL on `assert_eq!(version, 5)` (currently 4).

- [ ] **Step 3: Implement migration 5**

At the end of `migrate` before the final `Ok(())`, append:

```rust
if current < 5 {
    connection.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS service_domains (
            id TEXT PRIMARY KEY,
            service_id TEXT NOT NULL,
            hostname TEXT NOT NULL UNIQUE,
            status TEXT NOT NULL,
            challenge_token TEXT NOT NULL UNIQUE,
            verified_at TEXT,
            last_check_at TEXT,
            last_error TEXT,
            created_at TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_service_domains_service
            ON service_domains(service_id);
        CREATE INDEX IF NOT EXISTS idx_service_domains_status
            ON service_domains(status);
        "#,
    )?;

    // Backfill existing ServiceConfig.domains as verified rows.
    let now = Utc::now().to_rfc3339();
    let mut stmt = connection.prepare("SELECT id, config_json FROM services")?;
    let rows: Vec<(String, String)> = stmt
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
        .collect::<Result<_, _>>()?;
    drop(stmt);
    for (service_id, config_json) in rows {
        let svc: ServiceConfig = serde_json::from_str(&config_json)?;
        for hostname in &svc.domains {
            let token = crate::domains::generate_token();
            let id = Uuid::now_v7().to_string();
            connection.execute(
                r#"
                INSERT OR IGNORE INTO service_domains
                  (id, service_id, hostname, status, challenge_token,
                   verified_at, last_check_at, last_error, created_at)
                VALUES (?1, ?2, ?3, 'verified', ?4, ?5, NULL, NULL, ?5)
                "#,
                params![id, service_id, hostname, token, now],
            )?;
        }
    }

    connection.execute("DELETE FROM schema_version", [])?;
    connection.execute("INSERT INTO schema_version (version) VALUES (5)", [])?;
}
```

- [ ] **Step 4: Run test**

Run: `cargo test --lib state::tests::migrate_creates_service_domains_table`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/state.rs
git commit -m "feat(state): add service_domains migration with legacy backfill"
```

---

## Task 5: State CRUD for `service_domains`

**Files:**
- Modify: `src/state.rs`
- Test: `src/state.rs` (inline)

- [ ] **Step 1: Write failing tests in `src/state.rs`**

```rust
#[test]
fn service_domains_crud_round_trip() {
    let store = SqliteStore::open_in_memory().unwrap();
    store.migrate().unwrap();

    // Create a project + service first.
    let project = Project::new("p", None).unwrap();
    store.put_project(project.clone()).unwrap();
    let svc = ServiceConfig::new(
        project.id,
        "svc",
        vec!["example.com".into()],
        ServiceSource::ExternalImage(crate::domain::ExternalImageSource {
            image: "nginx".into(),
            credential: None,
        }),
        80,
        crate::domain::HealthCheck::new("/", 5),
        None,
        Vec::new(),
    )
    .unwrap();
    store.put_service(svc.clone()).unwrap();

    let d = ServiceDomain {
        id: Uuid::now_v7(),
        service_id: svc.id,
        hostname: "x.example.com".into(),
        status: DomainStatus::Pending,
        challenge_token: "tok".into(),
        verified_at: None,
        last_check_at: None,
        last_error: None,
        created_at: Utc::now(),
    };

    store.put_service_domain(&d).unwrap();
    let back = store.get_service_domain(d.id).unwrap().unwrap();
    assert_eq!(back.hostname, "x.example.com");
    assert_eq!(back.status, DomainStatus::Pending);

    let listed = store.list_service_domains_by_service(svc.id).unwrap();
    assert_eq!(listed.len(), 1);

    let by_token = store.get_service_domain_by_token("tok").unwrap().unwrap();
    assert_eq!(by_token.id, d.id);

    store
        .update_service_domain_status(d.id, DomainStatus::Verified, Some(Utc::now()), None)
        .unwrap();
    let v = store.get_service_domain(d.id).unwrap().unwrap();
    assert_eq!(v.status, DomainStatus::Verified);
    assert!(v.verified_at.is_some());

    store.delete_service_domain(d.id).unwrap();
    assert!(store.get_service_domain(d.id).unwrap().is_none());
}

#[test]
fn service_domains_unique_hostname_rejected() {
    let store = SqliteStore::open_in_memory().unwrap();
    store.migrate().unwrap();
    let project = Project::new("p", None).unwrap();
    store.put_project(project.clone()).unwrap();
    let svc = ServiceConfig::new(
        project.id,
        "svc",
        vec!["x.example.com".into()],
        ServiceSource::ExternalImage(crate::domain::ExternalImageSource {
            image: "nginx".into(),
            credential: None,
        }),
        80,
        crate::domain::HealthCheck::new("/", 5),
        None,
        Vec::new(),
    )
    .unwrap();
    store.put_service(svc.clone()).unwrap();

    let d1 = ServiceDomain {
        id: Uuid::now_v7(),
        service_id: svc.id,
        hostname: "dup.example.com".into(),
        status: DomainStatus::Pending,
        challenge_token: "t1".into(),
        verified_at: None,
        last_check_at: None,
        last_error: None,
        created_at: Utc::now(),
    };
    let d2 = ServiceDomain {
        challenge_token: "t2".into(),
        id: Uuid::now_v7(),
        ..d1.clone()
    };
    store.put_service_domain(&d1).unwrap();
    assert!(store.put_service_domain(&d2).is_err());
}
```

- [ ] **Step 2: Run tests to confirm failure**

Run: `cargo test --lib state::tests::service_domains_crud_round_trip state::tests::service_domains_unique_hostname_rejected`
Expected: FAIL — methods don't exist yet.

- [ ] **Step 3: Implement CRUD methods on `SqliteStore`**

Add `use crate::domain::{ServiceDomain, DomainStatus};` at top (and `DateTime` if needed). Then add:

```rust
pub fn put_service_domain(&self, d: &ServiceDomain) -> Result<(), StateError> {
    let connection = self.connection()?;
    connection.execute(
        r#"
        INSERT INTO service_domains
            (id, service_id, hostname, status, challenge_token,
             verified_at, last_check_at, last_error, created_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
        "#,
        params![
            d.id.to_string(),
            d.service_id.to_string(),
            d.hostname,
            status_str(d.status),
            d.challenge_token,
            d.verified_at.map(|t| t.to_rfc3339()),
            d.last_check_at.map(|t| t.to_rfc3339()),
            d.last_error,
            d.created_at.to_rfc3339(),
        ],
    )?;
    Ok(())
}

pub fn get_service_domain(&self, id: Uuid) -> Result<Option<ServiceDomain>, StateError> {
    let connection = self.connection()?;
    connection
        .query_row(
            "SELECT id, service_id, hostname, status, challenge_token, verified_at, last_check_at, last_error, created_at FROM service_domains WHERE id = ?1",
            params![id.to_string()],
            row_to_service_domain,
        )
        .optional()
        .map_err(Into::into)
}

pub fn get_service_domain_by_token(&self, token: &str) -> Result<Option<ServiceDomain>, StateError> {
    let connection = self.connection()?;
    connection
        .query_row(
            "SELECT id, service_id, hostname, status, challenge_token, verified_at, last_check_at, last_error, created_at FROM service_domains WHERE challenge_token = ?1",
            params![token],
            row_to_service_domain,
        )
        .optional()
        .map_err(Into::into)
}

pub fn list_service_domains_by_service(&self, service_id: Uuid) -> Result<Vec<ServiceDomain>, StateError> {
    let connection = self.connection()?;
    let mut stmt = connection.prepare(
        "SELECT id, service_id, hostname, status, challenge_token, verified_at, last_check_at, last_error, created_at FROM service_domains WHERE service_id = ?1 ORDER BY created_at",
    )?;
    let rows = stmt.query_map(params![service_id.to_string()], row_to_service_domain)?;
    rows.collect::<Result<_, _>>().map_err(Into::into)
}

pub fn update_service_domain_status(
    &self,
    id: Uuid,
    status: DomainStatus,
    verified_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
) -> Result<(), StateError> {
    let connection = self.connection()?;
    let now = Utc::now().to_rfc3339();
    connection.execute(
        r#"
        UPDATE service_domains
        SET status = ?2,
            verified_at = ?3,
            last_check_at = ?4,
            last_error = ?5
        WHERE id = ?1
        "#,
        params![
            id.to_string(),
            status_str(status),
            verified_at.map(|t| t.to_rfc3339()),
            now,
            last_error,
        ],
    )?;
    Ok(())
}

pub fn delete_service_domain(&self, id: Uuid) -> Result<(), StateError> {
    let connection = self.connection()?;
    connection.execute("DELETE FROM service_domains WHERE id = ?1", params![id.to_string()])?;
    Ok(())
}

pub fn list_verified_hostnames(&self, service_id: Uuid) -> Result<Vec<String>, StateError> {
    let connection = self.connection()?;
    let mut stmt = connection.prepare(
        "SELECT hostname FROM service_domains WHERE service_id = ?1 AND status = 'verified' ORDER BY hostname",
    )?;
    let rows = stmt.query_map(params![service_id.to_string()], |row| row.get::<_, String>(0))?;
    rows.collect::<Result<_, _>>().map_err(Into::into)
}

pub fn list_all_service_domains(&self) -> Result<Vec<ServiceDomain>, StateError> {
    let connection = self.connection()?;
    let mut stmt = connection.prepare(
        "SELECT id, service_id, hostname, status, challenge_token, verified_at, last_check_at, last_error, created_at FROM service_domains ORDER BY created_at",
    )?;
    let rows = stmt.query_map([], row_to_service_domain)?;
    rows.collect::<Result<_, _>>().map_err(Into::into)
}
```

Helper functions:

```rust
fn status_str(s: DomainStatus) -> &'static str {
    match s {
        DomainStatus::Pending => "pending",
        DomainStatus::Verified => "verified",
        DomainStatus::Failed => "failed",
    }
}

fn parse_status(s: &str) -> Result<DomainStatus, StateError> {
    match s {
        "pending" => Ok(DomainStatus::Pending),
        "verified" => Ok(DomainStatus::Verified),
        "failed" => Ok(DomainStatus::Failed),
        other => Err(StateError::InvalidStatus(other.to_string())),
    }
}

fn row_to_service_domain(row: &rusqlite::Row<'_>) -> rusqlite::Result<ServiceDomain> {
    let id: String = row.get(0)?;
    let service_id: String = row.get(1)?;
    let hostname: String = row.get(2)?;
    let status_s: String = row.get(3)?;
    let challenge_token: String = row.get(4)?;
    let verified_at: Option<String> = row.get(5)?;
    let last_check_at: Option<String> = row.get(6)?;
    let last_error: Option<String> = row.get(7)?;
    let created_at: String = row.get(8)?;
    Ok(ServiceDomain {
        id: Uuid::parse_str(&id).map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?,
        service_id: Uuid::parse_str(&service_id).map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?,
        hostname,
        status: match status_s.as_str() {
            "pending" => DomainStatus::Pending,
            "verified" => DomainStatus::Verified,
            "failed" => DomainStatus::Failed,
            _ => return Err(rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, "invalid status".into())),
        },
        challenge_token,
        verified_at: verified_at
            .map(|s| DateTime::parse_from_rfc3339(&s).map(|d| d.with_timezone(&Utc)))
            .transpose()
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?,
        last_check_at: last_check_at
            .map(|s| DateTime::parse_from_rfc3339(&s).map(|d| d.with_timezone(&Utc)))
            .transpose()
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?,
        last_error,
        created_at: DateTime::parse_from_rfc3339(&created_at)
            .map_err(|e| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e)))?
            .with_timezone(&Utc),
    })
}
```

Add `InvalidStatus(String)` to `StateError`.

- [ ] **Step 4: Run tests**

Run: `cargo test --lib state::tests::service_domains_crud_round_trip state::tests::service_domains_unique_hostname_rejected`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/state.rs
git commit -m "feat(state): add service_domains CRUD operations"
```

---

## Task 6: Cascade Delete on `delete_service`

**Files:**
- Modify: `src/state.rs`
- Test: `src/state.rs` (inline)

- [ ] **Step 1: Find existing `delete_service` (or equivalent name) and write failing test**

If a `delete_service` method exists, add:

```rust
#[test]
fn deleting_service_cascades_domains() {
    let store = SqliteStore::open_in_memory().unwrap();
    store.migrate().unwrap();
    let project = Project::new("p", None).unwrap();
    store.put_project(project.clone()).unwrap();
    let svc = ServiceConfig::new(/* same construction as previous test */ ).unwrap();
    store.put_service(svc.clone()).unwrap();
    let d = ServiceDomain { /* ... */ };
    store.put_service_domain(&d).unwrap();
    store.delete_service(svc.id).unwrap();
    assert!(store.get_service_domain(d.id).unwrap().is_none());
}
```

If no `delete_service` exists, skip this task entirely and remove this section. (Check first.)

- [ ] **Step 2: Run test, confirm failure**

Run: `cargo test --lib state::tests::deleting_service_cascades_domains`
Expected: FAIL — domain row still present.

- [ ] **Step 3: Add explicit cascade**

In `delete_service`, before deleting the service row:

```rust
connection.execute(
    "DELETE FROM service_domains WHERE service_id = ?1",
    params![service_id.to_string()],
)?;
```

- [ ] **Step 4: Run test**

Run: `cargo test --lib state::tests::deleting_service_cascades_domains`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/state.rs
git commit -m "feat(state): cascade delete service_domains on service removal"
```

---

## Task 7: `DomainVerifier` Trait + HTTP Implementation

**Files:**
- Modify: `src/domains.rs`
- Test: `src/domains.rs` (inline, uses `httpmock`)

- [ ] **Step 1: Write failing tests in `src/domains.rs`**

Append:

```rust
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DomainVerifyError {
    #[error("dns lookup failed")]
    DnsLookupFailed,
    #[error("connection refused")]
    ConnectionRefused,
    #[error("connection timeout")]
    ConnectionTimeout,
    #[error("http {0}")]
    HttpStatus(u16),
    #[error("body mismatch")]
    BodyMismatch,
    #[error("body too large")]
    BodyTooLarge,
}

#[async_trait::async_trait]
pub trait DomainVerifier: Send + Sync {
    async fn verify(&self, hostname: &str, token: &str) -> Result<(), DomainVerifyError>;
}

#[cfg(test)]
mod verifier_tests {
    use super::*;
    use httpmock::prelude::*;

    fn client_with_base(base: &str) -> HttpDomainVerifier {
        HttpDomainVerifier::with_base_url(base.to_string())
    }

    #[tokio::test]
    async fn verifier_success() {
        let server = MockServer::start_async().await;
        let _m = server.mock_async(|when, then| {
            when.method(GET).path("/.well-known/denia-challenge/tok123");
            then.status(200).body("tok123");
        }).await;
        let v = client_with_base(&server.base_url());
        v.verify("ignored.example.com", "tok123").await.unwrap();
    }

    #[tokio::test]
    async fn verifier_404_returns_http_status() {
        let server = MockServer::start_async().await;
        let _m = server.mock_async(|when, then| {
            when.method(GET).path_contains("/.well-known/denia-challenge/");
            then.status(404);
        }).await;
        let v = client_with_base(&server.base_url());
        let err = v.verify("ignored.example.com", "tok").await.unwrap_err();
        assert_eq!(err, DomainVerifyError::HttpStatus(404));
    }

    #[tokio::test]
    async fn verifier_body_mismatch() {
        let server = MockServer::start_async().await;
        let _m = server.mock_async(|when, then| {
            when.method(GET).path("/.well-known/denia-challenge/tok123");
            then.status(200).body("wrong");
        }).await;
        let v = client_with_base(&server.base_url());
        let err = v.verify("ignored.example.com", "tok123").await.unwrap_err();
        assert_eq!(err, DomainVerifyError::BodyMismatch);
    }

    #[tokio::test]
    async fn verifier_body_too_large() {
        let server = MockServer::start_async().await;
        let big = "x".repeat(2048);
        let _m = server.mock_async(|when, then| {
            when.method(GET).path("/.well-known/denia-challenge/tok");
            then.status(200).body(big);
        }).await;
        let v = client_with_base(&server.base_url());
        let err = v.verify("ignored.example.com", "tok").await.unwrap_err();
        assert_eq!(err, DomainVerifyError::BodyTooLarge);
    }
}
```

- [ ] **Step 2: Run tests to confirm failure**

Run: `cargo test --lib domains::verifier_tests`
Expected: FAIL — `HttpDomainVerifier` undefined.

- [ ] **Step 3: Implement `HttpDomainVerifier`**

In `src/domains.rs`:

```rust
use std::time::Duration;
use reqwest::redirect::Policy;
use subtle::ConstantTimeEq;

const MAX_BODY: usize = 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const READ_TIMEOUT: Duration = Duration::from_secs(5);

pub struct HttpDomainVerifier {
    client: reqwest::Client,
    base_url_override: Option<String>,
}

impl HttpDomainVerifier {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .redirect(Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(READ_TIMEOUT)
            .user_agent("denia-verifier/1")
            .build()
            .expect("reqwest client");
        Self { client, base_url_override: None }
    }

    #[cfg(test)]
    pub fn with_base_url(base: String) -> Self {
        let mut v = Self::new();
        v.base_url_override = Some(base);
        v
    }
}

impl Default for HttpDomainVerifier {
    fn default() -> Self { Self::new() }
}

#[async_trait::async_trait]
impl DomainVerifier for HttpDomainVerifier {
    async fn verify(&self, hostname: &str, token: &str) -> Result<(), DomainVerifyError> {
        let base = match &self.base_url_override {
            Some(b) => b.clone(),
            None => format!("http://{hostname}"),
        };
        let url = format!("{base}/.well-known/denia-challenge/{token}");

        let response = self.client.get(&url).send().await.map_err(|e| {
            if e.is_timeout() {
                DomainVerifyError::ConnectionTimeout
            } else if e.is_connect() {
                let msg = e.to_string().to_lowercase();
                if msg.contains("dns") || msg.contains("name") {
                    DomainVerifyError::DnsLookupFailed
                } else {
                    DomainVerifyError::ConnectionRefused
                }
            } else {
                DomainVerifyError::ConnectionRefused
            }
        })?;

        let status = response.status();
        if !status.is_success() {
            return Err(DomainVerifyError::HttpStatus(status.as_u16()));
        }

        let bytes = response.bytes().await.map_err(|_| DomainVerifyError::BodyTooLarge)?;
        if bytes.len() > MAX_BODY {
            return Err(DomainVerifyError::BodyTooLarge);
        }
        let trimmed = bytes
            .strip_suffix(b"\n")
            .unwrap_or(&bytes);
        let expected = token.as_bytes();
        if trimmed.len() != expected.len() {
            return Err(DomainVerifyError::BodyMismatch);
        }
        if trimmed.ct_eq(expected).unwrap_u8() != 1 {
            return Err(DomainVerifyError::BodyMismatch);
        }
        Ok(())
    }
}
```

Add `async-trait` import at top.

- [ ] **Step 4: Run tests**

Run: `cargo test --lib domains::verifier_tests`
Expected: PASS for all four.

- [ ] **Step 5: Commit**

```bash
git add src/domains.rs Cargo.toml Cargo.lock
git commit -m "feat(domains): add DomainVerifier trait and HTTP implementation"
```

---

## Task 8: Wire `DomainVerifier` and `verifying_domains` into `AppState`

**Files:**
- Modify: `src/app.rs`
- Test: existing `cargo test --lib app` should still pass.

- [ ] **Step 1: Add fields to `AppState`**

```rust
pub struct AppState {
    // ... existing fields
    pub domain_verifier: Arc<dyn DomainVerifier>,
    pub verifying_domains: Arc<Mutex<HashSet<Uuid>>>,
}
```

Update `new_with_deploy_dependencies_and_log` to initialize:

```rust
domain_verifier: Arc::new(HttpDomainVerifier::new()),
verifying_domains: Arc::new(Mutex::new(HashSet::new())),
```

Add a constructor variant that accepts a custom verifier (for tests):

```rust
pub fn with_domain_verifier(mut self, verifier: Arc<dyn DomainVerifier>) -> Self {
    self.domain_verifier = verifier;
    self
}
```

- [ ] **Step 2: Verify build**

Run: `cargo build`
Expected: PASS.

- [ ] **Step 3: Run unit tests**

Run: `cargo test --lib`
Expected: PASS — no regressions.

- [ ] **Step 4: Commit**

```bash
git add src/app.rs
git commit -m "feat(app): wire DomainVerifier into AppState"
```

---

## Task 9: Public Challenge Endpoint `GET /.well-known/denia-challenge/:token`

**Files:**
- Modify: `src/app.rs`
- Test: `tests/domain_verification.rs` (created later; for this task add inline test if possible, otherwise rely on integration test in later task).

- [ ] **Step 1: Add handler in `src/app.rs`**

```rust
async fn challenge_handler(
    State(state): State<AppState>,
    axum::extract::Path(token): axum::extract::Path<String>,
) -> Result<Response, ApiError> {
    match state.store.get_service_domain_by_token(&token)? {
        Some(_) => Ok((
            [(header::CONTENT_TYPE, "text/plain")],
            token,
        )
            .into_response()),
        None => Err(ApiError::NotFound("challenge token not found".into())),
    }
}
```

- [ ] **Step 2: Mount route outside `/v1` (public)**

In `build_router`, before the final `Router::new()...nest("/v1", ...)`, add the challenge route to the root router so it bypasses `require_auth`:

```rust
Router::new()
    .route("/healthz", get(healthz))
    .route(
        "/.well-known/denia-challenge/{token}",
        get(challenge_handler),
    )
    .nest("/v1", auth_public.merge(auth_routes).merge(protected))
    .fallback(crate::web::static_handler)
    .with_state(state)
```

- [ ] **Step 3: Build**

Run: `cargo build`
Expected: PASS.

- [ ] **Step 4: Commit**

```bash
git add src/app.rs
git commit -m "feat(app): expose public denia-challenge endpoint"
```

---

## Task 10: Domain Management Endpoints

**Files:**
- Modify: `src/app.rs`

- [ ] **Step 1: Add request/response types**

```rust
#[derive(serde::Deserialize)]
struct CreateDomainBody {
    hostname: String,
}
```

- [ ] **Step 2: Implement `create_service_domain` handler**

```rust
async fn create_service_domain(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(service_id): axum::extract::Path<Uuid>,
    Json(body): Json<CreateDomainBody>,
) -> Result<(axum::http::StatusCode, Json<ServiceDomain>), ApiError> {
    let svc = state
        .store
        .get_service(service_id)?
        .ok_or_else(|| ApiError::NotFound("service not found".into()))?;
    ensure_role(&state, &principal, svc.project_id, Role::Operator)?;

    let hostname = crate::domains::validate_hostname(&body.hostname)
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;

    let token = crate::domains::generate_token();
    let now = Utc::now();
    let d = ServiceDomain {
        id: Uuid::now_v7(),
        service_id,
        hostname,
        status: DomainStatus::Pending,
        challenge_token: token,
        verified_at: None,
        last_check_at: None,
        last_error: None,
        created_at: now,
    };
    state.store.put_service_domain(&d).map_err(|e| match e {
        StateError::Sqlite(rusqlite::Error::SqliteFailure(err, _))
            if err.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            ApiError::Conflict("hostname already in use".into())
        }
        other => ApiError::Internal(other.to_string()),
    })?;
    Ok((axum::http::StatusCode::CREATED, Json(d)))
}
```

- [ ] **Step 3: Implement `list_service_domains` handler**

```rust
async fn list_service_domains(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(service_id): axum::extract::Path<Uuid>,
) -> Result<Json<Vec<ServiceDomain>>, ApiError> {
    let svc = state.store.get_service(service_id)?
        .ok_or_else(|| ApiError::NotFound("service not found".into()))?;
    ensure_role(&state, &principal, svc.project_id, Role::Viewer)?;
    Ok(Json(state.store.list_service_domains_by_service(service_id)?))
}
```

- [ ] **Step 4: Implement `verify_service_domain` handler**

```rust
async fn verify_service_domain(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path((service_id, domain_id)): axum::extract::Path<(Uuid, Uuid)>,
) -> Result<Json<ServiceDomain>, ApiError> {
    let svc = state.store.get_service(service_id)?
        .ok_or_else(|| ApiError::NotFound("service not found".into()))?;
    ensure_role(&state, &principal, svc.project_id, Role::Operator)?;

    let d = state.store.get_service_domain(domain_id)?
        .ok_or_else(|| ApiError::NotFound("domain not found".into()))?;
    if d.service_id != service_id {
        return Err(ApiError::NotFound("domain not found".into()));
    }
    if d.status == DomainStatus::Verified {
        return Ok(Json(d));
    }

    {
        let mut guard = state
            .verifying_domains
            .lock()
            .map_err(|_| ApiError::Conflict("verifier lock poisoned".into()))?;
        if !guard.insert(d.id) {
            return Err(ApiError::Conflict("domain verification already in progress".into()));
        }
    }

    let result = state.domain_verifier.verify(&d.hostname, &d.challenge_token).await;

    {
        let mut guard = state.verifying_domains.lock().unwrap();
        guard.remove(&d.id);
    }

    let updated = match result {
        Ok(()) => {
            state.store.update_service_domain_status(
                d.id,
                DomainStatus::Verified,
                Some(Utc::now()),
                None,
            )?;
            // Re-render Traefik dynamic config.
            crate::deploy::rerender_traefik(&state)?;
            state.store.get_service_domain(d.id)?.unwrap()
        }
        Err(e) => {
            state.store.update_service_domain_status(
                d.id,
                DomainStatus::Failed,
                None,
                Some(e.to_string()),
            )?;
            state.store.get_service_domain(d.id)?.unwrap()
        }
    };
    Ok(Json(updated))
}
```

`rerender_traefik` is added in Task 12.

- [ ] **Step 5: Implement `delete_service_domain` handler**

```rust
async fn delete_service_domain_handler(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path((service_id, domain_id)): axum::extract::Path<(Uuid, Uuid)>,
) -> Result<axum::http::StatusCode, ApiError> {
    let svc = state.store.get_service(service_id)?
        .ok_or_else(|| ApiError::NotFound("service not found".into()))?;
    ensure_role(&state, &principal, svc.project_id, Role::Operator)?;

    let d = state.store.get_service_domain(domain_id)?
        .ok_or_else(|| ApiError::NotFound("domain not found".into()))?;
    if d.service_id != service_id {
        return Err(ApiError::NotFound("domain not found".into()));
    }
    let was_verified = d.status == DomainStatus::Verified;
    state.store.delete_service_domain(domain_id)?;
    if was_verified {
        crate::deploy::rerender_traefik(&state)?;
    }
    Ok(axum::http::StatusCode::NO_CONTENT)
}
```

- [ ] **Step 6: Register routes in `protected` block**

In `build_router`'s `protected` chain, add (place above `/services/{service_id}/{action}` because that catch-all `{action}` route would otherwise swallow `domains`):

```rust
.route(
    "/services/{service_id}/domains",
    get(list_service_domains).post(create_service_domain),
)
.route(
    "/services/{service_id}/domains/{domain_id}",
    axum::routing::delete(delete_service_domain_handler),
)
.route(
    "/services/{service_id}/domains/{domain_id}/verify",
    post(verify_service_domain),
)
```

Verify ordering: the existing `.route("/services/{service_id}/{action}", post(lifecycle_command))` must come AFTER the domain routes, since axum matches the more specific path first when registered. Confirm by reading the current ordering in `src/app.rs:162-195`.

- [ ] **Step 7: Build**

Run: `cargo build`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add src/app.rs
git commit -m "feat(api): add domain management endpoints"
```

---

## Task 11: Traefik Global Challenge Router

**Files:**
- Modify: `src/traefik.rs`
- Test: `src/traefik.rs` (inline)

- [ ] **Step 1: Write failing tests in `src/traefik.rs`**

Add to existing test module:

```rust
#[test]
fn render_emits_denia_challenge_router_when_routes_present() {
    let spec = make_spec("svc", vec!["a.example.com"], 9000, false);
    let opts = IngressRenderOptions::test_defaults();
    let out = render_file_provider_config(&[spec], &opts).unwrap();
    assert!(out.contains("denia-challenge:"));
    assert!(out.contains("PathPrefix(`/.well-known/denia-challenge`)"));
    assert!(out.contains("priority: 1000"));
    // service entry
    assert!(out.contains("    denia-challenge:\n      loadBalancer:"));
}

#[test]
fn render_emits_denia_challenge_router_even_with_zero_routes() {
    let opts = IngressRenderOptions::test_defaults();
    let out = render_file_provider_config(&[], &opts).unwrap();
    assert!(out.contains("denia-challenge:"));
}
```

- [ ] **Step 2: Run tests, confirm failure**

Run: `cargo test --lib traefik::tests::render_emits_denia_challenge_router_when_routes_present traefik::tests::render_emits_denia_challenge_router_even_with_zero_routes`
Expected: FAIL.

- [ ] **Step 3: Modify `render_file_provider_config`**

After the per-service router loop and the optional control router, emit the challenge router unconditionally:

```rust
output.push_str(
    "    denia-challenge:\n      rule: \"PathPrefix(`/.well-known/denia-challenge`)\"\n      entryPoints:\n        - web\n      priority: 1000\n      service: denia-challenge\n",
);
```

And in the services section, after services loop and optional control service:

```rust
output.push_str(&format!(
    "    denia-challenge:\n      loadBalancer:\n        servers:\n          - url: \"{addr}\"\n",
    addr = options.control_backend_addr,
));
```

If `control_backend_addr` is empty, skip the challenge service to keep the rendered YAML usable (operator misconfiguration). Decision: still emit; bad addr surfaces as a Traefik failure rather than silent omission.

- [ ] **Step 4: Run all traefik tests**

Run: `cargo test --lib traefik`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/traefik.rs
git commit -m "feat(traefik): emit global denia-challenge path router"
```

---

## Task 12: Deploy Re-Render Path Reads Verified Hostnames

**Files:**
- Modify: `src/deploy.rs`, `src/app.rs`
- Test: existing deploy tests must keep passing; add new test in `tests/deploy_orchestration.rs`.

- [ ] **Step 1: Find the route-insertion site**

In `src/deploy.rs` around line 272–280 the code inserts a `RouteSpec` using `service.domains.clone()`. Replace with verified hostnames from the store.

- [ ] **Step 2: Add helper `rerender_traefik` in `src/deploy.rs`**

```rust
pub fn rerender_traefik(state: &crate::app::AppState) -> Result<(), DeployError> {
    let services = state.store.list_services()?;
    let mut routes_guard = state.routes
        .lock()
        .map_err(|_| DeployError::BridgeLockPoisoned)?;
    // Rebuild routes from verified hostnames; preserve existing bridge_port assignments.
    let existing = routes_guard.clone();
    routes_guard.clear();
    for svc in services {
        let hostnames = state.store.list_verified_hostnames(svc.id)?;
        if hostnames.is_empty() {
            continue;
        }
        let Some(prev) = existing.get(&svc.name) else {
            // Service not currently routed (no bridge assigned). Skip until next deploy.
            continue;
        };
        routes_guard.insert(
            svc.name.clone(),
            RouteSpec {
                route_key: prev.route_key.clone(),
                service_name: svc.name.clone(),
                domains: hostnames,
                bridge_port: prev.bridge_port,
                tls: svc.tls_enabled,
            },
        );
    }
    let yaml = render_file_provider_config(
        &routes_guard.values().cloned().collect::<Vec<_>>(),
        &state.ingress_options,
    )?;
    let path = state.config.traefik_dynamic_path.clone();
    std::fs::write(&path, yaml)?;
    Ok(())
}
```

Notes:
- `state.routes` must be exposed (already is `SharedRoutes`).
- `state.config.traefik_dynamic_path` — confirm path field name and add a public accessor if needed.
- `routes` is also the source of truth for `list_ingress_routes`; this method must keep that map consistent. The strategy of preserving prior `bridge_port` from `existing` avoids re-assigning ports.

- [ ] **Step 3: Modify the deploy-time route insertion**

In the existing deploy code where `service.domains.clone()` is used, replace with verified hostnames:

```rust
let hostnames = state.store.list_verified_hostnames(service.id)?;
if hostnames.is_empty() {
    // No verified domains; skip Traefik registration. Workload still runs.
    return Ok(());
}
// ... continue with hostnames instead of service.domains.clone()
```

Adjust the surrounding code: `RouteSpec { domains: hostnames, ... }`.

- [ ] **Step 4: Add `StateError` conversion to `DeployError` if needed**

```rust
#[error("state error: {0}")]
State(#[from] crate::state::StateError),
```

- [ ] **Step 5: Run tests**

Run: `cargo test --lib`
Run: `cargo test --test deploy_orchestration`
Expected: PASS (existing tests may need a domain row inserted; update fixtures).

- [ ] **Step 6: Update deploy_orchestration test fixtures**

For each test that previously relied on `service.domains` triggering Traefik routing, insert a verified `ServiceDomain` row after `put_service`. Example helper at top of `tests/deploy_orchestration.rs`:

```rust
fn seed_verified_domain(store: &SqliteStore, service_id: Uuid, hostname: &str) {
    use denia::domain::{DomainStatus, ServiceDomain};
    use chrono::Utc;
    store.put_service_domain(&ServiceDomain {
        id: Uuid::now_v7(),
        service_id,
        hostname: hostname.into(),
        status: DomainStatus::Verified,
        challenge_token: denia::domains::generate_token(),
        verified_at: Some(Utc::now()),
        last_check_at: None,
        last_error: None,
        created_at: Utc::now(),
    }).unwrap();
}
```

- [ ] **Step 7: Run full test suite**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add src/deploy.rs src/app.rs tests/deploy_orchestration.rs
git commit -m "feat(deploy): drive Traefik routes from verified service_domains"
```

---

## Task 13: Integration Tests for Domain API

**Files:**
- Create: `tests/domain_verification.rs`

- [ ] **Step 1: Scaffold test file**

Look at `tests/backend_contract.rs` for the existing `TestServer` harness. Mirror its setup.

```rust
use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Utc;
use denia::app::{AppState, build_router};
use denia::config::AppConfig;
use denia::domain::DomainStatus;
use denia::domains::{DomainVerifier, DomainVerifyError};
use denia::state::SqliteStore;
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;

struct FakeVerifier {
    expected_token: String,
}

#[async_trait::async_trait]
impl DomainVerifier for FakeVerifier {
    async fn verify(&self, _hostname: &str, token: &str) -> Result<(), DomainVerifyError> {
        if token == self.expected_token {
            Ok(())
        } else {
            Err(DomainVerifyError::BodyMismatch)
        }
    }
}

async fn setup_state(verifier: Arc<dyn DomainVerifier>) -> (AppState, Uuid /* service_id */, String /* admin_token */) {
    // 1. open in-memory store
    // 2. migrate
    // 3. seed super-admin user + project + service
    // 4. build AppState with verifier override
    // 5. return state + service_id + super-admin bearer token
    todo!("mirror backend_contract::setup")
}
```

Implementation note: copy the `TestServer` pattern from `tests/backend_contract.rs`. If that file uses helpers from a shared module, reuse it.

- [ ] **Step 2: Write the 8 integration tests**

```rust
#[tokio::test]
async fn post_domains_creates_pending_row() {
    let verifier = Arc::new(FakeVerifier { expected_token: String::new() });
    let (state, svc_id, token) = setup_state(verifier).await;
    let app = build_router(state.clone());

    let body = serde_json::to_vec(&json!({"hostname": "app.example.com"})).unwrap();
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/v1/services/{svc_id}/domains"))
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::CREATED);
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX).await.unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(parsed["status"], "pending");
    assert_eq!(parsed["hostname"], "app.example.com");
    assert!(parsed["challenge_token"].as_str().unwrap().len() == 64);
}

#[tokio::test]
async fn post_domains_rejects_duplicate_hostname() {
    /* create twice, second returns 409 */
}

#[tokio::test]
async fn post_verify_marks_verified_on_match() {
    /* seed a pending domain; fake verifier returns Ok; assert status -> verified, verified_at non-null */
}

#[tokio::test]
async fn post_verify_marks_failed_on_mismatch() {
    /* fake verifier returns Err; assert status -> failed, last_error == "body mismatch" */
}

#[tokio::test]
async fn delete_domain_removes_row() {
    /* DELETE returns 204; subsequent GET shows empty list */
}

#[tokio::test]
async fn challenge_endpoint_returns_token_body() {
    /* create domain; GET /.well-known/denia-challenge/{token} returns 200 + token text */
}

#[tokio::test]
async fn challenge_endpoint_404_for_unknown_token() {
    /* GET with random token -> 404 */
}

#[tokio::test]
async fn verify_re_renders_traefik_dynamic_file() {
    /* deploy service so a bridge port exists, then verify domain, then read traefik file and assert host appears */
}

#[tokio::test]
async fn viewer_cannot_create_or_verify_domain() {
    /* seed a viewer; POST/verify/delete return 403 */
}
```

- [ ] **Step 3: Run tests, iterate**

Run: `cargo test --test domain_verification`
Expected: PASS for all.

If any test reveals a missing wiring, fix it and re-run.

- [ ] **Step 4: Commit**

```bash
git add tests/domain_verification.rs
git commit -m "test(domains): integration coverage for domain API and verification"
```

---

## Task 14: ADR-012 And README/TODO Updates

**Files:**
- Create: `docs/adr/012-domain-verification.md`
- Modify: `docs/adr/README.md`, `README.md`, `TODO.md`

- [ ] **Step 1: Write the ADR**

Create `docs/adr/012-domain-verification.md` using the existing ADR format. Status: Accepted. Date: 2026-05-25. Reference the spec at `docs/superpowers/specs/2026-05-25-domain-verification.md`. Summarize context, decision, consequences, alternatives.

- [ ] **Step 2: Add row to ADR index**

Append to the table in `docs/adr/README.md`:

```
| [012](012-domain-verification.md) | Domain Support And HTTP File Verification | Accepted | 2026-05-25 |
```

- [ ] **Step 3: Update README.md and TODO.md**

- README: mention domain verification under the ingress/Traefik section. Example: "Domains added to a service must be HTTP-verified before they route. See ADR-012."
- TODO: remove any prior `domain` placeholder; add follow-up: "Frontend console for adding/verifying domains."

- [ ] **Step 4: Commit**

```bash
git add docs/adr/012-domain-verification.md docs/adr/README.md README.md TODO.md
git commit -m "docs(adr): accept 012 domain verification and update index"
```

---

## Task 15: Final Verification

- [ ] **Step 1: Format**

Run: `cargo fmt --all`
Commit if changes: `git add -u && git commit -m "style: cargo fmt"`

- [ ] **Step 2: Lint**

Run: `cargo clippy --all-targets --all-features -- -D warnings`
Expected: clean.

- [ ] **Step 3: Full test suite**

Run: `cargo test`
Expected: all green.

- [ ] **Step 4: Manual sanity (optional, requires running server)**

```bash
cargo run &
# POST /v1/services/<id>/domains
# GET /.well-known/denia-challenge/<token>
# POST /v1/services/<id>/domains/<id>/verify  (using a real reachable hostname; or use FakeVerifier in dev mode)
```

- [ ] **Step 5: Report verification results**

Per CLAUDE.md: print the exact commands run and their outputs before finishing.

---

## Notes And Risks

- **Order of route registration matters.** `/services/{service_id}/{action}` is a catch-all that will swallow `/services/{id}/domains` if registered first. Register domain routes BEFORE the lifecycle catch-all.
- **`StateError` -> `DeployError` and `ApiError` conversions** may need additional `#[from]` impls. Add as compiler complaints surface.
- **`rusqlite::Error::SqliteFailure` matching** for unique constraint must use `rusqlite::ErrorCode::ConstraintViolation`. If the actual error variant differs, use `rusqlite::ffi::ErrorCode::ConstraintViolation`.
- **`reqwest` with `default-features = false`** must still link. If build fails, try `features = ["http2", "default-tls"]`, since `default-tls` is required for TLS but harmless for plain HTTP. Plain HTTP only is fine without TLS features in reqwest 0.12.
- **`httpmock` async API**: the `mock_async` form is used to compose with `tokio::test`. Confirm against the installed crate version.
- **No background scheduler** is added in this plan. Verification is strictly operator-triggered.
- **Frontend changes are out of scope** — a sibling plan will cover the console UI.

---

## Done When

- All checkboxes ticked.
- `cargo fmt`, `cargo clippy`, and `cargo test` all pass.
- ADR-012 committed and listed in the index.
- Existing Traefik dynamic file still renders for services with verified domains; pending/failed domains never appear.
