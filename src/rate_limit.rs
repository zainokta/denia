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

#[derive(Clone)]
struct BucketLimiter {
    inner: Arc<Mutex<HashMap<String, Vec<Instant>>>>,
    max_attempts: usize,
    window: Duration,
}

impl BucketLimiter {
    fn new(max_attempts: usize, window_secs: u64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            max_attempts,
            window: Duration::from_secs(window_secs),
        }
    }

    async fn check(&self, key: &str) -> bool {
        let mut map = self.inner.lock().await;
        let now = Instant::now();
        let entry = map.entry(key.to_string()).or_default();
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

    // Only trust X-Forwarded-For when the TCP peer is loopback (our own Traefik
    // reverse proxy). A directly-connected client could otherwise spoof the
    // header and evade or poison the rate-limit buckets.
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
