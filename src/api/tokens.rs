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
use crate::domain::ApiToken;

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/api-tokens",
            get(list_api_tokens_handler).post(create_api_token_handler),
        )
        .route("/api-tokens/{token_id}", delete(revoke_api_token_handler))
}

#[derive(Debug, Deserialize)]
struct CreateApiTokenRequest {
    name: String,
}

async fn list_api_tokens_handler(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<Vec<ApiToken>>, ApiError> {
    let user_id = principal
        .user_id
        .ok_or(ApiError::Forbidden("real user required".to_string()))?;
    Ok(Json(state.tokens.list_api_tokens(user_id)?))
}

async fn create_api_token_handler(
    State(state): State<AppState>,
    principal: Principal,
    Json(input): Json<CreateApiTokenRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let user_id = principal
        .user_id
        .ok_or(ApiError::Forbidden("real user required".to_string()))?;
    let api_token = state.tokens.create_api_token(user_id, &input.name)?;
    Ok((
        StatusCode::CREATED,
        Json(
            serde_json::json!({"id": api_token.id.to_string(), "name": api_token.name, "token": api_token.token}),
        ),
    ))
}

async fn revoke_api_token_handler(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(token_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let user_id = principal
        .user_id
        .ok_or(ApiError::Forbidden("real user required".to_string()))?;
    let tokens = state.tokens.list_api_tokens(user_id)?;
    let belongs = tokens.iter().any(|t| t.id == token_id);
    if !belongs {
        return Err(ApiError::NotFound("token not found".to_string()));
    }
    state.tokens.revoke_api_token(token_id)?;
    Ok(Json(serde_json::json!({"revoked": true})))
}
