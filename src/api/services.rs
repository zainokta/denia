use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use serde::Serialize;

use crate::api::ApiError;
use crate::app::AppState;
use crate::auth::{Principal, ensure_role};
use crate::deploy::DeploymentCoordinator;
use crate::domain::{Role, ServiceConfig};
use crate::logs::LogStore;
use crate::metrics::CgroupMetricsReader;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/services", get(list_services).post(put_service))
        .route("/services/{service_id}/logs", get(service_logs))
        .route("/services/{service_id}/metrics", get(service_metrics))
        .route("/services/{service_id}/{action}", post(lifecycle_command))
}

#[derive(Debug, Serialize)]
struct LifecycleResponse {
    service_id: uuid::Uuid,
    action: String,
}

async fn list_services(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<Vec<ServiceConfig>>, ApiError> {
    let all = state.services.list_services()?;
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
            .filter(|s| allowed.contains(&s.project_id))
            .collect(),
    ))
}

async fn put_service(
    State(state): State<AppState>,
    principal: Principal,
    Json(service): Json<ServiceConfig>,
) -> Result<Json<ServiceConfig>, ApiError> {
    ensure_role(&state, &principal, service.project_id, Role::Operator)?;
    if let crate::domain::ServiceSource::ExternalImage(src) = &service.source {
        src.validate()
            .map_err(|e| ApiError::BadRequest(e.to_string()))?;
        if let Some(registry_id) = src.registry_id {
            let registry = state
                .registries
                .registry(registry_id)?
                .ok_or_else(|| ApiError::NotFound("registry not found".into()))?;
            if registry.project_id != service.project_id {
                return Err(ApiError::NotFound("registry not found".into()));
            }
        }
    }
    Ok(Json(state.services.put_service(service)?))
}

async fn lifecycle_command(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path((service_id, action)): axum::extract::Path<(uuid::Uuid, String)>,
) -> Result<(StatusCode, Json<LifecycleResponse>), ApiError> {
    let Some(service) = state.services.get_service(service_id)? else {
        return Err(ApiError::NotFound("service not found".to_string()));
    };
    ensure_role(&state, &principal, service.project_id, Role::Operator)?;
    match action.as_str() {
        "stop" => {
            let coordinator = DeploymentCoordinator::new_with_shared_routing(
                state.deployment_repos(),
                state.runtime.clone(),
                state.health.clone(),
                state.bridge_allocator.clone(),
                state.bridge_manager.clone(),
                state.config.traefik_dynamic_config_path.clone(),
                state.routes.clone(),
                state.ingress_options.clone(),
            );
            coordinator.stop_service(&service).await?;
            Ok((
                StatusCode::ACCEPTED,
                Json(LifecycleResponse { service_id, action }),
            ))
        }
        _ => Err(ApiError::BadRequest(format!(
            "unsupported lifecycle action: {action}"
        ))),
    }
}

async fn service_logs(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(service_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<Vec<String>>, ApiError> {
    let Some(service) = state.services.get_service(service_id)? else {
        return Err(ApiError::NotFound("service not found".to_string()));
    };
    ensure_role(&state, &principal, service.project_id, Role::Operator)?;
    let logs = LogStore::new(&state.config.log_dir);
    match logs.read_recent(&service.name, 200) {
        Ok(lines) => Ok(Json(lines)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Json(Vec::new())),
        Err(error) => Err(ApiError::Log(error)),
    }
}

async fn service_metrics(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(service_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<Vec<crate::metrics::MetricSnapshot>>, ApiError> {
    let Some(service) = state.services.get_service(service_id)? else {
        return Err(ApiError::NotFound("service not found".to_string()));
    };
    ensure_role(&state, &principal, service.project_id, Role::Viewer)?;
    let Some(deployment_id) = state.deployments.promoted_deployment(service_id)? else {
        return Ok(Json(Vec::new()));
    };
    let reader = CgroupMetricsReader::new(state.config.cgroup_root.clone());
    Ok(Json(vec![reader.read_by_id(
        &service.name,
        service.id,
        deployment_id,
    )?]))
}
