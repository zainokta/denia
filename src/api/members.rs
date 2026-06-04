use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::{delete, get},
};
use serde::Deserialize;

use crate::api::ApiError;
use crate::app::AppState;
use crate::auth::{Principal, ensure_role};
use crate::domain::{ProjectMembership, Role};

pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/projects/{project_id}/members",
            get(list_project_members).post(add_project_member),
        )
        .route(
            "/projects/{project_id}/members/{user_id}",
            delete(remove_project_member),
        )
}

#[derive(Debug, Deserialize)]
struct AddMemberRequest {
    user_id: uuid::Uuid,
    role: Role,
}

async fn list_project_members(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(project_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<Vec<ProjectMembership>>, ApiError> {
    ensure_role(&state, &principal, project_id, Role::Viewer)?;
    Ok(Json(state.users.list_members(project_id)?))
}

async fn add_project_member(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(project_id): axum::extract::Path<uuid::Uuid>,
    Json(input): Json<AddMemberRequest>,
) -> Result<(StatusCode, Json<ProjectMembership>), ApiError> {
    ensure_role(&state, &principal, project_id, Role::Admin)?;
    // Reject memberships that bind to a non-existent user id. Without this a
    // project Admin can create dangling memberships for ids that never resolve.
    if state.users.get_user(input.user_id)?.is_none() {
        return Err(ApiError::NotFound("user not found".to_string()));
    }
    state
        .users
        .set_membership(input.user_id, project_id, input.role)?;
    Ok((
        StatusCode::CREATED,
        Json(ProjectMembership {
            user_id: input.user_id,
            project_id,
            role: input.role,
        }),
    ))
}

async fn remove_project_member(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path((project_id, user_id)): axum::extract::Path<(uuid::Uuid, uuid::Uuid)>,
) -> Result<Json<serde_json::Value>, ApiError> {
    ensure_role(&state, &principal, project_id, Role::Admin)?;
    // Lockout guard: refuse to remove the last Admin of a project so the
    // project can never end up with no one able to manage its membership.
    let members = state.users.list_members(project_id)?;
    let target_is_admin = members
        .iter()
        .any(|m| m.user_id == user_id && m.role >= Role::Admin);
    if target_is_admin {
        let admin_count = members.iter().filter(|m| m.role >= Role::Admin).count();
        if admin_count <= 1 {
            return Err(ApiError::Conflict(
                "cannot remove the last admin of a project".to_string(),
            ));
        }
    }
    state.users.remove_membership(user_id, project_id)?;
    Ok(Json(serde_json::json!({"removed": true})))
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

    async fn add_member(
        app: &axum::Router,
        project_id: uuid::Uuid,
        user_id: uuid::Uuid,
        role: &str,
    ) -> StatusCode {
        let body = serde_json::json!({"user_id": user_id, "role": role});
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/projects/{project_id}/members"))
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .header("Content-Type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    async fn remove_member(
        app: &axum::Router,
        project_id: uuid::Uuid,
        user_id: uuid::Uuid,
    ) -> StatusCode {
        app.clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/v1/projects/{project_id}/members/{user_id}"))
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn add_member_for_nonexistent_user_is_404() {
        let state = test_state();
        let project_id = state.projects.default_project_id().unwrap();
        let app = build_router(state);
        assert_eq!(
            add_member(&app, project_id, uuid::Uuid::now_v7(), "operator").await,
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn add_member_for_existing_user_is_201() {
        let state = test_state();
        let project_id = state.projects.default_project_id().unwrap();
        let user = state.users.create_user("u1", "hash", false).unwrap();
        let app = build_router(state);
        assert_eq!(
            add_member(&app, project_id, user.id, "operator").await,
            StatusCode::CREATED
        );
    }

    #[tokio::test]
    async fn removing_last_admin_is_409() {
        let state = test_state();
        let project_id = state.projects.default_project_id().unwrap();
        let admin = state
            .users
            .create_user("only-admin", "hash", false)
            .unwrap();
        state
            .users
            .set_membership(admin.id, project_id, Role::Admin)
            .unwrap();
        let app = build_router(state);
        assert_eq!(
            remove_member(&app, project_id, admin.id).await,
            StatusCode::CONFLICT
        );
    }

    #[tokio::test]
    async fn removing_admin_when_another_exists_is_ok() {
        let state = test_state();
        let project_id = state.projects.default_project_id().unwrap();
        let a1 = state.users.create_user("admin1", "hash", false).unwrap();
        let a2 = state.users.create_user("admin2", "hash", false).unwrap();
        state
            .users
            .set_membership(a1.id, project_id, Role::Admin)
            .unwrap();
        state
            .users
            .set_membership(a2.id, project_id, Role::Admin)
            .unwrap();
        let app = build_router(state);
        assert_eq!(remove_member(&app, project_id, a1.id).await, StatusCode::OK);
    }
}
