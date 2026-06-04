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

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use crate::app::{AppState, build_router};
    use crate::config::AppConfig;

    const ADMIN_TOKEN: &str = "test-admin-token-0123456789abcdef";

    fn test_state() -> AppState {
        AppState::builder(AppConfig::for_test(ADMIN_TOKEN)).build()
    }

    async fn bootstrap_req(app: &axum::Router, username: &str, password: &str) -> StatusCode {
        let body = serde_json::json!({"username": username, "password": password});
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/bootstrap")
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
    async fn bootstrap_happy_path_is_201() {
        let app = build_router(test_state());
        assert_eq!(
            bootstrap_req(&app, "root", "password8").await,
            StatusCode::CREATED
        );
    }

    #[tokio::test]
    async fn bootstrap_empty_username_is_400() {
        let app = build_router(test_state());
        assert_eq!(
            bootstrap_req(&app, "  ", "password8").await,
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn bootstrap_second_call_is_409() {
        let app = build_router(test_state());
        assert_eq!(
            bootstrap_req(&app, "root", "password8").await,
            StatusCode::CREATED
        );
        assert_eq!(
            bootstrap_req(&app, "root2", "password8").await,
            StatusCode::CONFLICT
        );
    }

    /// A duplicate username surfacing from `bootstrap_admin`'s inner
    /// `create_user_q` is classified as a `Conflict` (→ 409), never an opaque
    /// 500. Exercised at the repo layer because the handler's
    /// `is_admin_initialized` gate fires first on a second HTTP bootstrap.
    #[test]
    fn bootstrap_admin_duplicate_username_is_conflict() {
        let store = crate::state::SqliteStore::open_in_memory().unwrap();
        store.migrate().unwrap();
        store.create_user("dup", "hash", false).unwrap();
        let repo = crate::repo::sqlite::SqliteUserRepo::new(store.pool());
        let err = repo.bootstrap_admin("dup", "hash").unwrap_err();
        assert!(
            matches!(err, crate::repo::RepoError::Conflict(_)),
            "expected Conflict, got {err:?}"
        );
    }
}
