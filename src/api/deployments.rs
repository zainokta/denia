use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
    routing::{get, post},
};
use std::convert::Infallible;
use std::time::Duration;
use tokio_stream::wrappers::ReceiverStream;

use crate::api::ApiError;
use crate::app::AppState;
use crate::artifacts::acquirer::ArtifactAcquirer;
use crate::auth::{Principal, ensure_role};
use crate::deploy::DeploymentCoordinator;
use crate::domain::{DeploymentRequest, Role};
use crate::logs::LogTailer;

/// Process-wide cap on concurrent SSE deployment-log streams. Each stream
/// holds a long-lived task polling a file; an unbounded number of clients
/// could exhaust tasks/file descriptors (mirrors F-8 mitigation in services).
static DEPLOYMENT_LOG_STREAM_LIMIT: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::Semaphore::new(64)));

/// Read-model for deployment responses: the persisted `Deployment` plus its
/// resolved artifact (joined from the `artifacts` table). The domain
/// `Deployment` deliberately has no artifact field — the link lives in a
/// separate table — so the API attaches it here. `artifact` is `None` until the
/// deploy pipeline acquires + links the artifact (`set_deployment_artifact`).
#[derive(serde::Serialize)]
struct DeploymentView {
    #[serde(flatten)]
    deployment: crate::domain::Deployment,
    artifact: Option<crate::artifacts::ArtifactRecord>,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/deployments", post(create_deployment))
        .route("/deployments/{deployment_id}", get(get_deployment))
        .route(
            "/deployments/{deployment_id}/logs",
            get(deployment_log_stream),
        )
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
    let age_key_file: std::path::PathBuf = state.config.age_key_file.clone();
    let runner = state.command_runner.clone();
    let coordinator_for_task = DeploymentCoordinator::new_with_shared_routing(
        state.deployment_repos(),
        state.runtime.clone(),
        state.health.clone(),
        state.ingress.clone(),
        state.routes.clone(),
    );
    let autoscaler = state.autoscaler.clone();

    tokio::spawn(async move {
        let deps = crate::deploy::coordinator::RunDeps {
            acquirer: &acquirer,
            runner: runner.as_ref(),
            secret_store: &secret_store,
            sops_binary: sops_binary.as_path(),
            age_key_file: age_key_file.as_path(),
        };
        let is_autoscaled = svc.autoscale.is_some();
        let service_id = svc.id;
        let run = coordinator_for_task
            .run_with_deps(deployment_id, svc, req, &log, deps)
            .await;
        // Autoscaled service: hand replica ownership to the controller so it
        // launches `min` replicas (each health-gated) or none for min==0 (woken
        // by the activator). Without this, the deploy promotes a routable
        // service the autoscaler never tracks — `/v1/workloads` would report 0
        // and scale-to-zero / cold-start would never engage (ADR-028).
        if run.is_ok()
            && is_autoscaled
            && let Some(autoscaler) = autoscaler
        {
            let events = autoscaler.lock().await.reconcile_service(service_id).await;
            for ev in &events {
                let _ = log.write("AUTOSCALE", &format!("{ev:?}"));
            }
        }
    });

    Ok((StatusCode::ACCEPTED, Json(deployment)))
}

async fn list_service_deployments(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(service_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<Vec<DeploymentView>>, ApiError> {
    let Some(service) = state.services.get_service(service_id)? else {
        return Err(ApiError::NotFound("service not found".to_string()));
    };
    ensure_role(&state, &principal, service.project_id, Role::Viewer)?;
    let deployments = state.deployments.list_deployments(service_id)?;
    let views = deployments
        .into_iter()
        .map(|deployment| {
            let artifact = state.deployments.get_deployment_artifact(deployment.id)?;
            Ok(DeploymentView {
                deployment,
                artifact,
            })
        })
        .collect::<Result<Vec<_>, ApiError>>()?;
    Ok(Json(views))
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
) -> Result<Json<DeploymentView>, ApiError> {
    let Some(deployment) = state.deployments.get_deployment(deployment_id)? else {
        return Err(ApiError::NotFound("deployment not found".to_string()));
    };
    let Some(service) = state.services.get_service(deployment.service_id)? else {
        return Err(ApiError::NotFound("service not found".to_string()));
    };
    ensure_role(&state, &principal, service.project_id, Role::Viewer)?;
    let artifact = state.deployments.get_deployment_artifact(deployment.id)?;
    Ok(Json(DeploymentView {
        deployment,
        artifact,
    }))
}

/// `GET /v1/deployments/{deployment_id}/logs` — SSE tail of the per-deployment
/// log file written by the deploy task (ADR-024).
///
/// Mirrors the service-log SSE pattern: bounded by a process-wide semaphore,
/// emits the whole backlog up-front (deploy logs are short and operators want
/// the full story), then polls every 300ms. The stream terminates with a
/// `done` event once the deployment status is terminal (`Healthy`, `Failed`,
/// or `Stopped`) AND the latest poll returned zero new lines — guaranteeing
/// we don't truncate trailing output that landed in the same tick as the
/// status flip.
async fn deployment_log_stream(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(deployment_id): axum::extract::Path<uuid::Uuid>,
) -> Result<impl axum::response::IntoResponse, ApiError> {
    let Some(deployment) = state.deployments.get_deployment(deployment_id)? else {
        return Err(ApiError::NotFound("deployment not found".to_string()));
    };
    let Some(service) = state.services.get_service(deployment.service_id)? else {
        return Err(ApiError::NotFound("service not found".to_string()));
    };
    ensure_role(&state, &principal, service.project_id, Role::Operator)?;

    let permit = DEPLOYMENT_LOG_STREAM_LIMIT
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            ApiError::TooManyRequests("too many concurrent deployment log streams".to_string())
        })?;

    let log_path = crate::deploy::log::deployment_log_path(&state.config.log_dir, deployment_id);
    let store = state.deployments.clone();

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(256);

    tokio::spawn(async move {
        let _permit = permit;
        let mut tailer = LogTailer::new(&log_path);

        // Emit the entire backlog up-front. Deploy logs are short and the
        // operator wants the whole story; use a large bound rather than the
        // 200-line cap used for service logs.
        if let Ok(lines) = tokio::task::block_in_place(|| tailer.backlog(10_000)) {
            for line in lines {
                if tx.send(Ok(Event::default().data(line))).await.is_err() {
                    return;
                }
            }
        }

        let mut interval = tokio::time::interval(Duration::from_millis(300));
        loop {
            interval.tick().await;
            let lines = tokio::task::block_in_place(|| tailer.poll()).unwrap_or_default();
            let polled = lines.len();
            for line in lines {
                if tx.send(Ok(Event::default().data(line))).await.is_err() {
                    return;
                }
            }
            // Close the stream once the deployment has reached a terminal
            // state AND the tailer caught up to EOF (no new lines this tick).
            // Polling for a status row that disappeared (deleted service /
            // store error) is treated as non-terminal so we don't truncate a
            // healthy in-progress stream on a transient repo blip.
            if polled == 0
                && let Ok(Some(d)) = store.get_deployment(deployment_id)
            {
                let terminal = matches!(
                    d.status,
                    crate::domain::DeploymentStatus::Healthy
                        | crate::domain::DeploymentStatus::Failed
                        | crate::domain::DeploymentStatus::Stopped
                );
                if terminal {
                    let _ = tx.send(Ok(Event::default().event("done").data(""))).await;
                    return;
                }
            }
        }
    });

    Ok(Sse::new(ReceiverStream::new(rx))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
}
