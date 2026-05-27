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

use async_trait::async_trait;
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
    /// If a domain previously pointed at a different `route_key`, the most
    /// recent `upsert` wins for that host.
    pub fn upsert(&mut self, spec: RouteSpec) {
        for domain in &spec.domains {
            self.by_host.insert(domain.clone(), spec.clone());
        }
    }

    /// Remove every host entry owned by `route_key`.
    pub fn remove(&mut self, route_key: &str) {
        self.by_host.retain(|_, spec| spec.route_key != route_key);
    }

    /// Resolve a request `Host` to its owning route, if any.
    pub fn resolve(&self, host: &str) -> Option<&RouteSpec> {
        self.by_host.get(host)
    }

    /// Number of distinct host entries (for diagnostics/tests).
    pub fn host_count(&self) -> usize {
        self.by_host.len()
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
            t.resolve("api.example.com").map(|r| r.service_name.as_str()),
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
}
