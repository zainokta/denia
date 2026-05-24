use async_trait::async_trait;
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
