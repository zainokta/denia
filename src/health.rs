use async_trait::async_trait;
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;

use crate::domain::HealthCheck;

#[derive(Debug, Error)]
pub enum HealthError {
    #[error("health check failed")]
    Failed,
}

#[async_trait]
pub trait HealthChecker: Send + Sync {
    /// Probe a replica for readiness. `target` is the workload's Denia-owned Unix
    /// socket path (the path Pingora dials), not a URL — the loopback bridge was
    /// removed in ADR-020.
    async fn check(&self, target: &str, health: &HealthCheck) -> Result<(), HealthError>;
}

#[async_trait]
impl<T> HealthChecker for Arc<T>
where
    T: HealthChecker + ?Sized,
{
    async fn check(&self, target: &str, health: &HealthCheck) -> Result<(), HealthError> {
        (**self).check(target, health).await
    }
}

/// Polling interval for the cold-start readiness probe.
const SOCKET_PROBE_POLL: Duration = Duration::from_millis(50);

/// Production readiness gate: a replica is healthy once its Unix socket accepts a
/// connection. Polls `UnixStream::connect(target)` until it succeeds or
/// `health.timeout_seconds` elapses. Connect-accept proves the workload has called
/// `listen()`; it does not exercise the app's HTTP loop (accepted tradeoff — it
/// closes the cold-start 502 connect race without an HTTP client).
#[derive(Debug, Clone, Default)]
pub struct SocketHealthChecker;

impl SocketHealthChecker {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl HealthChecker for SocketHealthChecker {
    async fn check(&self, target: &str, health: &HealthCheck) -> Result<(), HealthError> {
        let deadline = Instant::now() + Duration::from_secs(health.timeout_seconds);
        loop {
            match tokio::net::UnixStream::connect(target).await {
                Ok(_) => return Ok(()),
                Err(_) => {
                    if Instant::now() >= deadline {
                        return Err(HealthError::Failed);
                    }
                    tokio::time::sleep(SOCKET_PROBE_POLL).await;
                }
            }
        }
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
    async fn check(&self, _target: &str, _health: &HealthCheck) -> Result<(), HealthError> {
        if self.healthy {
            Ok(())
        } else {
            Err(HealthError::Failed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UnixListener;

    #[tokio::test]
    async fn connect_probe_succeeds_when_socket_accepts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("svc.sock");
        let _listener = UnixListener::bind(&path).unwrap();
        let checker = SocketHealthChecker::new();
        let hc = HealthCheck::new("/healthz", 1);
        checker
            .check(&path.to_string_lossy(), &hc)
            .await
            .expect("listening socket should pass the readiness probe");
    }

    #[tokio::test]
    async fn connect_probe_fails_when_socket_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.sock");
        let checker = SocketHealthChecker::new();
        let hc = HealthCheck::new("/healthz", 1);
        assert!(
            checker.check(&path.to_string_lossy(), &hc).await.is_err(),
            "absent socket should time out and fail"
        );
    }
}
