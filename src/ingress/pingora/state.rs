//! Shared ingress state for the Pingora proxy.
//!
//! `IngressState` is the control brain shared (via `Arc`) between the Denia
//! control plane and the Pingora proxy services running on a dedicated OS
//! thread. It absorbs the loopback bridge's replica pools, health, scale-from-
//! zero activation, idle tracking and access log, and adds an `ArcSwap`-backed
//! route table and cert store.
//!
//! NOTE: this is the additive Phase 2 home. The legacy `src/ingress/bridge.rs`
//! still owns the live transport during this chunk; the types here are a
//! parallel, distinct definition (no name collision via module paths).

use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Instant};

use arc_swap::ArcSwap;
use async_trait::async_trait;
use pingora::tls::{pkey::PKey, pkey::Private, x509::X509};
use serde::Serialize;
use thiserror::Error;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::access_log::AccessLogStore;

/// Typed errors at the ingress boundary.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum IngressError {
    #[error("route service name cannot be empty")]
    EmptyServiceName,
    #[error("route must include at least one domain")]
    MissingDomain,
    #[error("invalid domain: {0}")]
    InvalidDomain(String),
}

/// Maximum length of a DNS name (RFC 1035 presentation form, excluding the
/// trailing root dot).
const MAX_DOMAIN_LEN: usize = 253;
/// Maximum length of a single DNS label.
const MAX_LABEL_LEN: usize = 63;

/// Validate and normalize a routing/SNI hostname.
///
/// Domains validated here flow into routing keys, ACME order identifiers, and
/// TLS SNI selection (audit A1/A5), so this is the single ingest chokepoint.
///
/// Rejects: empty / whitespace-only, total length > 253, any control character
/// or ASCII whitespace, backtick / CR / LF (the legacy Traefik check), a
/// leading or trailing dot, an empty label (`..`), any label > 63 chars,
/// `*` wildcards, and any non-ASCII byte. Non-ASCII is rejected deliberately:
/// callers must pass already-punycode-encoded (`xn--`) ASCII so IDN homoglyphs
/// cannot produce routing/SNI confusion.
///
/// On success returns the ASCII-lowercased hostname (audit A2): lookups use
/// exact `BTreeMap::get`, so a mixed-case `Host`/SNI would otherwise 404.
pub fn validate_domain(domain: &str) -> Result<String, IngressError> {
    let reject = || IngressError::InvalidDomain(domain.to_string());

    if domain.trim().is_empty() {
        return Err(reject());
    }
    if domain.len() > MAX_DOMAIN_LEN {
        return Err(reject());
    }
    // Non-ASCII (covers homoglyphs), control chars, whitespace, backtick, CR/LF,
    // and wildcards are all forbidden bytes.
    for b in domain.bytes() {
        if !b.is_ascii()
            || b.is_ascii_control()
            || (b as char).is_ascii_whitespace()
            || b == b'`'
            || b == b'*'
        {
            return Err(reject());
        }
    }
    if domain.starts_with('.') || domain.ends_with('.') {
        return Err(reject());
    }
    for label in domain.split('.') {
        if label.is_empty() || label.len() > MAX_LABEL_LEN {
            return Err(reject());
        }
    }

    Ok(domain.to_ascii_lowercase())
}

/// A single service's routing entry.
///
/// This is the Pingora-era successor to `traefik::RouteSpec`. It drops the
/// `bridge_port` field — with UDS upstreams (Spike 0.2 = YES) there is no
/// loopback bridge port to render. `route_key` is the stable per-entry key
/// (the service id; see `coordinator.rs` F-3 comment) used to deduplicate
/// services whose names collide across projects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RouteSpec {
    pub route_key: String,
    pub service_name: String,
    pub domains: Vec<String>,
    pub tls: bool,
}

/// Host-indexed routing table.
///
/// Each domain maps to the owning `RouteSpec`. The table is rebuilt and swapped
/// atomically (`ArcSwap`) on route changes, so resolution never blocks on a
/// lock in the proxy hot path.
#[derive(Debug, Clone, Default)]
pub struct RouteTable {
    by_host: BTreeMap<String, RouteSpec>,
}

impl RouteTable {
    /// Insert or replace `spec`, indexing it under each of its domains.
    ///
    /// Each domain is validated and lowercased via [`validate_domain`] before
    /// insertion, so the table never holds an unvalidated or mixed-case host
    /// (audit A1/A2). Any domain that fails validation is skipped; callers that
    /// need to surface rejection should use [`RouteTable::try_upsert`].
    ///
    /// If a domain previously pointed at a different `route_key`, the most
    /// recent `upsert` wins for that host.
    pub fn upsert(&mut self, spec: RouteSpec) {
        for domain in &spec.domains {
            if let Ok(host) = validate_domain(domain) {
                self.by_host.insert(host, spec.clone());
            }
        }
    }

