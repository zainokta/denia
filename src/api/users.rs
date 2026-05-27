use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::{delete, get},
};
use serde::Deserialize;

use crate::api::ApiError;
use crate::app::AppState;
use crate::auth::Principal;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/users", get(list_users).post(create_user_handler))
        .route("/users/{user_id}", delete(delete_user_handler))
}

async fn list_users(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<Vec<crate::domain::User>>, ApiError> {
    if !principal.is_super_admin {
        return Err(ApiError::Forbidden("super admin required".to_string()));
    }
    Ok(Json(state.users.list_users()?))
}

#[derive(Debug, Deserialize)]
struct CreateUserRequest {
    username: String,
    password: String,
    #[serde(default)]
    is_super_admin: bool,
}

async fn create_user_handler(
    State(state): State<AppState>,
    principal: Principal,
    Json(input): Json<CreateUserRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    if !principal.is_super_admin {
        return Err(ApiError::Forbidden("super admin required".to_string()));
    }
    if input.password.len() < 12 {
        return Err(ApiError::BadRequest(
            "password must be at least 12 characters".to_string(),
        ));
    }
    let hash = crate::auth::hash_password(&input.password)?;
    state
        .users
        .create_user(&input.username, &hash, input.is_super_admin)?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({"created": true})),
    ))
}

async fn delete_user_handler(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(user_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    if !principal.is_super_admin {
        return Err(ApiError::Forbidden("super admin required".to_string()));
    }
    state.users.delete_user(user_id)?;
    Ok(Json(serde_json::json!({"deleted": true})))
}
