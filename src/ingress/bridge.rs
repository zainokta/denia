use std::{collections::BTreeMap, net::SocketAddr, path::PathBuf, sync::Arc, time::Instant};

use async_trait::async_trait;
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, UnixStream},
    sync::{Mutex, oneshot},
    task::JoinHandle,
    time::{Duration, timeout},
};
use uuid::Uuid;

use crate::access_log::{AccessEntry, AccessLogStore, parse_request_line, parse_status_line};

/// Maximum time a connection waits for a cold-start activation to produce a
/// healthy replica before giving up with a 503.
const ACTIVATION_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Number of times a waiter re-checks `next_socket` after a successful
/// activation before treating the absence of a socket as a failure. A handful
/// of short retries covers the brief window where a replica is healthy but its
/// socket assignment is still settling.
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

/// A single replica's loopback Unix endpoint within a service's bridge pool.
#[derive(Debug, Clone)]
pub struct ReplicaEndpoint {
    pub replica_id: Uuid,
    pub socket_path: PathBuf,
    pub healthy: bool,
}

/// Per-service fan-out state: the set of replica endpoints, a round-robin
/// cursor over the healthy ones, and the last time a connection was proxied.
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeTarget {
    pub service_name: String,
    pub port: u16,
    pub socket_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct BridgeAllocator {
    next_port: u16,
    targets: BTreeMap<String, BridgeTarget>,
}

impl BridgeAllocator {
    pub fn new(start_port: u16) -> Self {
        Self {
            next_port: start_port,
            targets: BTreeMap::new(),
        }
    }

    pub fn assign(&mut self, service_name: &str, socket_path: PathBuf) -> Option<BridgeTarget> {
        if let Some(existing) = self.targets.get(service_name) {
            return Some(existing.clone());
        }
        let port = self.next_port;
        if port == 65535 {
            return None;
        }
        let target = BridgeTarget {
            service_name: service_name.to_string(),
            port,
            socket_path,
        };
        self.next_port = port.wrapping_add(1);
        self.targets
            .insert(service_name.to_string(), target.clone());
        Some(target)
    }
}

#[derive(Debug, Error)]
pub enum BridgeError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("bridge lock poisoned")]
    LockPoisoned,
}

#[async_trait]
pub trait BridgeManager: Send + Sync {
    async fn activate(&self, target: BridgeTarget) -> Result<(), BridgeError>;
    async fn deactivate(&self, service_name: &str) -> Result<(), BridgeError>;
}

#[async_trait]
impl<T> BridgeManager for Arc<T>
where
    T: BridgeManager + ?Sized,
{
    async fn activate(&self, target: BridgeTarget) -> Result<(), BridgeError> {
        (**self).activate(target).await
    }

    async fn deactivate(&self, service_name: &str) -> Result<(), BridgeError> {
        (**self).deactivate(service_name).await
    }
}

#[derive(Debug, Default, Clone)]
pub struct FakeBridgeManager {
    activated: Arc<std::sync::Mutex<Vec<BridgeTarget>>>,
    deactivated: Arc<std::sync::Mutex<Vec<String>>>,
}

impl FakeBridgeManager {
    pub fn activated_targets(&self) -> Vec<BridgeTarget> {
        self.activated.lock().expect("activated lock").clone()
    }

    pub fn deactivated_services(&self) -> Vec<String> {
        self.deactivated.lock().expect("deactivated lock").clone()
    }
}

#[async_trait]
impl BridgeManager for FakeBridgeManager {
    async fn activate(&self, target: BridgeTarget) -> Result<(), BridgeError> {
        self.activated
            .lock()
            .map_err(|_| BridgeError::LockPoisoned)?
            .push(target);
        Ok(())
    }

    async fn deactivate(&self, service_name: &str) -> Result<(), BridgeError> {
        self.deactivated
            .lock()
            .map_err(|_| BridgeError::LockPoisoned)?
            .push(service_name.to_string());
        Ok(())
    }
}

