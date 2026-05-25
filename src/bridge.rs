use std::{collections::BTreeMap, net::SocketAddr, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use thiserror::Error;
use tokio::{
    io,
    net::{TcpListener, UnixStream},
    sync::{Mutex, oneshot},
    task::JoinHandle,
};

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

    pub fn assign(&mut self, service_name: &str, socket_path: PathBuf) -> BridgeTarget {
        if let Some(existing) = self.targets.get(service_name) {
            return existing.clone();
        }
        let target = BridgeTarget {
            service_name: service_name.to_string(),
            port: self.next_port,
            socket_path,
        };
        self.next_port += 1;
        self.targets
            .insert(service_name.to_string(), target.clone());
        target
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

#[derive(Default)]
pub struct LoopbackBridgeSupervisor {
    tasks: Mutex<BTreeMap<String, BridgeTask>>,
}

struct BridgeTask {
    shutdown: oneshot::Sender<()>,
    join: JoinHandle<()>,
}

pub struct LoopbackBridge {
    listener: TcpListener,
    socket_path: PathBuf,
}

impl LoopbackBridge {
    pub async fn bind(port: u16, socket_path: impl Into<PathBuf>) -> Result<Self, BridgeError> {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port))).await?;
        Ok(Self {
            listener,
            socket_path: socket_path.into(),
        })
    }

    pub fn local_port(&self) -> u16 {
        self.listener
            .local_addr()
            .expect("loopback bridge listener address")
            .port()
    }

    pub async fn serve_one(&self) -> Result<(), BridgeError> {
        let (mut tcp, _) = self.listener.accept().await?;
        let mut unix = UnixStream::connect(&self.socket_path).await?;
        io::copy_bidirectional(&mut tcp, &mut unix).await?;
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

#[async_trait]
impl BridgeManager for LoopbackBridgeSupervisor {
    async fn activate(&self, target: BridgeTarget) -> Result<(), BridgeError> {
        let bridge = LoopbackBridge::bind(target.port, target.socket_path.clone()).await?;
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let join = tokio::spawn(bridge.serve_until_shutdown(shutdown_rx));
        let replaced = self.tasks.lock().await.insert(
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
        if let Some(task) = self.tasks.lock().await.remove(service_name) {
            let _ = task.shutdown.send(());
            task.join.abort();
        }
        Ok(())
    }
}
