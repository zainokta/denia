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
    /// Reap workloads left over from a previous (unclean) daemon session.
    ///
    /// `list_running` only reflects in-memory tracking, which is empty on a
    /// fresh process, so it cannot see survivors of a SIGKILL/crash/power-loss.
    /// Implementations scan persisted runtime state (filesystem + cgroups) and
    /// tear down anything still present, returning how many were swept. The
    /// default is a no-op for runtimes without persisted state (e.g. fakes).
    async fn sweep_orphans(&self) -> Result<usize, RuntimeError> {
        Ok(0)
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

    async fn sweep_orphans(&self) -> Result<usize, RuntimeError> {
        (**self).sweep_orphans().await
    }

    async fn run_to_completion(&self, request: JobRunRequest) -> Result<JobOutcome, RuntimeError> {
        (**self).run_to_completion(request).await
    }
}