/// Stable replica id used for the single endpoint registered by `activate`.
/// `activate` carries no replica identity, so a fixed nil-derived id keeps the
/// initial endpoint addressable (and replaceable) for single-instance services.
const ACTIVATE_REPLICA_ID: Uuid = Uuid::nil();

#[derive(Default, Clone)]
pub struct LoopbackBridgeSupervisor {
    inner: Arc<LoopbackBridgeInner>,
}

#[derive(Default)]
struct LoopbackBridgeInner {
    tasks: Mutex<BTreeMap<String, BridgeTask>>,
    pools: Mutex<BTreeMap<String, ServicePool>>,
    access_log: AccessLogStore,
    /// Optional cold-start hook injected by the controller. When unset, the
    /// listener falls back to closing connections with zero healthy replicas.
    activator: Mutex<Option<Arc<dyn ActivationHook>>>,
    /// Per-service single-flight gate. Connections that find no healthy replica
    /// serialize on the service's gate; the first holder that still sees no
    /// socket runs the activation, later holders re-check and reuse the result.
    activation_gates: Mutex<BTreeMap<String, Arc<Mutex<()>>>>,
}

impl LoopbackBridgeInner {
    /// Round-robin the next healthy socket for `service` and record activity.
    async fn next_socket(&self, service: &str) -> Option<PathBuf> {
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
    /// Single-flight: connections serialize on the per-service gate. The first
    /// holder that still observes no socket calls `activate`; concurrent
    /// waiters block on the gate, then re-check and reuse the now-healthy
    /// socket without a second activation. On activation failure the gate is
    /// released, so the next connection starts a fresh activation (no stuck
    /// latch). Returns `Ok(None)` only when no activator is configured.
    async fn resolve_or_activate(&self, service: &str) -> Result<Option<PathBuf>, ActivationError> {
        if let Some(socket) = self.next_socket(service).await {
            return Ok(Some(socket));
        }
        let activator = { self.activator.lock().await.clone() };
        let Some(activator) = activator else {
            return Ok(None);
        };

        let gate = self.activation_gate(service).await;
        let _guard = gate.lock().await;

        // Another connection may have activated while we waited on the gate.
        if let Some(socket) = self.next_socket(service).await {
            return Ok(Some(socket));
        }

        activator.activate(service).await?;

        // `activate` only returns Ok once a replica is healthy; allow a few
        // short retries for the socket selection to settle.
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

impl LoopbackBridgeSupervisor {
    pub fn with_access_log(access_log: AccessLogStore) -> Self {
        Self {
            inner: Arc::new(LoopbackBridgeInner {
                tasks: Mutex::new(BTreeMap::new()),
                pools: Mutex::new(BTreeMap::new()),
                access_log,
                activator: Mutex::new(None),
                activation_gates: Mutex::new(BTreeMap::new()),
            }),
        }
    }

    pub fn access_log(&self) -> AccessLogStore {
        self.inner.access_log.clone()
    }

    /// Inject the cold-start activation hook. Until set, connections to a
    /// scaled-to-zero service are closed rather than triggering a launch.
    pub async fn set_activator(&self, hook: Arc<dyn ActivationHook>) {
        *self.inner.activator.lock().await = Some(hook);
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
        let mut pools = self.inner.pools.lock().await;
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
        let mut pools = self.inner.pools.lock().await;
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
        let mut pools = self.inner.pools.lock().await;
        if let Some(pool) = pools.get_mut(service) {
            pool.endpoints.retain(|e| e.replica_id != replica_id);
            if pool.cursor >= pool.endpoints.len() {
                pool.cursor = 0;
            }
        }
    }

    /// Number of healthy endpoints for `service`.
    pub async fn healthy_count(&self, service: &str) -> usize {
        let pools = self.inner.pools.lock().await;
        pools
            .get(service)
            .map(ServicePool::healthy_count)
            .unwrap_or(0)
    }

    /// Last time a connection was proxied for `service`, if the pool exists.
    pub async fn last_activity(&self, service: &str) -> Option<Instant> {
        let pools = self.inner.pools.lock().await;
        pools.get(service).map(|pool| pool.last_activity)
    }

    /// Set the recorded `last_activity` for `service`, creating the pool entry
    /// if it does not yet exist. Primarily for backdating activity in tests; in
    /// production the proxy path advances `last_activity` to `Instant::now()`.
    pub async fn set_last_activity(&self, service: &str, when: Instant) {
        let mut pools = self.inner.pools.lock().await;
        pools
            .entry(service.to_string())
            .or_insert_with(ServicePool::new)
            .last_activity = when;
    }

    /// Round-robin the next healthy socket for `service`, advancing the cursor
    /// and updating `last_activity`. `None` if no healthy endpoint exists.
    pub async fn next_socket(&self, service: &str) -> Option<PathBuf> {
        self.inner.next_socket(service).await
    }
}

struct BridgeTask {
    shutdown: oneshot::Sender<()>,
    join: JoinHandle<()>,
}

pub struct LoopbackBridge {
    listener: TcpListener,
    service_name: String,
    access_log: AccessLogStore,
    connection_sem: Arc<tokio::sync::Semaphore>,
    pools: Arc<LoopbackBridgeInner>,
}

const BRIDGE_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CONCURRENT_PER_BRIDGE: usize = 256;

impl LoopbackBridge {
    /// Bind a per-service TCP listener that fans out across the supervisor's
    /// replica pool for `service_name`. The target Unix socket is resolved per
    /// connection via the pool's round-robin selection.
    async fn bind_with_pool(
        port: u16,
        service_name: String,
        pools: Arc<LoopbackBridgeInner>,
    ) -> Result<Self, BridgeError> {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port))).await?;
        let access_log = pools.access_log.clone();
        Ok(Self {
            listener,
            service_name,
            access_log,
            connection_sem: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_PER_BRIDGE)),
            pools,
        })
    }

    pub fn local_port(&self) -> u16 {
        self.listener
            .local_addr()
            .expect("loopback bridge listener address")
            .port()
    }

    pub async fn serve_one(&self) -> Result<(), BridgeError> {
        let permit = self
            .connection_sem
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| BridgeError::Io(std::io::Error::other("semaphore closed")))?;
        let (mut tcp, _) = self.listener.accept().await?;
        // Resolve a healthy replica socket per connection. With zero healthy
        // endpoints, single-flight a cold-start activation (if an activator is
        // configured) and hold the connection until a replica is healthy; on
        // failure/timeout reply 503. With no activator, close cleanly.
        let resolved = tokio::time::timeout(
            ACTIVATION_WAIT,
            self.pools.resolve_or_activate(&self.service_name),
        )
        .await;
        let socket_path = match resolved {
            Ok(Ok(Some(path))) => path,
            Ok(Ok(None)) => {
                // No activator configured: preserve close-on-zero behavior.
                drop(tcp);
                return Ok(());
            }
            Ok(Err(_)) | Err(_) => {
                // Activation failed/timed out: 503 this connection. The gate is
                // already released, so a later connection retries fresh.
                let _ = tcp
                    .write_all(
                        b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await;
                let _ = tcp.shutdown().await;
                return Ok(());
            }
        };
        let unix = match UnixStream::connect(&socket_path).await {
            Ok(unix) => unix,
            Err(_) => {
                drop(tcp);
                return Ok(());
            }
        };
        let log = self.access_log.clone();
        let service = self.service_name.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let _ = tee_proxy(tcp, unix, service, log).await;
        });
        Ok(())
    }

    async fn serve_until_shutdown(self, mut shutdown: oneshot::Receiver<()>) {
        loop {
            tokio::select! {
                _ = &mut shutdown => break,
                result = self.serve_one() => {
                    if result.is_err() {
                        break;
                    }
                }
            }
        }
    }
}

