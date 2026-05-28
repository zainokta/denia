use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
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
        .route("/deployments/{deployment_id}", get(get_deployment))
        .route(
            "/services/{service_id}/deployments",
            get(list_service_deployments),
        )
}

/// `POST /v1/deployments` — async deploy entry point (ADR-024).
///
/// Persists a `Pending` deployment row, opens the per-deployment log file, and
/// spawns the deploy pipeline on a detached `tokio` task. Returns
/// `202 Accepted` with the deployment body so the operator console can pivot
/// to the log-tail view immediately.
async fn create_deployment(
    State(state): State<AppState>,
    principal: Principal,
    Json(request): Json<DeploymentRequest>,
) -> Result<(StatusCode, Json<crate::domain::Deployment>), ApiError> {
    let Some(service) = state.services.get_service(request.service_id())? else {
        return Err(ApiError::NotFound("service not found".to_string()));
    };
    ensure_role(&state, &principal, service.project_id, Role::Operator)?;

    let coordinator = DeploymentCoordinator::new_with_shared_routing(
        state.deployment_repos(),
        state.runtime.clone(),
        state.health.clone(),
        state.ingress.clone(),
        state.routes.clone(),
    );
    let deployment = coordinator
        .create_pending(&service, request.clone())
        .await?;

    // Open the per-deployment log file (`<log_dir>/deployments/<id>.log`, 0600
    // append). The async task writes phase lines here; the SSE handler tails
    // it. On io error we surface 500 — the row is already persisted but the
    // operator should see the failure to provision the log surface.
    let log = crate::deploy::log::DeploymentLogWriter::create(&state.config.log_dir, deployment.id)
        .map_err(ApiError::Log)?;

    // Capture owned copies of everything the spawned task needs; the task
    // cannot borrow from the request handler's stack frame.
    let svc = service.clone();
    let req = request.clone();
    let deployment_id = deployment.id;
    // Prefer the AppState-shared OCI cache so acquirer, GC, and observability
    // share one reservation map; fall back to cache-less puller path if it
    // failed to initialize at boot.
    let acquirer = match state.oci_cache.clone() {
        Some(cache) => ArtifactAcquirer::new_with_cache(state.config.clone(), cache),
        None => ArtifactAcquirer::new(state.config.clone()),
    };
    let secret_store = crate::secrets::SopsSecretStore::new(state.config.data_dir.clone());
    let sops_binary: std::path::PathBuf = state.config.sops_binary.clone();
    let runner = state.command_runner.clone();
    let coordinator_for_task = DeploymentCoordinator::new_with_shared_routing(
        state.deployment_repos(),
        state.runtime.clone(),
        state.health.clone(),
        state.ingress.clone(),
        state.routes.clone(),
    );

    tokio::spawn(async move {
        let deps = crate::deploy::coordinator::RunDeps {
            acquirer: &acquirer,
            runner: runner.as_ref(),
            secret_store: &secret_store,
            sops_binary: sops_binary.as_path(),
        };
        let _ = coordinator_for_task
            .run_with_deps(deployment_id, svc, req, &log, deps)
            .await;
    });

    Ok((StatusCode::ACCEPTED, Json(deployment)))
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

/// `GET /v1/deployments/{deployment_id}` — fetch a single deployment row.
///
/// The deployment-detail page polls this endpoint while the deploy task is
/// running to observe status transitions (Pending → Building → Starting →
/// Healthy|Failed). Returns 404 if the deployment or its parent service no
/// longer exists; otherwise returns the JSON-encoded `Deployment`.
async fn get_deployment(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(deployment_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<crate::domain::Deployment>, ApiError> {
    let Some(deployment) = state.deployments.get_deployment(deployment_id)? else {
        return Err(ApiError::NotFound("deployment not found".to_string()));
    };
    let Some(service) = state.services.get_service(deployment.service_id)? else {
        return Err(ApiError::NotFound("service not found".to_string()));
    };
    ensure_role(&state, &principal, service.project_id, Role::Viewer)?;
    Ok(Json(deployment))
}
