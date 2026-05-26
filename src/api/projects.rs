use axum::{Json, Router, extract::State, routing::get};

use crate::app::{ApiError, AppState};
use crate::auth::{Principal, ensure_role, ensure_super_admin};
use crate::domain::{Project, Role};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects", get(list_projects).post(create_project))
        .route(
            "/projects/{project_id}",
            get(get_project).delete(delete_project),
        )
}

async fn list_projects(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<Vec<Project>>, ApiError> {
    let all = state.projects.list_projects()?;
    if principal.is_super_admin {
        return Ok(Json(all));
    }
    let user_id = principal
        .user_id
        .ok_or_else(|| ApiError::Forbidden("authenticated user required".to_string()))?;
    let memberships = state.users.list_memberships_for_user(user_id)?;
    let allowed: std::collections::HashSet<uuid::Uuid> =
        memberships.into_iter().map(|m| m.project_id).collect();
    Ok(Json(
        all.into_iter()
            .filter(|p| allowed.contains(&p.id))
            .collect(),
    ))
}

async fn get_project(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(project_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<Project>, ApiError> {
    ensure_role(&state, &principal, project_id, Role::Viewer)?;
    let project = state
        .projects
        .get_project(project_id)?
        .ok_or_else(|| ApiError::NotFound("project not found".to_string()))?;
    Ok(Json(project))
}

async fn create_project(
    State(state): State<AppState>,
    principal: Principal,
    Json(project): Json<Project>,
) -> Result<Json<Project>, ApiError> {
    ensure_super_admin(&principal)?;
    Ok(Json(state.projects.put_project(project)?))
}

async fn delete_project(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(project_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    ensure_role(&state, &principal, project_id, Role::Admin)?;
    state.projects.delete_project(project_id)?;
    Ok(Json(serde_json::json!({"deleted": true})))
}
