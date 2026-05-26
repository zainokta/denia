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
    state.users.remove_membership(user_id, project_id)?;
    Ok(Json(serde_json::json!({"removed": true})))
}
