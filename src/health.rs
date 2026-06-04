use async_trait::async_trait;
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use crate::domain::HealthCheck;
use crate::observability::access_log::parse_status_line;

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

/// Polling interval between readiness probe attempts during cold start.
const SOCKET_PROBE_POLL: Duration = Duration::from_millis(200);
/// Maximum time the readiness gate waits for a waking workload to start serving.
/// Kept under the ingress `ACTIVATION_WAIT` (30s) that bounds the held request.
const READINESS_PROBE_BUDGET: Duration = Duration::from_secs(25);
/// Cap on bytes read while looking for the HTTP status line.
const PROBE_READ_LIMIT: usize = 8192;

/// Production readiness gate. A replica is promoted to `Healthy` only once it
/// answers an HTTP request over its Denia-owned Unix socket.
///
/// The socket is fronted by the in-guest `socket-proxy`, which binds it
/// immediately at start — before the app is listening — and is a transparent byte
/// proxy that simply CLOSES the connection (no HTTP response) when it cannot reach
/// the upstream app. So a bare `connect()` is not enough: it succeeds against the
/// proxy while the app is still booting, which is exactly the cold-start 502 race.
/// Instead we send `GET {path}` and treat ANY parsed HTTP status line as ready —
/// receiving one means the proxy reached the app, i.e. the app is serving.
#[derive(Debug, Clone)]
pub struct SocketHealthChecker {
    budget: Duration,
}

impl Default for SocketHealthChecker {
    fn default() -> Self {
        Self::new()
    }
}

impl SocketHealthChecker {
    pub fn new() -> Self {
        Self {
            budget: READINESS_PROBE_BUDGET,
        }
    }

    #[cfg(test)]
    fn with_budget(budget: Duration) -> Self {
        Self { budget }
    }
}

/// One probe attempt: connect the socket, send a minimal HTTP/1.1 GET, and return
/// the response status code if a status line comes back before EOF. `None` means
/// the attempt yielded no status (connect failed, the socket-proxy closed the
/// connection because the app is not up yet, or the read timed out). The whole
/// attempt is bounded by `attempt_timeout`.
async fn probe_once(target: &str, path: &str, attempt_timeout: Duration) -> Option<u16> {
    let attempt = async {
        let mut stream = UnixStream::connect(target).await.ok()?;
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: localhost\r\nUser-Agent: denia-readiness\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.ok()?;

        let mut buf = Vec::with_capacity(256);
        let mut chunk = [0u8; 256];
        loop {
            let n = stream.read(&mut chunk).await.ok()?;
            if n == 0 {
                return None; // EOF before a status line: app not serving yet
            }
            buf.extend_from_slice(&chunk[..n]);
            if let Some(pos) = buf.iter().position(|&b| b == b'\n') {
                // `parse_status_line` splits on whitespace, so a trailing `\r` is
                // harmless — it only reads the first two tokens.
                let line = String::from_utf8_lossy(&buf[..pos]);
                return parse_status_line(&line);
            }
            if buf.len() >= PROBE_READ_LIMIT {
                return None;
            }
        }
    };
    tokio::time::timeout(attempt_timeout, attempt)
        .await
        .ok()
        .flatten()
}

#[async_trait]
impl HealthChecker for SocketHealthChecker {
    async fn check(&self, target: &str, health: &HealthCheck) -> Result<(), HealthError> {
        let attempt_timeout = Duration::from_secs(health.timeout_seconds);
        let deadline = Instant::now() + self.budget;
        loop {
            if probe_once(target, &health.path, attempt_timeout)
                .await
                .is_some()
            {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(HealthError::Failed);
            }
            tokio::time::sleep(SOCKET_PROBE_POLL).await;
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

    /// Always reports `HealthError::Failed`. Used to exercise the
    /// partial-deploy compensation path (a started replica must be stopped when
    /// the post-start healthcheck fails).
    pub fn failing() -> Self {
        Self { healthy: false }
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

    /// Accept exactly one connection, drain the request, and reply with `response`.
    async fn serve_once(listener: UnixListener, response: &'static str) {
        if let Ok((mut conn, _)) = listener.accept().await {
            let mut scratch = [0u8; 1024];
            let _ = conn.read(&mut scratch).await;
            let _ = conn.write_all(response.as_bytes()).await;
        }
    }

    #[tokio::test]
    async fn probe_ready_when_app_answers_2xx() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("svc.sock");
        let listener = UnixListener::bind(&path).unwrap();
        tokio::spawn(serve_once(
            listener,
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n",
        ));

        let checker = SocketHealthChecker::new();
        let hc = HealthCheck::new("/healthz", 2);
        checker
            .check(&path.to_string_lossy(), &hc)
            .await
            .expect("a 200 response means the app is serving");
    }

    #[tokio::test]
    async fn probe_ready_on_any_http_response() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("svc.sock");
        let listener = UnixListener::bind(&path).unwrap();
        tokio::spawn(serve_once(
            listener,
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n",
        ));

        let checker = SocketHealthChecker::new();
        let hc = HealthCheck::new("/healthz", 2);
        checker
            .check(&path.to_string_lossy(), &hc)
            .await
            .expect("any HTTP status proves the app is up (any-response rule)");
    }

    #[tokio::test]
    async fn probe_fails_when_proxy_closes_without_response() {
        // Mimic the socket-proxy with the app down: accept, then drop the
        // connection with no HTTP response. The probe must keep retrying and fail
        // within the (short, test-only) budget.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("svc.sock");
        let listener = UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            while let Ok((conn, _)) = listener.accept().await {
                drop(conn);
            }
        });

        let checker = SocketHealthChecker::with_budget(Duration::from_millis(300));
        let hc = HealthCheck::new("/healthz", 1);
        assert!(
            checker.check(&path.to_string_lossy(), &hc).await.is_err(),
            "no HTTP response should fail readiness within the budget"
        );
    }

    #[tokio::test]
    async fn probe_fails_when_socket_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.sock");
        let checker = SocketHealthChecker::with_budget(Duration::from_millis(300));
        let hc = HealthCheck::new("/healthz", 1);
        assert!(
            checker.check(&path.to_string_lossy(), &hc).await.is_err(),
            "absent socket should fail within the budget"
        );
    }
}
