use axum::{Json, Router, extract::State, routing::get};

use serde::Deserialize;

use crate::api::ApiError;
use crate::app::AppState;
use crate::auth::{Principal, ensure_role, ensure_super_admin};
use crate::domain::service::ResourceLimits;
use crate::domain::{Project, Role};

#[derive(Debug, Deserialize)]
struct CreateProjectRequest {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    shared_env: Vec<(String, String)>,
    #[serde(default)]
    default_resource_limits: Option<ResourceLimits>,
}

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
    Json(req): Json<CreateProjectRequest>,
) -> Result<Json<Project>, ApiError> {
    ensure_super_admin(&principal)?;
    let mut project =
        Project::new(req.name, req.description).map_err(|e| ApiError::BadRequest(e.to_string()))?;
    project.shared_env = req.shared_env;
    project.default_resource_limits = req.default_resource_limits;
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

#[cfg(test)]
mod tests {
    use crate::app::{AppState, build_router};
    use crate::config::AppConfig;
    use crate::domain::Project;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    const ADMIN_TOKEN: &str = "test-admin-token-0123456789abcdef";

    fn test_state() -> AppState {
        AppState::builder(AppConfig::for_test(ADMIN_TOKEN)).build()
    }

    #[tokio::test]
    async fn create_then_get_project_roundtrips() {
        let state = test_state();
        let body = serde_json::json!({ "name": "team-a" }).to_string();
        let app = build_router(state);

        let create = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/projects")
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .header("Content-Type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create.status(), StatusCode::OK);

        let created_bytes = axum::body::to_bytes(create.into_body(), usize::MAX)
            .await
            .unwrap();
        let created: Project = serde_json::from_slice(&created_bytes).unwrap();

        let get = app
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/projects/{}", created.id))
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn get_unknown_project_returns_404() {
        let resp = build_router(test_state())
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/projects/{}", uuid::Uuid::now_v7()))
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