    /// Validating insert: rejects an empty `service_name` ([`IngressError::EmptyServiceName`]),
    /// an empty `domains` list ([`IngressError::MissingDomain`]), and any domain
    /// that fails [`validate_domain`] ([`IngressError::InvalidDomain`]).
    ///
    /// Insertion is all-or-nothing: if any domain is invalid, nothing is
    /// inserted. Valid domains are normalized to lowercase before insertion.
    pub fn try_upsert(&mut self, spec: RouteSpec) -> Result<(), IngressError> {
        if spec.service_name.trim().is_empty() {
            return Err(IngressError::EmptyServiceName);
        }
        if spec.domains.is_empty() {
            return Err(IngressError::MissingDomain);
        }
        // Validate everything before mutating so a later invalid domain cannot
        // leave a partial entry behind.
        let hosts = spec
            .domains
            .iter()
            .map(|d| validate_domain(d))
            .collect::<Result<Vec<_>, _>>()?;
        for host in hosts {
            self.by_host.insert(host, spec.clone());
        }
        Ok(())
    }

    /// Remove every host entry owned by `route_key`.
    pub fn remove(&mut self, route_key: &str) {
        self.by_host.retain(|_, spec| spec.route_key != route_key);
    }

    /// Resolve a request `Host` to its owning route, if any. The lookup key is
    /// lowercased so a mixed-case `Host` header still matches (audit A2).
    pub fn resolve(&self, host: &str) -> Option<&RouteSpec> {
        self.by_host.get(&host.to_ascii_lowercase())
    }

    /// Number of distinct host entries (for diagnostics/tests).
    pub fn host_count(&self) -> usize {
        self.by_host.len()
    }
}

/// A parsed TLS certificate chain plus its private key, ready to install into a
/// handshake. The leaf certificate is first in `chain`.
///
/// Holds boringssl-parsed material (`X509` / `PKey`). The key is never logged or
/// serialized (CLAUDE.md secrets discipline); `ParsedCert` intentionally does
/// not derive `Debug`/`Serialize`.
#[derive(Clone)]
pub struct ParsedCert {
    /// Full chain, leaf first.
    pub chain: Vec<X509>,
    /// Private key for the leaf certificate.
    pub key: PKey<Private>,
}

/// SNI → parsed certificate map, swapped atomically on issuance/renewal.
///
/// Selection (the `TlsAccept` callback, wired in a later chunk) reads a snapshot
/// of this store synchronously at handshake time; issuance swaps a new store in
/// without restarting the listener. A missing SNI means "decline" — no default
/// cert is ever leaked.
#[derive(Default, Clone)]
pub struct CertStore {
    by_sni: BTreeMap<String, ParsedCert>,
}

impl CertStore {
    /// Insert or replace the certificate served for `sni`, validating and
    /// lowercasing the SNI key via [`validate_domain`] (audit A2/A5). Returns
    /// [`IngressError::InvalidDomain`] for a malformed SNI so unsanitized hosts
    /// never become handshake-selection keys.
    pub fn try_insert(
        &mut self,
        sni: impl AsRef<str>,
        cert: ParsedCert,
    ) -> Result<(), IngressError> {
        let key = validate_domain(sni.as_ref())?;
        self.by_sni.insert(key, cert);
        Ok(())
    }

    /// Look up the certificate for an SNI, if present. The lookup key is
    /// lowercased so case-variant SNI still matches (audit A2).
    pub fn get(&self, sni: &str) -> Option<&ParsedCert> {
        self.by_sni.get(&sni.to_ascii_lowercase())
    }

    /// Number of certificates held (for diagnostics/tests).
    pub fn len(&self) -> usize {
        self.by_sni.len()
    }

    /// Whether the store holds no certificates.
    pub fn is_empty(&self) -> bool {
        self.by_sni.is_empty()
    }

    /// The SNI names currently held (for renewal scanning / diagnostics).
    pub fn sni_names(&self) -> Vec<String> {
        self.by_sni.keys().cloned().collect()
    }
}

/// Maximum time a request waits for a cold-start activation to produce a
/// healthy replica before giving up with a 503.
pub const ACTIVATION_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Number of times a waiter re-checks `next_socket` after a successful
/// activation before treating the absence of a socket as a failure.
const POST_ACTIVATION_RETRIES: usize = 5;
const POST_ACTIVATION_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(20);

