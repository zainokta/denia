use axum::{Json, Router, extract::State, http::StatusCode, routing::post};
use serde::Deserialize;

use crate::api::ApiError;
use crate::app::AppState;
use crate::auth::Principal;

pub fn router() -> Router<AppState> {
    Router::new().route("/bootstrap", post(bootstrap_handler))
}

#[derive(Debug, Deserialize)]
struct BootstrapRequest {
    username: String,
    password: String,
}

async fn bootstrap_handler(
    State(state): State<AppState>,
    principal: Principal,
    Json(input): Json<BootstrapRequest>,
) -> Result<(StatusCode, Json<crate::domain::User>), ApiError> {
    if !principal.is_super_admin {
        return Err(ApiError::Forbidden("super admin required".to_string()));
    }
    if input.username.trim().is_empty() {
        return Err(ApiError::BadRequest("username required".to_string()));
    }
    if input.password.len() < 8 {
        return Err(ApiError::BadRequest(
            "password must be at least 8 characters".to_string(),
        ));
    }
    if state.users.is_admin_initialized()? {
        return Err(ApiError::Conflict("admin already initialized".to_string()));
    }
    let hash = crate::auth::hash_password(&input.password)?;
    let user = state.users.bootstrap_admin(&input.username, &hash)?;
    Ok((StatusCode::CREATED, Json(user)))
}
