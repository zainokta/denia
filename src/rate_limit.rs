use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    Json,
    extract::{Request, State},
    middleware::Next,
    response::{IntoResponse, Response},
};
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct LoginRateLimiter {
    inner: Arc<Mutex<HashMap<String, Vec<Instant>>>>,
    max_attempts: usize,
    window: Duration,
}

impl LoginRateLimiter {
    pub fn new(max_attempts: usize, window_secs: u64) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            max_attempts,
            window: Duration::from_secs(window_secs),
        }
    }

    async fn check(&self, ip: &str) -> bool {
        let mut map = self.inner.lock().await;
        let now = Instant::now();
        let entry = map.entry(ip.to_string()).or_default();
        entry.retain(|t| now.duration_since(*t) < self.window);
        if entry.len() >= self.max_attempts {
            return false;
        }
        entry.push(now);
        true
    }
}

impl Default for LoginRateLimiter {
    fn default() -> Self {
        Self::new(5, 60)
    }
}

fn extract_client_ip(headers: &axum::http::HeaderMap) -> String {
    if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok())
        && let Some(ip) = xff.split(',').next()
    {
        return ip.trim().to_string();
    }
    "unknown".to_string()
}

pub async fn rate_limit_login(
    State(limiter): State<LoginRateLimiter>,
    request: Request,
    next: Next,
) -> Response {
    let ip = extract_client_ip(request.headers());

    if !limiter.check(&ip).await {
        let body = serde_json::json!({"error": "too many login attempts, try again later"});
        return (axum::http::StatusCode::TOO_MANY_REQUESTS, Json(body)).into_response();
    }

    next.run(request).await
}