/// Cold-start hook for scale-to-zero services. The controller launches the
/// service and only returns `Ok` once at least one replica is `Healthy`.
#[async_trait]
pub trait ActivationHook: Send + Sync {
    async fn activate(&self, service: &str) -> Result<(), ActivationError>;
}

#[derive(Debug, Error)]
pub enum ActivationError {
    #[error("activation timed out")]
    Timeout,
    #[error("activation failed: {0}")]
    Failed(String),
}

/// A single replica's Denia-owned Unix socket endpoint within a service pool.
#[derive(Debug, Clone)]
pub struct ReplicaEndpoint {
    pub replica_id: Uuid,
    pub socket_path: PathBuf,
    pub healthy: bool,
}

/// Per-service fan-out state: replica endpoints, a round-robin cursor over the
/// healthy ones, and the last time a request was proxied.
struct ServicePool {
    endpoints: Vec<ReplicaEndpoint>,
    cursor: usize,
    last_activity: Instant,
}

impl ServicePool {
    fn new() -> Self {
        Self {
            endpoints: Vec::new(),
            cursor: 0,
            last_activity: Instant::now(),
        }
    }

    fn healthy_count(&self) -> usize {
        self.endpoints.iter().filter(|e| e.healthy).count()
    }

    /// Round-robin over `healthy == true` endpoints, advancing the cursor.
    /// Returns `None` if no healthy endpoint exists.
    fn next_socket(&mut self) -> Option<PathBuf> {
        let len = self.endpoints.len();
        if len == 0 {
            return None;
        }
        for offset in 0..len {
            let idx = (self.cursor + offset) % len;
            if self.endpoints[idx].healthy {
                self.cursor = (idx + 1) % len;
                return Some(self.endpoints[idx].socket_path.clone());
            }
        }
        None
    }
}

/// Shared ingress control brain.
///
/// Holds the per-service replica pools, health, the scale-from-zero activation
/// hook + single-flight gates, idle activity tracking, and the access log. The
/// method signatures mirror `LoopbackBridgeSupervisor` so the Phase 5 cutover is
/// mechanical.
///
/// The route table and cert store are added as `ArcSwap` fields in Task 2.3.
#[derive(Default)]
pub struct IngressState {
    /// Host-indexed routing table, swapped atomically on route changes.
    routes: ArcSwap<RouteTable>,
    /// SNI-indexed cert store, swapped atomically on issuance/renewal.
    certs: ArcSwap<CertStore>,
    pools: Mutex<BTreeMap<String, ServicePool>>,
    access_log: AccessLogStore,
    /// Optional cold-start hook injected by the controller. When unset, a
    /// request to a zero-replica service resolves to `None`.
    activator: Mutex<Option<Arc<dyn ActivationHook>>>,
    /// Per-service single-flight gate serializing concurrent cold starts.
    activation_gates: Mutex<BTreeMap<String, Arc<Mutex<()>>>>,
}

impl IngressState {
    /// Construct with a shared `AccessLogStore`.
    pub fn with_access_log(access_log: AccessLogStore) -> Self {
        Self {
            routes: ArcSwap::default(),
            certs: ArcSwap::default(),
            pools: Mutex::new(BTreeMap::new()),
            access_log,
            activator: Mutex::new(None),
            activation_gates: Mutex::new(BTreeMap::new()),
        }
    }

    /// Clone the shared access log handle.
    pub fn access_log(&self) -> AccessLogStore {
        self.access_log.clone()
    }

    /// Atomically replace the routing table. Lock-free for readers.
    ///
    /// Single-writer / last-writer-wins (audit A8): only the control plane
    /// (deploy/verify/delete paths) calls this, so the whole-table swap is safe;
    /// it is NOT safe under concurrent writers.
    pub fn swap_routes(&self, table: RouteTable) {
        self.routes.store(Arc::new(table));
    }

    /// Load a snapshot of the current routing table (cheap, lock-free).
    pub fn routes(&self) -> Arc<RouteTable> {
        self.routes.load_full()
    }

    /// Resolve a request `Host` to its owning service name, if routed.
    pub fn resolve_host(&self, host: &str) -> Option<String> {
        self.routes
            .load()
            .resolve(host)
            .map(|r| r.service_name.clone())
    }

    /// Atomically replace the cert store. Lock-free for readers.
    ///
    /// Single-writer / last-writer-wins (audit A8): only the control plane (boot
    /// load + the ACME issuance/renewal task) calls this; it is NOT safe under
    /// concurrent writers.
    pub fn swap_certs(&self, store: CertStore) {
        self.certs.store(Arc::new(store));
    }

    /// Load a snapshot of the current cert store (cheap, lock-free).
    pub fn certs(&self) -> Arc<CertStore> {
        self.certs.load_full()
    }