async fn tee_proxy(
    tcp: tokio::net::TcpStream,
    unix: UnixStream,
    service_name: String,
    access_log: AccessLogStore,
) -> std::io::Result<()> {
    let (mut tcp_read, mut tcp_write) = tokio::io::split(tcp);
    let (mut unix_read, mut unix_write) = tokio::io::split(unix);
    let started = Instant::now();

    let mut request_line: Option<(String, String)> = None;
    let mut req_bytes: u64 = 0;
    let mut head_buf = Vec::with_capacity(1024);

    let req_done = async {
        loop {
            let mut byte = [0u8; 1];
            let n = match timeout(BRIDGE_IDLE_TIMEOUT, tcp_read.read(&mut byte)).await {
                Ok(Ok(n)) => n,
                _ => break,
            };
            if n == 0 {
                break;
            }
            req_bytes += 1;
            if request_line.is_none() && head_buf.len() < 8192 {
                head_buf.extend_from_slice(&byte);
                if let Some(pos) = head_buf.iter().position(|&b| b == b'\n') {
                    let line = String::from_utf8_lossy(&head_buf[..pos]);
                    request_line = parse_request_line(line.trim_end_matches('\r'));
                }
            }
            unix_write.write_all(&byte).await?;
            if byte[0] == b'\n' && head_buf.ends_with(b"\r\n\r\n") || head_buf.ends_with(b"\n\n") {
                break;
            }
        }
        let mut rest = [0u8; 8192];
        loop {
            let n = match timeout(BRIDGE_IDLE_TIMEOUT, tcp_read.read(&mut rest)).await {
                Ok(Ok(n)) => n,
                _ => break,
            };
            if n == 0 {
                break;
            }
            req_bytes += n as u64;
            unix_write.write_all(&rest[..n]).await?;
        }
        unix_write.shutdown().await.ok();
        Ok::<_, std::io::Error>(())
    };

    let mut status_code: Option<u16> = None;
    let mut resp_bytes: u64 = 0;
    let mut resp_head = Vec::with_capacity(256);
    let resp_done = async {
        loop {
            let mut byte = [0u8; 1];
            let n = match timeout(BRIDGE_IDLE_TIMEOUT, unix_read.read(&mut byte)).await {
                Ok(Ok(n)) => n,
                _ => break,
            };
            if n == 0 {
                break;
            }
            resp_bytes += 1;
            if status_code.is_none() && resp_head.len() < 1024 {
                resp_head.extend_from_slice(&byte);
                if let Some(pos) = resp_head.iter().position(|&b| b == b'\n') {
                    let line = String::from_utf8_lossy(&resp_head[..pos]);
                    status_code = parse_status_line(line.trim_end_matches('\r'));
                }
            }
            tcp_write.write_all(&byte).await?;
            if status_code.is_some() {
                break;
            }
        }
        let mut rest = [0u8; 8192];
        loop {
            let n = match timeout(BRIDGE_IDLE_TIMEOUT, unix_read.read(&mut rest)).await {
                Ok(Ok(n)) => n,
                _ => break,
            };
            if n == 0 {
                break;
            }
            resp_bytes += n as u64;
            tcp_write.write_all(&rest[..n]).await?;
        }
        tcp_write.shutdown().await.ok();
        Ok::<_, std::io::Error>(())
    };

    let (a, b) = tokio::join!(req_done, resp_done);
    let _ = req_bytes;
    a?;
    b?;

    if !service_name.is_empty()
        && let Some((method, path)) = request_line
    {
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        access_log.append(AccessEntry {
            service_name,
            method,
            path,
            status: status_code.unwrap_or(0),
            bytes: Some(resp_bytes),
            duration_ms: Some(duration_ms),
            recorded_at: chrono::Utc::now().to_rfc3339(),
        });
    }
    Ok(())
}

