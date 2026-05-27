use std::sync::Arc;

use async_trait::async_trait;

use crate::domain::{
    JobOutcome, JobRunRequest, RuntimeInstanceId, RuntimeStartRequest, RuntimeStatus,
};
use crate::runtime::error::RuntimeError;

#[async_trait]
pub trait Runtime: Send + Sync {
    async fn start(&self, request: RuntimeStartRequest) -> Result<RuntimeStatus, RuntimeError>;
    async fn stop(&self, instance: &RuntimeInstanceId) -> Result<(), RuntimeError>;
    async fn list_running(&self) -> Result<Vec<RuntimeStatus>, RuntimeError> {
        Ok(Vec::new())
    }
    async fn run_to_completion(&self, _request: JobRunRequest) -> Result<JobOutcome, RuntimeError> {
        Err(RuntimeError::InvalidServiceName {
            name: "run_to_completion not implemented".to_string(),
        })
    }
}

#[async_trait]
impl<T> Runtime for Arc<T>
where
    T: Runtime + ?Sized,
{
    async fn start(&self, request: RuntimeStartRequest) -> Result<RuntimeStatus, RuntimeError> {
        (**self).start(request).await
    }

    async fn stop(&self, instance: &RuntimeInstanceId) -> Result<(), RuntimeError> {
        (**self).stop(instance).await
    }

    async fn list_running(&self) -> Result<Vec<RuntimeStatus>, RuntimeError> {
        (**self).list_running().await
    }

    async fn run_to_completion(&self, request: JobRunRequest) -> Result<JobOutcome, RuntimeError> {
        (**self).run_to_completion(request).await
    }
}
