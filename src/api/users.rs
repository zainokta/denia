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
        .route("/users/directory", get(list_user_directory))
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

/// `GET /v1/users/directory` — username/id list backing the member-picker UI.
///
/// Per ADR-008 user enumeration is privileged, so this is restricted to
/// principals that can actually act on the directory: a super-admin, or any
/// user who is `Admin` on at least one project (the only role that can add
/// project members via `POST /projects/{id}/members`). A plain Viewer/Operator
/// no longer enumerates every username + user-id in the system.
async fn list_user_directory(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<Vec<crate::domain::UserSummary>>, ApiError> {
    if !principal.is_super_admin {
        let user_id = principal
            .user_id
            .ok_or_else(|| ApiError::Forbidden("authenticated user required".to_string()))?;
        let is_project_admin = state
            .users
            .list_memberships_for_user(user_id)?
            .iter()
            .any(|m| m.role >= crate::domain::Role::Admin);
        if !is_project_admin {
            return Err(ApiError::Forbidden(
                "project admin or super admin required".to_string(),
            ));
        }
    }
    Ok(Json(state.users.list_user_directory()?))
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
    if input.username.trim().is_empty() {
        return Err(ApiError::BadRequest("username required".to_string()));
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

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use crate::app::{AppState, build_router};
    use crate::config::AppConfig;
    use crate::domain::Role;

    const ADMIN_TOKEN: &str = "test-admin-token-0123456789abcdef";

    fn test_state() -> AppState {
        AppState::builder(AppConfig::for_test(ADMIN_TOKEN)).build()
    }

    async fn create_user_req(app: &axum::Router, username: &str, password: &str) -> StatusCode {
        let body = serde_json::json!({"username": username, "password": password});
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/users")
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .header("Content-Type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn empty_username_is_400_not_500() {
        let app = build_router(test_state());
        assert_eq!(
            create_user_req(&app, "   ", "password-长enough-123").await,
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn short_password_is_400() {
        let app = build_router(test_state());
        assert_eq!(
            create_user_req(&app, "alice", "short").await,
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn duplicate_username_is_409_not_500() {
        let app = build_router(test_state());
        assert_eq!(
            create_user_req(&app, "alice", "password-long-enough-1").await,
            StatusCode::CREATED
        );
        assert_eq!(
            create_user_req(&app, "alice", "password-long-enough-2").await,
            StatusCode::CONFLICT
        );
    }

    #[tokio::test]
    async fn directory_forbidden_for_plain_viewer() {
        let state = test_state();
        let project_id = state.projects.default_project_id().unwrap();
        let viewer = state.users.create_user("viewer", "hash", false).unwrap();
        state
            .users
            .set_membership(viewer.id, project_id, Role::Viewer)
            .unwrap();
        let token = state
            .tokens
            .create_api_token(viewer.id, "viewer")
            .unwrap()
            .token;

        let resp = build_router(state)
            .oneshot(
                Request::builder()
                    .uri("/v1/users/directory")
                    .header("Authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn directory_allowed_for_project_admin() {
        let state = test_state();
        let project_id = state.projects.default_project_id().unwrap();
        let admin = state.users.create_user("padmin", "hash", false).unwrap();
        state
            .users
            .set_membership(admin.id, project_id, Role::Admin)
            .unwrap();
        let token = state
            .tokens
            .create_api_token(admin.id, "padmin")
            .unwrap()
            .token;

        let resp = build_router(state)
            .oneshot(
                Request::builder()
                    .uri("/v1/users/directory")
                    .header("Authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn directory_allowed_for_super_admin() {
        let resp = build_router(test_state())
            .oneshot(
                Request::builder()
                    .uri("/v1/users/directory")
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
