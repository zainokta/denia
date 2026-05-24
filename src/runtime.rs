use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use thiserror::Error;

use crate::domain::{RuntimeStartRequest, RuntimeStatus};

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("runtime lock poisoned")]
    LockPoisoned,
}

#[async_trait]
pub trait Runtime: Send + Sync {
    async fn start(&self, request: RuntimeStartRequest) -> Result<RuntimeStatus, RuntimeError>;
    async fn stop(&self, service_name: &str) -> Result<(), RuntimeError>;
}

#[derive(Debug, Default, Clone)]
pub struct FakeRuntime {
    started: Arc<Mutex<Vec<RuntimeStartRequest>>>,
    stopped: Arc<Mutex<Vec<String>>>,
}

impl FakeRuntime {
    pub fn stopped_services(&self) -> Vec<String> {
        self.stopped.lock().expect("stopped lock").clone()
    }
}

#[async_trait]
impl Runtime for FakeRuntime {
    async fn start(&self, request: RuntimeStartRequest) -> Result<RuntimeStatus, RuntimeError> {
        self.started
            .lock()
            .map_err(|_| RuntimeError::LockPoisoned)?
            .push(request.clone());
        Ok(RuntimeStatus {
            service_name: request.service_name,
            deployment_id: request.deployment_id,
            state: "running".to_string(),
            pid: Some(1234),
            cgroup_path: "/sys/fs/cgroup/denia/fake".into(),
            socket_path: request.socket_path,
        })
    }

    async fn stop(&self, service_name: &str) -> Result<(), RuntimeError> {
        self.stopped
            .lock()
            .map_err(|_| RuntimeError::LockPoisoned)?
            .push(service_name.to_string());
        Ok(())
    }
}
