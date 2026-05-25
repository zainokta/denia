use async_trait::async_trait;
use std::sync::Arc;
use thiserror::Error;

use crate::domain::HealthCheck;

#[derive(Debug, Error)]
pub enum HealthError {
    #[error("health check failed")]
    Failed,
}

#[async_trait]
pub trait HealthChecker: Send + Sync {
    async fn check(&self, url: &str, health: &HealthCheck) -> Result<(), HealthError>;
}

#[async_trait]
impl<T> HealthChecker for Arc<T>
where
    T: HealthChecker + ?Sized,
{
    async fn check(&self, url: &str, health: &HealthCheck) -> Result<(), HealthError> {
        (**self).check(url, health).await
    }
}

#[derive(Debug, Clone)]
pub struct FakeHealthChecker {
    healthy: bool,
}

impl FakeHealthChecker {
    pub fn healthy() -> Self {
        Self { healthy: true }
    }
}

#[async_trait]
impl HealthChecker for FakeHealthChecker {
    async fn check(&self, _url: &str, _health: &HealthCheck) -> Result<(), HealthError> {
        if self.healthy {
            Ok(())
        } else {
            Err(HealthError::Failed)
        }
    }
}
