use axum::{
    Json, Router,
    extract::State,
    routing::{get, post},
};

use crate::api::ApiError;
use crate::app::AppState;
use crate::artifacts::acquirer::ArtifactAcquirer;
use crate::auth::{Principal, ensure_role};
use crate::deploy::DeploymentCoordinator;
use crate::domain::{DeploymentRequest, Role};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/deployments", post(create_deployment))
        .route(
            "/services/{service_id}/deployments",
            get(list_service_deployments),
        )
}

async fn create_deployment(
    State(state): State<AppState>,
    principal: Principal,
    Json(request): Json<DeploymentRequest>,
) -> Result<Json<crate::domain::Deployment>, ApiError> {
    let Some(service) = state.services.get_service(request.service_id())? else {
        return Err(ApiError::NotFound("service not found".to_string()));
    };
    ensure_role(&state, &principal, service.project_id, Role::Operator)?;
    match request {
        DeploymentRequest::ExternalImage { .. } => {
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
            let acquirer = ArtifactAcquirer::new(state.config.clone());
            let secret_store = crate::secrets::SopsSecretStore::new(state.config.data_dir.clone());
            Ok(Json(
                coordinator
                    .deploy_external_image_source(
                        &service,
                        &acquirer,
                        state.command_runner.as_ref(),
                        &secret_store,
                        state.config.sops_binary.as_path(),
                    )
                    .await?,
            ))
        }
        DeploymentRequest::Git { .. } => {
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
            let acquirer = ArtifactAcquirer::new(state.config.clone());
            Ok(Json(
                coordinator
                    .deploy_git_source(&service, &acquirer, state.command_runner.as_ref())
                    .await?,
            ))
        }
    }
}

async fn list_service_deployments(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(service_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<Vec<crate::domain::Deployment>>, ApiError> {
    let Some(service) = state.services.get_service(service_id)? else {
        return Err(ApiError::NotFound("service not found".to_string()));
    };
    ensure_role(&state, &principal, service.project_id, Role::Viewer)?;
    Ok(Json(state.deployments.list_deployments(service_id)?))
}
