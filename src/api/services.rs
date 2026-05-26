use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
    routing::{get, post},
};
use serde::Serialize;
use std::convert::Infallible;
use std::time::Duration;
use tokio_stream::wrappers::ReceiverStream;

use crate::api::ApiError;
use crate::app::AppState;
use crate::auth::{Principal, ensure_role};
use crate::deploy::DeploymentCoordinator;
use crate::domain::{Role, ServiceConfig};
use crate::logs::{LogStore, LogTailer};
use crate::metrics::CgroupMetricsReader;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/services", get(list_services).post(put_service))
        .route("/services/{service_id}/logs", get(service_logs))
        .route(
            "/services/{service_id}/logs/stream",
            get(service_logs_stream),
        )
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

async fn service_logs_stream(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(service_id): axum::extract::Path<uuid::Uuid>,
) -> Result<impl axum::response::IntoResponse, ApiError> {
    let Some(service) = state.services.get_service(service_id)? else {
        return Err(ApiError::NotFound("service not found".to_string()));
    };
    ensure_role(&state, &principal, service.project_id, Role::Operator)?;

    let log_path =
        std::path::Path::new(&state.config.log_dir).join(format!("{}.log", service.name));

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(256);

    tokio::spawn(async move {
        let mut tailer = LogTailer::new(&log_path);

        if let Ok(lines) = tailer.backlog(200) {
            for line in lines {
                if tx.send(Ok(Event::default().data(line))).await.is_err() {
                    return;
                }
            }
        }

        let mut interval = tokio::time::interval(Duration::from_millis(300));
        loop {
            interval.tick().await;
            match tailer.poll() {
                Ok(lines) => {
                    for line in lines {
                        if tx.send(Ok(Event::default().data(line))).await.is_err() {
                            return;
                        }
                    }
                }
                Err(_) => continue, // transient read error; retry next tick
            }
        }
    });

    Ok(Sse::new(ReceiverStream::new(rx))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15))))
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

#[cfg(test)]
mod tests {
    use crate::app::{AppState, build_router};
    use crate::config::AppConfig;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    const ADMIN_TOKEN: &str = "test-admin-token-0123456789abcdef";

    fn test_state() -> AppState {
        AppState::builder(AppConfig::for_test(ADMIN_TOKEN)).build()
    }

    async fn body_string(resp: axum::response::Response) -> String {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn list_services_empty_returns_200() {
        let resp = build_router(test_state())
            .oneshot(
                Request::builder()
                    .uri("/v1/services")
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(body_string(resp).await, "[]");
    }

    #[tokio::test]
    async fn list_services_unauthenticated_returns_401() {
        let resp = build_router(test_state())
            .oneshot(
                Request::builder()
                    .uri("/v1/services")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn log_stream_unauthenticated_returns_401() {
        let resp = build_router(test_state())
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/services/{}/logs/stream", uuid::Uuid::now_v7()))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn log_stream_emits_backlog_then_live() {
        use crate::domain::{
            ExternalImageSource, HealthCheck, Project, ServiceConfig, ServiceSource,
        };
        use tokio_stream::StreamExt;

        let state = test_state();
        let log_dir = state.config.log_dir.clone();
        let project = Project::new("team-stream", None).unwrap();
        state.projects.put_project(project.clone()).unwrap();
        let svc = ServiceConfig::new(
            project.id,
            "streamsvc",
            vec!["stream.example.com".into()],
            ServiceSource::ExternalImage(ExternalImageSource {
                image: "nginx".into(),
                credential: None,
                registry_id: None,
                image_ref: None,
            }),
            80,
            HealthCheck::new("/health", 5),
            None,
            Vec::new(),
        )
        .unwrap();
        let service_id = svc.id;
        state.services.put_service(svc).unwrap();

        // Seed a backlog line in the service log file.
        std::fs::create_dir_all(&log_dir).unwrap();
        let log_path = log_dir.join("streamsvc.log");
        std::fs::write(&log_path, "backlog-line\n").unwrap();

        let resp = build_router(state)
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/services/{service_id}/logs/stream"))
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let ct = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        assert!(ct.starts_with("text/event-stream"), "content-type was {ct}");

        // Read bounded frames under a timeout; the stream itself never ends.
        let mut stream = resp.into_body().into_data_stream();
        let mut seen = String::new();

        // Backlog frame.
        let frame = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
            .await
            .expect("backlog frame timed out")
            .expect("stream ended")
            .unwrap();
        seen.push_str(&String::from_utf8_lossy(&frame));

        // Append a live line; poll interval is 300ms.
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&log_path)
                .unwrap();
            f.write_all(b"live-line\n").unwrap();
        }

        // Drain a few more frames (skipping keep-alive comments) for the live line.
        for _ in 0..10 {
            if seen.contains("live-line") {
                break;
            }
            if let Ok(Some(Ok(frame))) =
                tokio::time::timeout(std::time::Duration::from_secs(2), stream.next()).await
            {
                seen.push_str(&String::from_utf8_lossy(&frame));
            }
        }

        assert!(seen.contains("backlog-line"), "missing backlog in: {seen}");
        assert!(seen.contains("live-line"), "missing live line in: {seen}");
        // Dropping `stream` here cancels the tailer task.
    }

    #[tokio::test]
    async fn create_then_list_service_roundtrips() {
        use crate::domain::{
            ExternalImageSource, HealthCheck, Project, ServiceConfig, ServiceSource,
        };
        let state = test_state();
        let project = Project::new("team-a", None).unwrap();
        state.projects.put_project(project.clone()).unwrap();
        let svc = ServiceConfig::new(
            project.id,
            "web",
            vec!["example.com".into()],
            ServiceSource::ExternalImage(ExternalImageSource {
                image: "nginx".into(),
                credential: None,
                registry_id: None,
                image_ref: None,
            }),
            80,
            HealthCheck::new("/health", 5),
            None,
            Vec::new(),
        )
        .unwrap();
        let body = serde_json::to_vec(&svc).unwrap();

        let app = build_router(state);
        let create = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/services")
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .header("Content-Type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create.status(), StatusCode::OK);

        let list = app
            .oneshot(
                Request::builder()
                    .uri("/v1/services")
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list.status(), StatusCode::OK);
        let listed: Vec<ServiceConfig> = serde_json::from_str(&body_string(list).await).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "web");
    }
}