#[async_trait]
impl BridgeManager for LoopbackBridgeSupervisor {
    async fn activate(&self, target: BridgeTarget) -> Result<(), BridgeError> {
        // Register the target socket as the service's first (healthy) replica
        // endpoint, preserving single-instance behavior: one listener fans out
        // to one healthy socket. A stable nil-derived replica id keeps this
        // endpoint replaceable on re-activation.
        self.add_replica_with_health(
            &target.service_name,
            ACTIVATE_REPLICA_ID,
            target.socket_path.clone(),
            true,
        )
        .await;

        let bridge = LoopbackBridge::bind_with_pool(
            target.port,
            target.service_name.clone(),
            self.inner.clone(),
        )
        .await?;
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let join = tokio::spawn(bridge.serve_until_shutdown(shutdown_rx));
        let replaced = self.inner.tasks.lock().await.insert(
            target.service_name.clone(),
            BridgeTask {
                shutdown: shutdown_tx,
                join,
            },
        );
        if let Some(task) = replaced {
            let _ = task.shutdown.send(());
            task.join.abort();
        }
        Ok(())
    }

    async fn deactivate(&self, service_name: &str) -> Result<(), BridgeError> {
        if let Some(task) = self.inner.tasks.lock().await.remove(service_name) {
            let _ = task.shutdown.send(());
            task.join.abort();
        }
        self.inner.pools.lock().await.remove(service_name);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpStream, UnixListener};