    /// Inject the cold-start activation hook. Until set, requests to a
    /// scaled-to-zero service resolve to `None` rather than triggering a launch.
    pub async fn set_activator(&self, hook: Arc<dyn ActivationHook>) {
        *self.activator.lock().await = Some(hook);
    }

    /// Insert or replace a replica endpoint (default `healthy = false`),
    /// creating the service pool if it does not yet exist.
    pub async fn add_replica(&self, service: &str, replica_id: Uuid, socket_path: PathBuf) {
        self.add_replica_with_health(service, replica_id, socket_path, false)
            .await;
    }

    async fn add_replica_with_health(
        &self,
        service: &str,
        replica_id: Uuid,
        socket_path: PathBuf,
        healthy: bool,
    ) {
        let mut pools = self.pools.lock().await;
        let pool = pools
            .entry(service.to_string())
            .or_insert_with(ServicePool::new);
        let endpoint = ReplicaEndpoint {
            replica_id,
            socket_path,
            healthy,
        };
        if let Some(existing) = pool
            .endpoints
            .iter_mut()
            .find(|e| e.replica_id == replica_id)
        {
            *existing = endpoint;
        } else {
            pool.endpoints.push(endpoint);
        }
    }

    /// Mark a replica endpoint healthy or unhealthy. No-op if absent.
    pub async fn set_replica_healthy(&self, service: &str, replica_id: Uuid, healthy: bool) {
        let mut pools = self.pools.lock().await;
        if let Some(pool) = pools.get_mut(service)
            && let Some(endpoint) = pool
                .endpoints
                .iter_mut()
                .find(|e| e.replica_id == replica_id)
        {
            endpoint.healthy = healthy;
        }
    }

    /// Remove a replica endpoint. No-op if absent.
    pub async fn remove_replica(&self, service: &str, replica_id: Uuid) {
        let mut pools = self.pools.lock().await;
        if let Some(pool) = pools.get_mut(service) {
            pool.endpoints.retain(|e| e.replica_id != replica_id);
            if pool.cursor >= pool.endpoints.len() {
                pool.cursor = 0;
            }
        }
    }

    /// Number of healthy endpoints for `service`.
    pub async fn healthy_count(&self, service: &str) -> usize {
        let pools = self.pools.lock().await;
        pools
            .get(service)
            .map(ServicePool::healthy_count)
            .unwrap_or(0)
    }

    /// Last time a request was proxied for `service`, if the pool exists.
    pub async fn last_activity(&self, service: &str) -> Option<Instant> {
        let pools = self.pools.lock().await;
        pools.get(service).map(|pool| pool.last_activity)
    }

    /// Set the recorded `last_activity` for `service`, creating the pool entry
    /// if it does not yet exist. Primarily for backdating activity in tests.
    pub async fn set_last_activity(&self, service: &str, when: Instant) {
        let mut pools = self.pools.lock().await;
        pools
            .entry(service.to_string())
            .or_insert_with(ServicePool::new)
            .last_activity = when;
    }

    /// Round-robin the next healthy socket for `service`, advancing the cursor
    /// and updating `last_activity`. `None` if no healthy endpoint exists.
    pub async fn next_socket(&self, service: &str) -> Option<PathBuf> {
        let mut pools = self.pools.lock().await;
        let pool = pools.get_mut(service)?;
        let socket = pool.next_socket()?;
        pool.last_activity = Instant::now();
        Some(socket)
    }

