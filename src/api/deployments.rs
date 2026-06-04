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
use crate::auth::Principal;
use crate::deploy::DeploymentCoordinator;
use crate::domain::{DeploymentRequest, Role};
use crate::logs::LogTailer;

/// Process-wide cap on concurrent SSE deployment-log streams. Each stream
/// holds a long-lived task polling a file; an unbounded number of clients
/// could exhaust tasks/file descriptors (mirrors F-8 mitigation in services).
static DEPLOYMENT_LOG_STREAM_LIMIT: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::Semaphore::new(64)));

/// Process-wide cap on concurrently-running deploy pipelines. Each accepted
/// deploy spawns a detached build/run task; without a cap a burst of requests
/// could spawn unbounded build pipelines and exhaust CPU/memory/file
/// descriptors. A permit is held for the lifetime of the spawned task and
/// released when it finishes; over-capacity requests get `429`.
static DEPLOY_CONCURRENCY_LIMIT: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::Semaphore::new(8)));

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

fn ensure_deployment_role(
    state: &AppState,
    principal: &Principal,
    project_id: uuid::Uuid,
    role: Role,
) -> Result<(), ApiError> {
    crate::auth::ensure_role_or_not_found(
        state,
        principal,
        project_id,
        role,
        "deployment not found",
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
    ensure_deployment_role(&state, &principal, service.project_id, Role::Operator)?;

    // Bound the number of in-flight deploy pipelines. The permit is moved into
    // the spawned task and released when it finishes; if the cap is reached the
    // operator gets a 429 instead of overwhelming the host with builds.
    let deploy_permit = DEPLOY_CONCURRENCY_LIMIT
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            ApiError::TooManyRequests("too many concurrent deployments, retry shortly".to_string())
        })?;

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
    let upload_cleanup = match &request {
        crate::domain::DeploymentRequest::Upload { upload_id, .. } => {
            uuid::Uuid::parse_str(upload_id)
                .ok()
                .map(|id| state.config.uploads_dir.join(id.to_string()))
        }
        _ => None,
    };
    // On-demand TLS issuance (review HIGH): first deploy of a tls_enabled
    // service with already-verified domains must trigger cert issuance so
    // `:443` serves immediately, not after the next 12h renewal scan. Capture
    // the request channel + domains repo for the spawned task.
    let cert_issue_tx = state.cert_issue_tx.clone();
    let domains_repo = state.domains.clone();
    let tls_enabled = service.tls_enabled;

    tokio::spawn(async move {
        // Hold the concurrency permit for the lifetime of the pipeline; it is
        // released when this task ends (success or failure).
        let _deploy_permit = deploy_permit;
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
        // On-demand TLS issuance for a successful deploy of a tls_enabled
        // service: request a cert for each already-verified hostname that the
        // deploy just made routable. The ACME task skips any hostname that
        // already has a cert, so this is idempotent across redeploys.
        if run.is_ok() && tls_enabled {
            match domains_repo.list_verified_hostnames(service_id) {
                Ok(hostnames) => {
                    for hostname in hostnames {
                        crate::ingress::pingora::request_issue(&cert_issue_tx, hostname);
                    }
                }
                Err(error) => {
                    let _ = log.write(
                        "TLS",
                        &format!("could not list verified hostnames for issuance: {error}"),
                    );
                }
            }
        }
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
        // Best-effort remove the staged upload directory after the deploy
        // pipeline finishes (success OR failure). The directory is no longer
        // needed once the build context has been consumed by `run_with_deps`.
        // Use `tokio::fs` so this end-of-task cleanup never blocks a runtime
        // worker (review LOW — sync I/O on the async deploy task).
        if let Some(dir) = upload_cleanup {
            let _ = tokio::fs::remove_dir_all(&dir).await;
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
    ensure_deployment_role(&state, &principal, service.project_id, Role::Viewer)?;
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
    ensure_deployment_role(&state, &principal, service.project_id, Role::Viewer)?;
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
    ensure_deployment_role(&state, &principal, service.project_id, Role::Operator)?;

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

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use crate::app::{AppState, build_router};
    use crate::config::AppConfig;
    use crate::domain::{
        DeploymentStatus, ExternalImageSource, HealthCheck, ServiceConfig, ServiceSource,
    };

    const ADMIN_TOKEN: &str = "test-admin-token-0123456789abcdef";

    fn test_state_with_dirs(uploads_dir: &std::path::Path, log_dir: &std::path::Path) -> AppState {
        let mut config = AppConfig::for_test(ADMIN_TOKEN);
        config.uploads_dir = uploads_dir.to_path_buf();
        config.log_dir = log_dir.to_path_buf();
        AppState::builder(config).build()
    }

    fn make_service(state: &AppState) -> ServiceConfig {
        let project_id = state.projects.default_project_id().unwrap();
        state
            .services
            .put_service(
                ServiceConfig::new(
                    project_id,
                    "web",
                    Vec::new(),
                    ServiceSource::ExternalImage(ExternalImageSource {
                        image: "busybox".to_string(),
                        credential: None,
                        registry_id: None,
                        image_ref: None,
                    }),
                    8080,
                    HealthCheck::new("/", 5),
                    None,
                    Vec::new(),
                )
                .unwrap(),
            )
            .unwrap()
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn upload_cleanup_rejects_non_uuid_before_building_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let uploads_dir = tmp.path().join("uploads");
        let log_dir = tmp.path().join("logs");
        std::fs::create_dir_all(&uploads_dir).unwrap();
        let outside_dir = tmp.path().join("must-survive");
        std::fs::create_dir_all(&outside_dir).unwrap();
        std::fs::write(outside_dir.join("sentinel"), b"keep").unwrap();

        let state = test_state_with_dirs(&uploads_dir, &log_dir);
        let service = make_service(&state);
        let request_body = serde_json::json!({
            "source": "upload",
            "service_id": service.id,
            "upload_id": outside_dir.to_string_lossy(),
            "dockerfile_path": "Dockerfile",
            "context_path": "."
        });

        let resp = build_router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/deployments")
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .header("Content-Type", "application/json")
                    .body(Body::from(serde_json::to_vec(&request_body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::ACCEPTED);
        let body = body_json(resp).await;
        let deployment_id = body["id"].as_str().unwrap().parse().unwrap();

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let deployment = state
                    .deployments
                    .get_deployment(deployment_id)
                    .unwrap()
                    .unwrap();
                if deployment.status == DeploymentStatus::Failed {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("deployment should fail after acquirer rejects upload_id");

        assert!(
            outside_dir.join("sentinel").exists(),
            "cleanup must not remove a path derived from a non-UUID upload_id"
        );
    }
}
