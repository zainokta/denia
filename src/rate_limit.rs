use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    Json,
    extract::{ConnectInfo, Request, State},
    middleware::Next,
    response::{IntoResponse, Response},
};
use tokio::sync::Mutex;

/// How many `check` calls between full sweeps that evict keys whose entire
/// timestamp window has lapsed. Bounds the cost of GC while keeping the map
/// from growing without limit under IP churn / spoofed-loopback XFF.
const SWEEP_INTERVAL: u64 = 1024;

struct BucketState {
    buckets: HashMap<String, Vec<Instant>>,
    ops_since_sweep: u64,
}

#[derive(Clone)]
struct BucketLimiter {
    inner: Arc<Mutex<BucketState>>,
    max_attempts: usize,
    window: Duration,
}

impl BucketLimiter {
    fn new(max_attempts: usize, window_secs: u64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(BucketState {
                buckets: HashMap::new(),
                ops_since_sweep: 0,
            })),
            max_attempts,
            window: Duration::from_secs(window_secs),
        }
    }

    async fn check(&self, key: &str) -> bool {
        let mut state = self.inner.lock().await;
        let now = Instant::now();

        // Periodically evict stale keys so IPs that never return don't leak
        // memory forever. Per-key trimming below handles live keys; this drops
        // keys whose whole window has expired.
        state.ops_since_sweep += 1;
        if state.ops_since_sweep >= SWEEP_INTERVAL {
            state.ops_since_sweep = 0;
            let window = self.window;
            state
                .buckets
                .retain(|_, ts| ts.iter().any(|t| now.duration_since(*t) < window));
        }

        let entry = state.buckets.entry(key.to_string()).or_default();
        entry.retain(|t| now.duration_since(*t) < self.window);
        if entry.len() >= self.max_attempts {
            return false;
        }
        entry.push(now);
        true
    }
}

#[derive(Clone)]
pub struct LoginRateLimiter {
    inner: BucketLimiter,
}

impl LoginRateLimiter {
    pub fn new(max_attempts: usize, window_secs: u64) -> Self {
        Self {
            inner: BucketLimiter::new(max_attempts, window_secs),
        }
    }
}

impl Default for LoginRateLimiter {
    fn default() -> Self {
        Self::new(5, 60)
    }
}

#[derive(Clone)]
pub struct AdminRateLimiter {
    inner: BucketLimiter,
}

impl AdminRateLimiter {
    pub fn new(max_attempts: usize, window_secs: u64) -> Self {
        Self {
            inner: BucketLimiter::new(max_attempts, window_secs),
        }
    }
}

impl Default for AdminRateLimiter {
    fn default() -> Self {
        Self::new(300, 60)
    }
}

fn extract_client_ip(request: &Request) -> String {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.ip());

    // Only trust X-Forwarded-For when the TCP peer is loopback (our own
    // in-process Pingora ingress, which overwrites the header with the real
    // client IP). A directly-connected client could otherwise spoof the header
    // and evade or poison the rate-limit buckets.
    if peer.map(|ip| ip.is_loopback()).unwrap_or(false)
        && let Some(forwarded) = request
            .headers()
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
        && let Some(client) = forwarded.split(',').next().map(str::trim)
        && !client.is_empty()
    {
        return client.to_string();
    }

    peer.map(|ip| ip.to_string())
        .unwrap_or_else(|| "anonymous".to_string())
}

pub async fn rate_limit_login(
    State(limiter): State<LoginRateLimiter>,
    request: Request,
    next: Next,
) -> Response {
    let ip = extract_client_ip(&request);

    if !limiter.inner.check(&ip).await {
        let body = serde_json::json!({"error": "too many login attempts, try again later"});
        return (axum::http::StatusCode::TOO_MANY_REQUESTS, Json(body)).into_response();
    }

    next.run(request).await
}

pub async fn rate_limit_admin(
    State(limiter): State<AdminRateLimiter>,
    request: Request,
    next: Next,
) -> Response {
    let ip = extract_client_ip(&request);

    if !limiter.inner.check(&ip).await {
        let body = serde_json::json!({"error": "too many requests"});
        return (axum::http::StatusCode::TOO_MANY_REQUESTS, Json(body)).into_response();
    }

    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::ConnectInfo;
    use std::net::SocketAddr;

    fn req_with(peer: &str, xff: Option<&str>) -> Request {
        let mut b = Request::builder().uri("/");
        if let Some(v) = xff {
            b = b.header("x-forwarded-for", v);
        }
        let mut req = b.body(axum::body::Body::empty()).unwrap();
        req.extensions_mut()
            .insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
        req
    }

    #[test]
    fn loopback_peer_trusts_forwarded_for() {
        let req = req_with("127.0.0.1:5000", Some("203.0.113.9, 10.0.0.1"));
        assert_eq!(extract_client_ip(&req), "203.0.113.9");
    }

    #[test]
    fn non_loopback_peer_ignores_forwarded_for() {
        let req = req_with("198.51.100.4:5000", Some("203.0.113.9"));
        assert_eq!(extract_client_ip(&req), "198.51.100.4");
    }

    #[tokio::test]
    async fn sweep_evicts_stale_keys() {
        // Zero-second window: every prior timestamp is immediately stale, so a
        // sweep must drop the key once it stops being touched.
        let limiter = BucketLimiter::new(1000, 0);
        assert!(limiter.check("stale-ip").await);
        // The first `check` inserts the key. Drive enough additional `check`s on
        // a *different* key to trigger the periodic sweep, which should evict
        // the now-stale "stale-ip" entry whose only timestamp has expired.
        for _ in 0..SWEEP_INTERVAL {
            limiter.check("live-ip").await;
        }
        let state = limiter.inner.lock().await;
        assert!(
            !state.buckets.contains_key("stale-ip"),
            "stale key should have been swept out"
        );
    }
}