    /// Fetch (creating if absent) the per-service single-flight activation gate.
    async fn activation_gate(&self, service: &str) -> Arc<Mutex<()>> {
        let mut gates = self.activation_gates.lock().await;
        gates
            .entry(service.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// Resolve a healthy socket for `service`, triggering a single-flight
    /// cold-start activation when none is available and an activator is set.
    ///
    /// Returns `Ok(None)` only when no activator is configured. On activation
    /// failure the gate is released so a later request retries fresh.
    pub async fn resolve_or_activate(
        &self,
        service: &str,
    ) -> Result<Option<PathBuf>, ActivationError> {
        if let Some(socket) = self.next_socket(service).await {
            return Ok(Some(socket));
        }
        let activator = { self.activator.lock().await.clone() };
        let Some(activator) = activator else {
            return Ok(None);
        };

        let gate = self.activation_gate(service).await;
        let _guard = gate.lock().await;

        // Another request may have activated while we waited on the gate.
        if let Some(socket) = self.next_socket(service).await {
            return Ok(Some(socket));
        }

        // Bound the whole activation + post-activation wait so a hung
        // `ActivationHook::activate()` cannot block the proxy hot path forever
        // (audit A4). On elapse we drop the gate guard and return `Timeout`,
        // letting a later request retry fresh.
        match tokio::time::timeout(ACTIVATION_WAIT, self.activate_and_wait(service, &activator))
            .await
        {
            Ok(result) => result,
            Err(_elapsed) => Err(ActivationError::Timeout),
        }
    }

    /// Run the activation hook then poll for a healthy socket. Wrapped by
    /// `resolve_or_activate` in an overall `ACTIVATION_WAIT` timeout.
    async fn activate_and_wait(
        &self,
        service: &str,
        activator: &Arc<dyn ActivationHook>,
    ) -> Result<Option<PathBuf>, ActivationError> {
        activator.activate(service).await?;

        for attempt in 0..POST_ACTIVATION_RETRIES {
            if let Some(socket) = self.next_socket(service).await {
                return Ok(Some(socket));
            }
            if attempt + 1 < POST_ACTIVATION_RETRIES {
                tokio::time::sleep(POST_ACTIVATION_RETRY_DELAY).await;
            }
        }
        Err(ActivationError::Failed(
            "activation reported healthy but no socket became available".to_string(),
        ))
    }
}

#[cfg(test)]
mod route_table_tests {
    use super::*;

    #[test]
    fn route_table_resolves_host_to_service() {
        let mut t = RouteTable::default();
        t.upsert(RouteSpec {
            route_key: "svc-1".into(),
            service_name: "api".into(),
            domains: vec!["api.example.com".into()],
            tls: true,
        });
        assert_eq!(
            t.resolve("api.example.com")
                .map(|r| r.service_name.as_str()),
            Some("api")
        );
        assert!(t.resolve("nope.example.com").is_none());
    }

    #[test]
    fn upsert_indexes_all_domains_and_remove_drops_them() {
        let mut t = RouteTable::default();
        t.upsert(RouteSpec {
            route_key: "svc-1".into(),
            service_name: "api".into(),
            domains: vec!["api.example.com".into(), "www.api.example.com".into()],
            tls: false,
        });
        assert_eq!(t.host_count(), 2);
        assert!(t.resolve("www.api.example.com").is_some());

        t.remove("svc-1");
        assert_eq!(t.host_count(), 0);
        assert!(t.resolve("api.example.com").is_none());
    }

    #[test]
    fn latest_upsert_wins_for_a_shared_host() {
        let mut t = RouteTable::default();
        t.upsert(RouteSpec {
            route_key: "svc-old".into(),
            service_name: "old".into(),
            domains: vec!["app.example.com".into()],
            tls: false,
        });
        t.upsert(RouteSpec {
            route_key: "svc-new".into(),
            service_name: "new".into(),
            domains: vec!["app.example.com".into()],
            tls: true,
        });
        let resolved = t.resolve("app.example.com").expect("resolved");
        assert_eq!(resolved.service_name, "new");
        assert!(resolved.tls);
    }
}

#[cfg(test)]
mod validation_tests {
    use super::*;

    #[test]
    fn validate_domain_accepts_normal_domain() {
        assert_eq!(
            validate_domain("api.example.com").unwrap(),
            "api.example.com"
        );
    }

    #[test]
    fn validate_domain_normalizes_uppercase_to_lower() {
        assert_eq!(
            validate_domain("API.Example.COM").unwrap(),
            "api.example.com"
        );
    }

    #[test]
    fn validate_domain_rejects_empty_and_whitespace() {
        assert!(matches!(
            validate_domain(""),
            Err(IngressError::InvalidDomain(_))
        ));
        assert!(matches!(
            validate_domain("   "),
            Err(IngressError::InvalidDomain(_))
        ));
    }

    #[test]
    fn validate_domain_rejects_backtick_and_crlf() {
        assert!(validate_domain("ev`il.com").is_err());
        assert!(validate_domain("evil.com\r").is_err());
        assert!(validate_domain("evil.com\n").is_err());
        assert!(validate_domain("ev il.com").is_err());
        assert!(validate_domain("ev\til.com").is_err());
    }

    #[test]
    fn validate_domain_rejects_non_ascii() {
        // Cyrillic homoglyph "а" (U+0430) — must be rejected, not silently routed.
        assert!(validate_domain("exаmple.com").is_err());
        assert!(validate_domain("xn--mnchen-3ya.de").is_ok()); // punycode ASCII OK
    }

    #[test]
    fn validate_domain_rejects_wildcards() {
        assert!(validate_domain("*.example.com").is_err());
        assert!(validate_domain("foo.*.com").is_err());
    }

    #[test]
    fn validate_domain_rejects_overlong_total_and_label() {
        let long_total = format!("{}.com", "a".repeat(252));
        assert!(validate_domain(&long_total).is_err());
        let long_label = format!("{}.com", "a".repeat(64));
        assert!(validate_domain(&long_label).is_err());
    }

    #[test]
    fn validate_domain_rejects_dot_edges_and_empty_labels() {
        assert!(validate_domain(".example.com").is_err());
        assert!(validate_domain("example.com.").is_err());
        assert!(validate_domain("a..b.com").is_err());
    }

    #[test]
    fn try_upsert_rejects_empty_service_name() {
        let mut t = RouteTable::default();
        assert_eq!(
            t.try_upsert(RouteSpec {
                route_key: "k".into(),
                service_name: "  ".into(),
                domains: vec!["a.example.com".into()],
                tls: false,
            }),
            Err(IngressError::EmptyServiceName)
        );
        assert_eq!(t.host_count(), 0);
    }

    #[test]
    fn try_upsert_rejects_missing_domains() {
        let mut t = RouteTable::default();
        assert_eq!(
            t.try_upsert(RouteSpec {
                route_key: "k".into(),
                service_name: "api".into(),
                domains: vec![],
                tls: false,
            }),
            Err(IngressError::MissingDomain)
        );
    }

    #[test]
    fn try_upsert_rejects_invalid_domain_and_inserts_nothing() {
        let mut t = RouteTable::default();
        assert!(matches!(
            t.try_upsert(RouteSpec {
                route_key: "k".into(),
                service_name: "api".into(),
                domains: vec!["good.example.com".into(), "ev`il.com".into()],
                tls: false,
            }),
            Err(IngressError::InvalidDomain(_))
        ));
        // Reject is all-or-nothing: no partial insertion.
        assert_eq!(t.host_count(), 0);
    }

    #[test]
    fn try_upsert_normalizes_domains_to_lowercase() {
        let mut t = RouteTable::default();
        t.try_upsert(RouteSpec {
            route_key: "k".into(),
            service_name: "api".into(),
            domains: vec!["API.Example.COM".into()],
            tls: false,
        })
        .unwrap();
        assert!(t.resolve("api.example.com").is_some());
    }

    #[test]
    fn resolve_lowercases_lookup_argument() {
        let mut t = RouteTable::default();
        t.try_upsert(RouteSpec {
            route_key: "k".into(),
            service_name: "api".into(),
            domains: vec!["api.example.com".into()],
            tls: false,
        })
        .unwrap();
        assert!(t.resolve("API.EXAMPLE.COM").is_some());
    }

    /// Throwaway self-signed EC test material (NOT a secret; generated solely
    /// for parsing in unit tests, never used on the wire).
    const TEST_KEY_PEM: &[u8] = b"-----BEGIN PRIVATE KEY-----\n\
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgZFwD6luyekuuSrp6\n\
jir4r0J1o+Lb2L1YFBR7HBJHCE2hRANCAATBJ6iTtPrDFPLnqcNA/87722/N255n\n\
xDZ2oRsDFpP735ud77NSPM8v0nRsW9nBm0C4JsOfznUnNCFfbQBs/3Rn\n\
-----END PRIVATE KEY-----\n";
    const TEST_CERT_PEM: &[u8] = b"-----BEGIN CERTIFICATE-----\n\
MIIBfzCCASWgAwIBAgIUT2TFIC8WbUryUcwKjixECF5vQoswCgYIKoZIzj0EAwIw\n\
FTETMBEGA1UEAwwKZGVuaWEtdGVzdDAeFw0yNjA1MjcxODQ1MjNaFw0zNjA1MjQx\n\
ODQ1MjNaMBUxEzARBgNVBAMMCmRlbmlhLXRlc3QwWTATBgcqhkjOPQIBBggqhkjO\n\
PQMBBwNCAATBJ6iTtPrDFPLnqcNA/87722/N255nxDZ2oRsDFpP735ud77NSPM8v\n\
0nRsW9nBm0C4JsOfznUnNCFfbQBs/3Rno1MwUTAdBgNVHQ4EFgQUQ+pPRiWYnXOs\n\
F7Gt+6mn7TM+MOYwHwYDVR0jBBgwFoAUQ+pPRiWYnXOsF7Gt+6mn7TM+MOYwDwYD\n\
VR0TAQH/BAUwAwEB/zAKBggqhkjOPQQDAgNIADBFAiEA7rINC49fiLX2DYAE06Cm\n\
7WYc7cctlyaUC0Nr9HUIgkQCIDQkV/AqQqzeDIL0B1zFwp8gttKI+dcUY0EOFPnf\n\
/bBZ\n\
-----END CERTIFICATE-----\n";

    /// Build a throwaway `ParsedCert` for cert-store tests by parsing the
    /// embedded test PEMs via the re-exported boring modules.
    fn fake_cert() -> ParsedCert {
        let key = PKey::private_key_from_pem(TEST_KEY_PEM).unwrap();
        let cert = X509::from_pem(TEST_CERT_PEM).unwrap();
        ParsedCert {
            chain: vec![cert],
            key,
        }
    }

    #[test]
    fn cert_store_insert_validates_and_lowercases_sni() {
        let mut store = CertStore::default();
        store
            .try_insert("API.Example.COM", fake_cert())
            .expect("valid sni");
        // Stored lowercased.
        assert!(store.get("api.example.com").is_some());
        // Lookup is also case-insensitive.
        assert!(store.get("API.EXAMPLE.COM").is_some());
    }

    #[test]
    fn cert_store_insert_rejects_invalid_sni() {
        let mut store = CertStore::default();
        assert!(matches!(
            store.try_insert("ev`il.com", fake_cert()),
            Err(IngressError::InvalidDomain(_))
        ));
        assert!(store.is_empty());
    }
}

#[cfg(test)]
mod assembly_tests {
    use super::*;

    #[test]
    fn cert_store_default_is_empty() {
        let store = CertStore::default();
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
        assert!(store.get("api.example.com").is_none());
    }

    #[tokio::test]
    async fn ingress_state_swaps_routes() {
        let state = IngressState::default();
        assert_eq!(state.routes().host_count(), 0);
        assert!(state.certs().is_empty());

        let mut table = RouteTable::default();
        table.upsert(RouteSpec {
            route_key: "svc-1".into(),
            service_name: "api".into(),
            domains: vec!["api.example.com".into()],
            tls: true,
        });
        state.swap_routes(table);

        // Swap is observable through a fresh snapshot.
        assert_eq!(state.routes().host_count(), 1);
        assert_eq!(
            state.resolve_host("api.example.com").as_deref(),
            Some("api")
        );
        assert_eq!(state.resolve_host("nope.example.com"), None);

        // Swapping a fresh (empty) cert store is observable too.
        state.swap_certs(CertStore::default());
        assert!(state.certs().is_empty());
    }
}

#[cfg(test)]
mod pool_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;

    #[tokio::test]
    async fn next_socket_round_robins_healthy_replicas() {
        let state = IngressState::default();
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        let path_a = PathBuf::from("/run/denia/a.sock");
        let path_b = PathBuf::from("/run/denia/b.sock");

        state.add_replica("svc", a, path_a.clone()).await;
        state.add_replica("svc", b, path_b.clone()).await;
        // Unhealthy by default → no selection.
        assert_eq!(state.healthy_count("svc").await, 0);
        assert_eq!(state.next_socket("svc").await, None);

        state.set_replica_healthy("svc", a, true).await;
        state.set_replica_healthy("svc", b, true).await;
        assert_eq!(state.healthy_count("svc").await, 2);

        let first = state.next_socket("svc").await.expect("first");
        let second = state.next_socket("svc").await.expect("second");
        let third = state.next_socket("svc").await.expect("third");
        assert_ne!(first, second);
        assert_eq!(first, third);
        assert!(first == path_a || first == path_b);

        // Mark one unhealthy → only the other is returned.
        state.set_replica_healthy("svc", a, false).await;
        assert_eq!(state.healthy_count("svc").await, 1);
        assert_eq!(state.next_socket("svc").await, Some(path_b.clone()));
        assert_eq!(state.next_socket("svc").await, Some(path_b.clone()));

        // Remove the remaining healthy endpoint → None.
        state.remove_replica("svc", b).await;
        assert_eq!(state.healthy_count("svc").await, 0);
        assert_eq!(state.next_socket("svc").await, None);
    }

    #[tokio::test]
    async fn next_socket_advances_last_activity() {
        let state = IngressState::default();
        let id = Uuid::now_v7();
        state
            .add_replica("svc", id, PathBuf::from("/run/denia/z.sock"))
            .await;
        state.set_replica_healthy("svc", id, true).await;

        let before = state.last_activity("svc").await.expect("activity");
        tokio::time::sleep(Duration::from_millis(5)).await;
        let _ = state.next_socket("svc").await.expect("socket");
        let after = state.last_activity("svc").await.expect("activity");
        assert!(after > before);
    }

    /// Fake activation hook: counts calls and, on success, registers a healthy
    /// replica so `resolve_or_activate` resolves after `activate` returns.
    struct FakeActivator {
        state: Arc<IngressState>,
        calls: Arc<AtomicUsize>,
        fail_first: Arc<AtomicBool>,
    }

    #[async_trait]
    impl ActivationHook for FakeActivator {
        async fn activate(&self, service: &str) -> Result<(), ActivationError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail_first.swap(false, Ordering::SeqCst) {
                return Err(ActivationError::Failed("boom".to_string()));
            }
            let id = Uuid::now_v7();
            self.state
                .add_replica(service, id, PathBuf::from("/run/denia/activated.sock"))
                .await;
            self.state.set_replica_healthy(service, id, true).await;
            Ok(())
        }
    }