    fn socket_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir();
        let unique = format!(
            "denia-bridge-{tag}-{}-{}.sock",
            std::process::id(),
            Uuid::now_v7()
        );
        dir.join(unique)
    }

    #[tokio::test]
    async fn pool_round_robin_over_healthy() {
        let sup = LoopbackBridgeSupervisor::default();
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        let path_a = PathBuf::from("/run/denia/a.sock");
        let path_b = PathBuf::from("/run/denia/b.sock");

        sup.add_replica("svc", a, path_a.clone()).await;
        sup.add_replica("svc", b, path_b.clone()).await;
        // Added unhealthy by default → no selection yet.
        assert_eq!(sup.healthy_count("svc").await, 0);
        assert_eq!(sup.next_socket("svc").await, None);

        sup.set_replica_healthy("svc", a, true).await;
        sup.set_replica_healthy("svc", b, true).await;
        assert_eq!(sup.healthy_count("svc").await, 2);

        // Round-robin alternates across the two healthy endpoints.
        let first = sup.next_socket("svc").await.expect("first");
        let second = sup.next_socket("svc").await.expect("second");
        let third = sup.next_socket("svc").await.expect("third");
        assert_ne!(first, second);
        assert_eq!(first, third);
        assert!(first == path_a || first == path_b);

        // Mark one unhealthy → only the other is returned.
        sup.set_replica_healthy("svc", a, false).await;
        assert_eq!(sup.healthy_count("svc").await, 1);
        assert_eq!(sup.next_socket("svc").await, Some(path_b.clone()));
        assert_eq!(sup.next_socket("svc").await, Some(path_b.clone()));

        // Remove the remaining healthy endpoint → None.
        sup.remove_replica("svc", b).await;
        assert_eq!(sup.healthy_count("svc").await, 0);
        assert_eq!(sup.next_socket("svc").await, None);
    }

    #[tokio::test]
    async fn next_socket_none_when_no_healthy() {
        let sup = LoopbackBridgeSupervisor::default();
        sup.add_replica("svc", Uuid::now_v7(), PathBuf::from("/run/denia/x.sock"))
            .await;
        sup.add_replica("svc", Uuid::now_v7(), PathBuf::from("/run/denia/y.sock"))
            .await;
        assert_eq!(sup.healthy_count("svc").await, 0);
        assert_eq!(sup.next_socket("svc").await, None);
        // Unknown service has no pool.
        assert_eq!(sup.next_socket("missing").await, None);
        assert_eq!(sup.last_activity("missing").await, None);
    }

    #[tokio::test]
    async fn next_socket_advances_last_activity() {
        let sup = LoopbackBridgeSupervisor::default();
        let id = Uuid::now_v7();
        sup.add_replica("svc", id, PathBuf::from("/run/denia/z.sock"))
            .await;
        sup.set_replica_healthy("svc", id, true).await;

        let before = sup.last_activity("svc").await.expect("activity");
        tokio::time::sleep(Duration::from_millis(5)).await;
        let _ = sup.next_socket("svc").await.expect("socket");
        let after = sup.last_activity("svc").await.expect("activity");
        assert!(after > before);
    }

    #[tokio::test]
    async fn set_last_activity_round_trips() {
        let sup = LoopbackBridgeSupervisor::default();
        // No pool yet: set creates the entry, get reads it back.
        let when = Instant::now() - Duration::from_secs(500);
        sup.set_last_activity("svc", when).await;
        assert_eq!(sup.last_activity("svc").await, Some(when));

        // Overwrite with a newer instant.
        let later = Instant::now();
        sup.set_last_activity("svc", later).await;
        assert_eq!(sup.last_activity("svc").await, Some(later));
    }

    /// Spawn a Unix listener that replies with a fixed body tagged per socket,
    /// recording how many connections it served.
    fn spawn_echo_socket(path: PathBuf, tag: &'static str) -> Arc<std::sync::atomic::AtomicUsize> {
        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let listener = UnixListener::bind(&path).expect("bind unix socket");
        let count = counter.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let tag = tag.to_string();
                tokio::spawn(async move {
                    // Drain the request head, then reply with a tagged response.
                    let mut buf = [0u8; 1024];
                    let _ = stream.read(&mut buf).await;
                    let body = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{tag}",
                        tag.len()
                    );
                    let _ = stream.write_all(body.as_bytes()).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        counter
    }

    async fn send_request(port: u16) -> String {
        let mut tcp = TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("connect");
        tcp.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .expect("write");
        tcp.shutdown().await.ok();
        let mut resp = String::new();
        tcp.read_to_string(&mut resp).await.expect("read");
        resp
    }

    #[tokio::test]
    async fn listener_fans_out_round_robin_across_two_sockets() {
        let sup = LoopbackBridgeSupervisor::default();

        let path_a = socket_path("a");
        let path_b = socket_path("b");
        let count_a = spawn_echo_socket(path_a.clone(), "AAA");
        let count_b = spawn_echo_socket(path_b.clone(), "BBB");

        let id_a = Uuid::now_v7();
        let id_b = Uuid::now_v7();
        sup.add_replica("svc", id_a, path_a.clone()).await;
        sup.add_replica("svc", id_b, path_b.clone()).await;
        sup.set_replica_healthy("svc", id_a, true).await;
        sup.set_replica_healthy("svc", id_b, true).await;

        // Bind a listener on an ephemeral port that fans out via the pool.
        let bridge = LoopbackBridge::bind_with_pool(0, "svc".to_string(), sup.inner.clone())
            .await
            .expect("bind");
        let port = bridge.local_port();
        let (_tx, rx) = oneshot::channel();
        tokio::spawn(bridge.serve_until_shutdown(rx));

        let before = sup.last_activity("svc").await.expect("activity");
        tokio::time::sleep(Duration::from_millis(5)).await;

        let r1 = send_request(port).await;
        let r2 = send_request(port).await;

        // Two connections reached two different sockets (one each).
        assert!(r1.ends_with("AAA") || r1.ends_with("BBB"));
        assert!(r2.ends_with("AAA") || r2.ends_with("BBB"));
        assert_ne!(
            r1.split("\r\n\r\n").nth(1),
            r2.split("\r\n\r\n").nth(1),
            "round-robin should hit distinct sockets"
        );
        assert_eq!(count_a.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(count_b.load(std::sync::atomic::Ordering::SeqCst), 1);

        let after = sup.last_activity("svc").await.expect("activity");
        assert!(
            after > before,
            "last_activity must advance on proxied conns"
        );

        let _ = std::fs::remove_file(&path_a);
        let _ = std::fs::remove_file(&path_b);
    }

    /// Fake activation hook: counts calls and, when configured to succeed,
    /// registers a healthy replica (backed by a live echo socket) in the
    /// supervisor so `next_socket` resolves after `activate` returns.
    struct FakeActivator {
        sup: LoopbackBridgeSupervisor,
        calls: Arc<std::sync::atomic::AtomicUsize>,
        fail_first: Arc<std::sync::atomic::AtomicBool>,
        socket_path: PathBuf,
    }

    #[async_trait]
    impl ActivationHook for FakeActivator {
        async fn activate(&self, service: &str) -> Result<(), ActivationError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self
                .fail_first
                .swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                return Err(ActivationError::Failed("boom".to_string()));
            }
            let id = Uuid::now_v7();
            self.sup
                .add_replica(service, id, self.socket_path.clone())
                .await;
            self.sup.set_replica_healthy(service, id, true).await;
            Ok(())
        }
    }

    #[tokio::test]
    async fn single_flight_one_launch_for_concurrent_connections() {
        let sup = LoopbackBridgeSupervisor::default();
        let path = socket_path("activate-sf");
        let _count = spawn_echo_socket(path.clone(), "OK");

        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hook = Arc::new(FakeActivator {
            sup: sup.clone(),
            calls: calls.clone(),
            fail_first: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            socket_path: path.clone(),
        });
        sup.set_activator(hook).await;

        // Pool starts with zero replicas (scaled to zero).
        assert_eq!(sup.healthy_count("svc").await, 0);

        let bridge = LoopbackBridge::bind_with_pool(0, "svc".to_string(), sup.inner.clone())
            .await
            .expect("bind");
        let port = bridge.local_port();
        let (_tx, rx) = oneshot::channel();
        tokio::spawn(bridge.serve_until_shutdown(rx));

        // Fire N concurrent connections at the cold service.
        let mut handles = Vec::new();
        for _ in 0..6 {
            handles.push(tokio::spawn(async move { send_request(port).await }));
        }
        for h in handles {
            let resp = h.await.expect("join");
            assert!(resp.ends_with("OK"), "expected proxied body, got: {resp:?}");
        }

        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "exactly one activation for concurrent cold-start connections"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn activation_failure_returns_503_and_resets_latch() {
        let sup = LoopbackBridgeSupervisor::default();
        let path = socket_path("activate-503");
        let _count = spawn_echo_socket(path.clone(), "OK");

        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hook = Arc::new(FakeActivator {
            sup: sup.clone(),
            calls: calls.clone(),
            fail_first: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            socket_path: path.clone(),
        });
        sup.set_activator(hook).await;

        let bridge = LoopbackBridge::bind_with_pool(0, "svc".to_string(), sup.inner.clone())
            .await
            .expect("bind");
        let port = bridge.local_port();
        let (_tx, rx) = oneshot::channel();
        tokio::spawn(bridge.serve_until_shutdown(rx));

        // First connection: activation fails → 503.
        let r1 = send_request(port).await;
        assert!(
            r1.contains("503"),
            "expected 503 on activation failure, got: {r1:?}"
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);

        // Latch reset: a NEW connection triggers a second (now succeeding)
        // activation and is proxied.
        let r2 = send_request(port).await;
        assert!(
            r2.ends_with("OK"),
            "expected proxied body after retry, got: {r2:?}"
        );
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "second activation must run (no stuck latch)"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn no_activator_closes_connection() {
        let sup = LoopbackBridgeSupervisor::default();
        sup.add_replica("svc", Uuid::now_v7(), PathBuf::from("/run/denia/none.sock"))
            .await; // unhealthy

        let bridge = LoopbackBridge::bind_with_pool(0, "svc".to_string(), sup.inner.clone())
            .await
            .expect("bind");
        let port = bridge.local_port();
        let (_tx, rx) = oneshot::channel();
        tokio::spawn(bridge.serve_until_shutdown(rx));

        let mut tcp = TcpStream::connect(("127.0.0.1", port))
            .await
            .expect("connect");
        tcp.write_all(b"GET / HTTP/1.1\r\n\r\n").await.ok();
        let mut resp = String::new();
        // Connection should be closed with no body: either clean EOF (empty
        // read) or a reset, both acceptable for a dropped connection.
        match tcp.read_to_string(&mut resp).await {
            Ok(_) => assert!(resp.is_empty(), "expected closed connection, got: {resp:?}"),
            Err(err) => assert_eq!(err.kind(), std::io::ErrorKind::ConnectionReset),
        }
    }
}
