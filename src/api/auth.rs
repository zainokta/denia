use axum::{
    Json, Router,
    extract::{Request, State},
    http::{StatusCode, header},
    routing::{get, post},
};
use serde::Deserialize;

use crate::api::ApiError;
use crate::app::AppState;
use crate::auth::Principal;
use crate::domain::{LoginResult, Me, PrincipalView};

pub fn public_router() -> Router<AppState> {
    Router::new().route("/auth/login", post(login_handler))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/logout", post(logout_handler))
        .route("/me", get(me_handler))
}

#[derive(Debug, Deserialize)]
struct LoginRequest {
    username: String,
    password: String,
}

async fn login_handler(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    Json(input): Json<LoginRequest>,
) -> Result<Json<LoginResult>, ApiError> {
    if headers.get(header::AUTHORIZATION).is_some() {
        return Err(ApiError::BadRequest("already authenticated".to_string()));
    }
    let user = state
        .users
        .verify_login(&input.username, &input.password)
        .map_err(|_| ApiError::Unauthorized("invalid credentials".to_string()))?;
    let session = state.users.create_session(user.id, 24)?;
    Ok(Json(LoginResult {
        token: session.token,
        expires_at: session.expires_at,
    }))
}

async fn logout_handler(
    State(state): State<AppState>,
    request: Request,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if let Some(t) = token {
        let th = crate::auth::hash_token(t);
        let _ = state.users.delete_session(&th);
    }
    Ok((
        StatusCode::OK,
        Json(serde_json::json!({"logged_out": true})),
    ))
}

async fn me_handler(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<Me>, ApiError> {
    let admin_initialized = state.users.is_admin_initialized()?;
    if principal.is_super_admin && !principal.is_authenticated() {
        return Ok(Json(Me {
            principal: PrincipalView::Bootstrap,
            is_super_admin: true,
            admin_initialized,
            memberships: vec![],
        }));
    }
    let user_id = principal
        .user_id
        .ok_or(ApiError::Conflict("no user".to_string()))?;
    let user = state
        .users
        .get_user(user_id)?
        .ok_or_else(|| ApiError::NotFound("user not found".to_string()))?;
    let memberships = state.users.list_memberships_for_user(user_id)?;
    Ok(Json(Me {
        principal: PrincipalView::User { user },
        is_super_admin: principal.is_super_admin,
        admin_initialized,
        memberships,
    }))
}
