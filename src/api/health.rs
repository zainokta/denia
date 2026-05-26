use axum::Json;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    ok: bool,
}

pub async fn healthz() -> Json<HealthResponse> {
    Json(HealthResponse { ok: true })
}
