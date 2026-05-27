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

use crate::access_log::{AccessEntry, AccessLogStore, parse_request_line, parse_status_line};

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

#[derive(Default, Clone)]
pub struct LoopbackBridgeSupervisor {
    inner: Arc<LoopbackBridgeInner>,
}

#[derive(Default)]
struct LoopbackBridgeInner {
    tasks: Mutex<BTreeMap<String, BridgeTask>>,
    access_log: AccessLogStore,
}

impl LoopbackBridgeSupervisor {
    pub fn with_access_log(access_log: AccessLogStore) -> Self {
        Self {
            inner: Arc::new(LoopbackBridgeInner {
                tasks: Mutex::new(BTreeMap::new()),
                access_log,
            }),
        }
    }

    pub fn access_log(&self) -> AccessLogStore {
        self.inner.access_log.clone()
    }
}

struct BridgeTask {
    shutdown: oneshot::Sender<()>,
    join: JoinHandle<()>,
}

pub struct LoopbackBridge {
    listener: TcpListener,
    socket_path: PathBuf,
    service_name: String,
    access_log: AccessLogStore,
    connection_sem: Arc<tokio::sync::Semaphore>,
}

const BRIDGE_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CONCURRENT_PER_BRIDGE: usize = 256;

impl LoopbackBridge {
    pub async fn bind(port: u16, socket_path: impl Into<PathBuf>) -> Result<Self, BridgeError> {
        Self::bind_with_log(port, socket_path, String::new(), AccessLogStore::new()).await
    }

    pub async fn bind_with_log(
        port: u16,
        socket_path: impl Into<PathBuf>,
        service_name: String,
        access_log: AccessLogStore,
    ) -> Result<Self, BridgeError> {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port))).await?;
        Ok(Self {
            listener,
            socket_path: socket_path.into(),
            service_name,
            access_log,
            connection_sem: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_PER_BRIDGE)),
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
        let (tcp, _) = self.listener.accept().await?;
        let unix = UnixStream::connect(&self.socket_path).await?;
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
        let bridge = LoopbackBridge::bind_with_log(
            target.port,
            target.socket_path.clone(),
            target.service_name.clone(),
            self.inner.access_log.clone(),
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
        Ok(())
    }
}
