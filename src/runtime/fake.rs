use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::domain::{JobOutcome, JobRunRequest, RuntimeStartRequest, RuntimeStatus};
use crate::runtime::error::RuntimeError;
use crate::runtime::runtime_trait::Runtime;

#[derive(Debug, Default, Clone)]
pub struct FakeRuntime {
    started: Arc<Mutex<Vec<RuntimeStartRequest>>>,
    stopped: Arc<Mutex<Vec<String>>>,
}

impl FakeRuntime {
    pub fn started_requests(&self) -> Vec<RuntimeStartRequest> {
        self.started.lock().expect("started lock").clone()
    }

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

    async fn run_to_completion(&self, _request: JobRunRequest) -> Result<JobOutcome, RuntimeError> {
        let now = chrono::Utc::now();
        Ok(JobOutcome {
            exit_code: 0,
            started_at: now,
            finished_at: now,
        })
    }
}