    #[tokio::test]
    async fn zero_replicas_invokes_activation_hook() {
        let state = Arc::new(IngressState::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let hook = Arc::new(FakeActivator {
            state: state.clone(),
            calls: calls.clone(),
            fail_first: Arc::new(AtomicBool::new(false)),
        });
        state.set_activator(hook).await;

        // Pool starts scaled to zero.
        assert_eq!(state.healthy_count("svc").await, 0);

        let resolved = state.resolve_or_activate("svc").await.expect("resolve");
        assert_eq!(
            resolved,
            Some(PathBuf::from("/run/denia/activated.sock")),
            "activation should register a healthy replica and resolve to it"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "exactly one activation fired for the cold service"
        );
    }

    #[tokio::test]
    async fn no_activator_resolves_none() {
        let state = IngressState::default();
        state
            .add_replica("svc", Uuid::now_v7(), PathBuf::from("/run/denia/x.sock"))
            .await; // unhealthy
        assert_eq!(state.resolve_or_activate("svc").await.unwrap(), None);
    }

    #[tokio::test]
    async fn activation_failure_resets_latch() {
        let state = Arc::new(IngressState::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let hook = Arc::new(FakeActivator {
            state: state.clone(),
            calls: calls.clone(),
            fail_first: Arc::new(AtomicBool::new(true)),
        });
        state.set_activator(hook).await;

        // First attempt fails.
        assert!(state.resolve_or_activate("svc").await.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        // Latch reset: a second attempt runs a fresh (now succeeding) activation.
        let resolved = state.resolve_or_activate("svc").await.expect("resolve");
        assert!(resolved.is_some());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    /// Hook that blocks far longer than `ACTIVATION_WAIT`, simulating a hung
    /// controller, to prove the hot path is bounded (A4).
    struct HangingActivator;

    #[async_trait]
    impl ActivationHook for HangingActivator {
        async fn activate(&self, _service: &str) -> Result<(), ActivationError> {
            // Far longer than the (paused) ACTIVATION_WAIT clock will advance.
            tokio::time::sleep(Duration::from_secs(3600)).await;
            Ok(())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn activation_times_out_when_hook_hangs() {
        let state = IngressState::default();
        state.set_activator(Arc::new(HangingActivator)).await;

        let result = state.resolve_or_activate("svc").await;
        assert!(
            matches!(result, Err(ActivationError::Timeout)),
            "a hung activation must elapse ACTIVATION_WAIT and return Timeout, got {result:?}"
        );
    }

    /// Hook that counts calls and, after a small delay, registers a healthy
    /// replica. The delay widens the race window so concurrent waiters pile up
    /// on the single-flight gate.
    struct SlowCountingActivator {
        state: Arc<IngressState>,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl ActivationHook for SlowCountingActivator {
        async fn activate(&self, service: &str) -> Result<(), ActivationError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(50)).await;
            let id = Uuid::now_v7();
            self.state
                .add_replica(service, id, PathBuf::from("/run/denia/sf.sock"))
                .await;
            self.state.set_replica_healthy(service, id, true).await;
            Ok(())
        }
    }

    #[tokio::test]
    async fn concurrent_resolves_trigger_single_activation() {
        let state = Arc::new(IngressState::default());
        let calls = Arc::new(AtomicUsize::new(0));
        state
            .set_activator(Arc::new(SlowCountingActivator {
                state: state.clone(),
                calls: calls.clone(),
            }))
            .await;

        let mut set = tokio::task::JoinSet::new();
        for _ in 0..16 {
            let state = state.clone();
            set.spawn(async move { state.resolve_or_activate("svc").await });
        }

        let mut resolved = 0usize;
        while let Some(joined) = set.join_next().await {
            let socket = joined.expect("task join").expect("resolve");
            assert_eq!(socket, Some(PathBuf::from("/run/denia/sf.sock")));
            resolved += 1;
        }
        assert_eq!(resolved, 16);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "single-flight: exactly one activation for N concurrent cold-service requests"
        );
    }
}
