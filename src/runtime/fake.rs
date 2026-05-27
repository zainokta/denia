use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::domain::{
    JobOutcome, JobRunRequest, RuntimeInstanceId, RuntimeStartRequest, RuntimeStatus,
};
use crate::runtime::error::RuntimeError;
use crate::runtime::runtime_trait::Runtime;

#[derive(Debug, Default, Clone)]
pub struct FakeRuntime {
    started: Arc<Mutex<Vec<RuntimeStartRequest>>>,
    stopped: Arc<Mutex<Vec<RuntimeInstanceId>>>,
    running: Arc<Mutex<Vec<RuntimeStatus>>>,
}

impl FakeRuntime {
    pub fn started_requests(&self) -> Vec<RuntimeStartRequest> {
        self.started.lock().expect("started lock").clone()
    }

    pub fn stopped_services(&self) -> Vec<String> {
        self.stopped
            .lock()
            .expect("stopped lock")
            .iter()
            .map(|instance| instance.service_name.clone())
            .collect()
    }

    pub fn stopped_instances(&self) -> Vec<RuntimeInstanceId> {
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
        let status = RuntimeStatus {
            service_id: request.service_id,
            service_name: request.service_name,
            deployment_id: request.deployment_id,
            state: "running".to_string(),
            pid: Some(1234),
            cgroup_path: "/sys/fs/cgroup/denia/fake".into(),
            socket_path: request.socket_path,
            replica_index: request.replica_index,
        };
        self.running
            .lock()
            .map_err(|_| RuntimeError::LockPoisoned)?
            .push(status.clone());
        Ok(status)
    }

    async fn stop(&self, instance: &RuntimeInstanceId) -> Result<(), RuntimeError> {
        self.stopped
            .lock()
            .map_err(|_| RuntimeError::LockPoisoned)?
            .push(instance.clone());
        self.running
            .lock()
            .map_err(|_| RuntimeError::LockPoisoned)?
            .retain(|status| {
                status.service_name != instance.service_name
                    || status.replica_index != instance.replica_index
            });
        Ok(())
    }

    async fn list_running(&self) -> Result<Vec<RuntimeStatus>, RuntimeError> {
        Ok(self
            .running
            .lock()
            .map_err(|_| RuntimeError::LockPoisoned)?
            .clone())
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
