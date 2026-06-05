use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
    routing::{get, post},
};
use serde::Serialize;
use serde_json::json;
use std::convert::Infallible;
use std::time::Duration;
use tokio_stream::wrappers::ReceiverStream;

use crate::api::ApiError;
use crate::app::AppState;
use crate::auth::{Principal, ensure_role};
use crate::config::ConfigError;
use crate::deploy::DeploymentCoordinator;
use crate::domain::{DomainStatus, Role, ServiceConfig, ServiceDomain};
use crate::logs::{LogStore, LogTailer};
use crate::metrics::CgroupMetricsReader;

/// Process-wide cap on concurrent SSE log streams (F-8 DoS mitigation).
static LOG_STREAM_LIMIT: std::sync::LazyLock<std::sync::Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(tokio::sync::Semaphore::new(64)));

fn ensure_service_role(
    state: &AppState,
    principal: &Principal,
    project_id: uuid::Uuid,
    role: Role,
) -> Result<(), ApiError> {
    crate::auth::ensure_role_or_not_found(state, principal, project_id, role, "service not found")
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/services", get(list_services).post(put_service))
        .route(
            "/services/{service_id}",
            get(get_service).delete(delete_service),
        )
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
    let roles: std::collections::HashMap<uuid::Uuid, Role> = memberships
        .into_iter()
        .map(|m| (m.project_id, m.role))
        .collect();
    Ok(Json(
        all.into_iter()
            .filter_map(|mut s| {
                let role = roles.get(&s.project_id)?;
                // Viewers (below Operator) must not see raw env values (F-7).
                if *role < Role::Operator {
                    s.redact_env();
                }
                Some(s)
            })
            .collect(),
    ))
}

async fn put_service(
    State(state): State<AppState>,
    principal: Principal,
    Json(mut service): Json<ServiceConfig>,
) -> Result<Json<ServiceConfig>, ApiError> {
    ensure_role(&state, &principal, service.project_id, Role::Operator)?;
    // A service with no domain cannot have ACME-issued TLS. Domain management
    // now stores verification rows separately from the legacy ServiceConfig
    // `domains` array, so a TLS toggle can arrive with `domains: []` even when
    // this existing service already has verified hostnames. Preserve that
    // operator intent by hydrating the legacy array from verified domain rows.
    if service.tls_enabled && service.domains.is_empty() && !service.id.is_nil() {
        service.domains = state.domains.list_verified_hostnames(service.id)?;
    }
    if service.domains.is_empty() {
        service.tls_enabled = false;
    }
    // The body is deserialized straight into ServiceConfig, bypassing the
    // `ServiceConfig::new` constructor, so re-validate every invariant here:
    // safe service name (F-6), git build-path confinement (F-1), domains, port.
    service
        .validate()
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    if let crate::domain::ServiceSource::ExternalImage(src) = &service.source
        && src.credential.is_some()
    {
        return Err(ApiError::BadRequest(
            "legacy external image credential refs are no longer supported; configure a project registry and set registry_id/image_ref".to_string(),
        ));
    }
    if let crate::domain::ServiceSource::ExternalImage(src) = &service.source
        && let Some(registry_id) = src.registry_id
    {
        let registry = state
            .registries
            .registry(registry_id)?
            .ok_or_else(|| ApiError::NotFound("registry not found".into()))?;
        if registry.project_id != service.project_id {
            return Err(ApiError::NotFound("registry not found".into()));
        }
    }
    // Validate ACME email before persisting a service that enables TLS.
    state
        .config
        .require_acme_email(service.tls_enabled)
        .map_err(|e| match e {
            ConfigError::AcmeEmailRequired => {
                ApiError::BadRequest("DENIA_ACME_EMAIL must be set to enable TLS".to_string())
            }
            other => ApiError::BadRequest(other.to_string()),
        })?;
    // Resolve the id server-side. The client never supplies a UUIDv7 (browsers
    // can only mint v4), so on a nil id we reuse the existing row's id for the
    // same (project_id, name) — the upsert keys on that pair and never updates
    // the PK — and otherwise mint a fresh v7. A non-nil id is the update path.
    if service.id.is_nil() {
        let existing = state
            .services
            .list_services()?
            .into_iter()
            .find(|s| s.project_id == service.project_id && s.name == service.name);
        service.id = match existing {
            Some(found) => found.id,
            None => uuid::Uuid::now_v7(),
        };
    } else {
        let existing = state
            .services
            .get_service(service.id)?
            .ok_or_else(|| ApiError::BadRequest("service id does not exist".to_string()))?;
        if existing.project_id != service.project_id || existing.name != service.name {
            return Err(ApiError::BadRequest(
                "service id does not match existing service project/name".to_string(),
            ));
        }
    }
    let saved = state.services.put_service(service)?;
    // Bridge the form's `ServiceConfig.domains` array into the first-class
    // `service_domains` table (ADR-013). The legacy array is a read-only
    // projection of verified rows, but the create/edit form is the only place an
    // operator types a hostname, so each new hostname must become a `Pending`
    // verification row — otherwise it persists in `config_json` yet never appears
    // in the domains panel, never routes, and never gets a cert (operator has to
    // re-add it by hand).
    sync_domain_rows(&state, &saved)?;
    Ok(Json(saved))
}

/// Reconcile a service's legacy `domains` array into `service_domains` rows.
///
/// Additive only: missing hostnames are inserted as `Pending`; existing rows
/// (any status) are left untouched so a `Verified` domain is never reset to
/// pending, and removals stay an explicit `DELETE /domains/{id}` operation (the
/// array can arrive stale or hydrated from verified rows, so it is not an
/// authoritative delete signal). Reserved control hostnames are rejected and a
/// hostname already owned by another service surfaces as a 409, mirroring
/// `create_service_domain`.
fn sync_domain_rows(state: &AppState, service: &ServiceConfig) -> Result<(), ApiError> {
    if service.domains.is_empty() {
        return Ok(());
    }
    let existing: std::collections::HashSet<String> = state
        .domains
        .list_service_domains_by_service(service.id)?
        .into_iter()
        .map(|d| d.hostname)
        .collect();
    for raw in &service.domains {
        let hostname = crate::verification::validate_hostname(raw)
            .map_err(|e| ApiError::BadRequest(e.to_string()))?;
        if existing.contains(&hostname) {
            continue;
        }
        if crate::api::domains::is_reserved_control_hostname(
            &hostname,
            state.config.control_domain.as_deref(),
        ) {
            return Err(ApiError::Conflict(
                "hostname is reserved for the control plane".into(),
            ));
        }
        let d = ServiceDomain {
            id: uuid::Uuid::now_v7(),
            service_id: service.id,
            hostname,
            status: DomainStatus::Pending,
            challenge_token: crate::verification::generate_token(),
            verified_at: None,
            last_check_at: None,
            last_error: None,
            created_at: chrono::Utc::now(),
        };
        state.domains.put_service_domain(&d).map_err(|e| match e {
            crate::repo::RepoError::Sqlite(ref err)
                if crate::api::error::is_constraint_violation(err) =>
            {
                ApiError::Conflict(format!("hostname already in use: {}", d.hostname))
            }
            other => ApiError::Repo(other),
        })?;
    }
    Ok(())
}

async fn get_service(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(service_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<ServiceConfig>, ApiError> {
    let Some(mut service) = state.services.get_service(service_id)? else {
        return Err(ApiError::NotFound("service not found".to_string()));
    };
    ensure_service_role(&state, &principal, service.project_id, Role::Viewer)?;
    // Only Operators+ (and super admins) see raw env values (F-7).
    if ensure_role(&state, &principal, service.project_id, Role::Operator).is_err() {
        service.redact_env();
    }
    Ok(Json(service))
}

async fn delete_service(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(service_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let Some(service) = state.services.get_service(service_id)? else {
        return Err(ApiError::NotFound("service not found".to_string()));
    };
    ensure_service_role(&state, &principal, service.project_id, Role::Operator)?;
    state.services.delete_service(service_id)?;
    Ok(Json(json!({"deleted": true})))
}

async fn lifecycle_command(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path((service_id, action)): axum::extract::Path<(uuid::Uuid, String)>,
) -> Result<(StatusCode, Json<LifecycleResponse>), ApiError> {
    let Some(service) = state.services.get_service(service_id)? else {
        return Err(ApiError::NotFound("service not found".to_string()));
    };
    ensure_service_role(&state, &principal, service.project_id, Role::Operator)?;
    match action.as_str() {
        "stop" => {
            let coordinator = DeploymentCoordinator::new_with_shared_routing(
                state.deployment_repos(),
                state.runtime.clone(),
                state.health.clone(),
                state.ingress.clone(),
                state.routes.clone(),
            );
            if service.autoscale.is_some() {
                // Autoscaled service: the controller owns the replicas (ADR-028).
                // Drain them FIRST (removes ingress entries + releases the
                // ledger) while the service is still promoted so `drain_all` can
                // resolve its limits via the catalog, THEN tear down routes and
                // clear the promoted row so the autoscaler does not relaunch.
                if let Some(autoscaler) = &state.autoscaler {
                    autoscaler.lock().await.drain_all(service.id).await;
                }
                coordinator.stop_service_routes_only(&service).await?;
            } else {
                coordinator.stop_service(&service).await?;
            }
            Ok((
                StatusCode::ACCEPTED,
                Json(LifecycleResponse { service_id, action }),
            ))
        }
        // `start`/`restart` are part of the ADR-008 lifecycle surface but their
        // implementation lives in the deploy pipeline (owned elsewhere) and is
        // not yet wired here. Distinguish a known-but-unimplemented action from
        // an outright unknown one so clients get an accurate signal.
        "start" | "restart" => Err(ApiError::BadRequest(format!(
            "lifecycle action not yet implemented: {action}"
        ))),
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
    ensure_service_role(&state, &principal, service.project_id, Role::Operator)?;
    let logs = LogStore::new(&state.config.log_dir);
    // Logs are stored by service_id (globally unique), not name (F-3).
    match logs.read_recent(&service.id.to_string(), 200) {
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
    ensure_service_role(&state, &principal, service.project_id, Role::Operator)?;

    // Bound the number of concurrent SSE log streams process-wide. Each stream
    // holds a long-lived task polling a file; without a cap a client could open
    // unbounded streams and exhaust tasks/file descriptors (F-8). The permit is
    // moved into the streaming task and released when the client disconnects.
    let permit = LOG_STREAM_LIMIT
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::TooManyRequests("too many concurrent log streams".to_string()))?;

    let log_path = std::path::Path::new(&state.config.log_dir).join(format!("{}.log", service.id));

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Event, Infallible>>(256);

    tokio::spawn(async move {
        let _permit = permit;
        let mut tailer = LogTailer::new(&log_path);

        if let Ok(lines) = tokio::task::block_in_place(|| tailer.backlog(200)) {
            for line in lines {
                if tx.send(Ok(Event::default().data(line))).await.is_err() {
                    return;
                }
            }
        }

        let mut interval = tokio::time::interval(Duration::from_millis(300));
        loop {
            interval.tick().await;
            let result = tokio::task::block_in_place(|| tailer.poll());
            match result {
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
    ensure_service_role(&state, &principal, service.project_id, Role::Viewer)?;
    let Some(deployment_id) = state.deployments.promoted_deployment(service_id)? else {
        return Ok(Json(Vec::new()));
    };
    let reader = CgroupMetricsReader::new(state.config.cgroup_root.clone());
    // A scaled-to-zero service has no live cgroup, so the read hits ENOENT. Treat
    // that as "no metrics yet" (empty, 200) rather than a 500 — matching the
    // sibling `/v1/workloads` endpoint and the no-promoted-deployment branch above.
    // The cgroup read is blocking fs I/O; run it under `block_in_place` so it does
    // not stall the async executor thread, matching the SSE log path (review 07 LOW).
    match tokio::task::block_in_place(|| {
        reader.read_by_id(&service.name, service.id, deployment_id)
    }) {
        Ok(snapshot) => Ok(Json(vec![snapshot])),
        Err(_) => Ok(Json(Vec::new())),
    }
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

    fn test_state_with_acme_email() -> AppState {
        let mut config = AppConfig::for_test(ADMIN_TOKEN);
        config.acme_email = Some("ops@example.com".to_string());
        AppState::builder(config).build()
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

    #[tokio::test(flavor = "multi_thread")]
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

        // Seed a backlog line in the service log file (named by service_id).
        std::fs::create_dir_all(&log_dir).unwrap();
        let log_path = log_dir.join(format!("{service_id}.log"));
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
    async fn put_service_with_tls_and_no_acme_email_returns_400() {
        use crate::domain::{
            ExternalImageSource, HealthCheck, Project, ServiceConfig, ServiceSource,
        };
        let state = test_state(); // acme_email is None in for_test
        let project = Project::new("team-tls", None).unwrap();
        state.projects.put_project(project.clone()).unwrap();
        let mut svc = ServiceConfig::new(
            project.id,
            "tlssvc",
            vec!["tls.example.com".into()],
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
        svc.tls_enabled = true;
        let body = serde_json::to_vec(&svc).unwrap();

        let resp = build_router(state)
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
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let msg = body_string(resp).await;
        assert!(
            msg.contains("DENIA_ACME_EMAIL"),
            "expected ACME error in: {msg}"
        );
    }

    #[tokio::test]
    async fn create_then_list_service_roundtrips() {
        use crate::domain::{Project, ServiceConfig};

        let state = test_state();
        let project = Project::new("team-a", None).unwrap();
        state.projects.put_project(project.clone()).unwrap();
        let body = service_create_body(project.id, "web");

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

    use crate::domain::{ExternalImageSource, HealthCheck, Project, ServiceConfig, ServiceSource};

    /// Build a service-create JSON body that omits `id`, so the server must
    /// resolve it. Mirrors what the web client POSTs.
    fn service_create_body(project_id: uuid::Uuid, name: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "project_id": project_id,
            "name": name,
            "domains": [format!("{name}.example.com")],
            "source": {
                "type": "external_image",
                "image": "nginx",
                "credential": null,
            },
            "internal_port": 80,
            "health_check": { "path": "/health", "timeout_seconds": 5 },
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn create_without_id_mints_v7_then_get_roundtrips() {
        let state = test_state();
        let project = Project::new("team-create", None).unwrap();
        state.projects.put_project(project.clone()).unwrap();
        let body = service_create_body(project.id, "web");

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
        let created: ServiceConfig = serde_json::from_str(&body_string(create).await).unwrap();
        assert!(!created.id.is_nil(), "server must mint an id");
        assert_eq!(
            created.id.get_version(),
            Some(uuid::Version::SortRand),
            "minted id must be a UUIDv7"
        );

        let get = app
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/services/{}", created.id))
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get.status(), StatusCode::OK);
        let fetched: ServiceConfig = serde_json::from_str(&body_string(get).await).unwrap();
        assert_eq!(fetched.id, created.id);
    }

    #[tokio::test]
    async fn get_unknown_service_returns_404() {
        let resp = build_router(test_state())
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/services/{}", uuid::Uuid::now_v7()))
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_service_in_foreign_project_returns_404() {
        let state = test_state();
        let project = Project::new("foreign-team", None).unwrap();
        state.projects.put_project(project.clone()).unwrap();
        let stranger = state.users.create_user("stranger", "hash", false).unwrap();
        let stranger_token = state
            .tokens
            .create_api_token(stranger.id, "stranger")
            .unwrap()
            .token;
        let svc = ServiceConfig::new(
            project.id,
            "foreignsvc",
            vec!["foreignsvc.example.com".into()],
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

        let resp = build_router(state)
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/services/{service_id}"))
                    .header("Authorization", format!("Bearer {stranger_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_service_then_get_returns_404() {
        let state = test_state();
        let project = Project::new("team-del", None).unwrap();
        state.projects.put_project(project.clone()).unwrap();
        let svc = ServiceConfig::new(
            project.id,
            "delsvc",
            vec!["delsvc.example.com".into()],
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

        let app = build_router(state);
        let delete = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/v1/services/{service_id}"))
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(delete.status(), StatusCode::OK);

        let get = app
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/services/{service_id}"))
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(get.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_service_unauthenticated_returns_401() {
        let resp = build_router(test_state())
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/v1/services/{}", uuid::Uuid::now_v7()))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn put_service_no_domain_coerces_tls_off() {
        let state = test_state(); // acme_email is None in for_test
        let project = Project::new("team-nodomain", None).unwrap();
        state.projects.put_project(project.clone()).unwrap();

        let body = serde_json::to_vec(&serde_json::json!({
            "project_id": project.id,
            "name": "nodomain",
            "domains": [],
            "source": { "type": "external_image", "image": "nginx", "credential": null },
            "internal_port": 80,
            "health_check": { "path": "/health", "timeout_seconds": 5 },
            "tls_enabled": true,
        }))
        .unwrap();

        let resp = build_router(state)
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

        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "no-domain service must be accepted"
        );
        let cfg: ServiceConfig = serde_json::from_str(&body_string(resp).await).unwrap();
        assert!(cfg.domains.is_empty());
        assert!(!cfg.tls_enabled, "tls must be coerced off when no domain");
    }

    #[tokio::test]
    async fn put_service_tls_uses_verified_domain_rows_when_body_domains_are_stale() {
        use crate::domain::{
            DomainStatus, ExternalImageSource, HealthCheck, Project, ServiceConfig, ServiceDomain,
            ServiceSource,
        };
        let state = test_state_with_acme_email();
        let project = Project::new("team-domainrows", None).unwrap();
        state.projects.put_project(project.clone()).unwrap();

        let svc = state
            .services
            .put_service(
                ServiceConfig::new(
                    project.id,
                    "web",
                    vec![],
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
                .unwrap(),
            )
            .unwrap();
        state
            .domains
            .put_service_domain(&ServiceDomain {
                id: uuid::Uuid::now_v7(),
                service_id: svc.id,
                hostname: "web.example.com".into(),
                status: DomainStatus::Verified,
                challenge_token: "tok".into(),
                verified_at: Some(chrono::Utc::now()),
                last_check_at: Some(chrono::Utc::now()),
                last_error: None,
                created_at: chrono::Utc::now(),
            })
            .unwrap();

        let mut update = svc;
        update.domains = vec![];
        update.tls_enabled = true;
        let body = serde_json::to_vec(&update).unwrap();

        let resp = build_router(state)
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

        assert_eq!(resp.status(), StatusCode::OK);
        let cfg: ServiceConfig = serde_json::from_str(&body_string(resp).await).unwrap();
        assert_eq!(cfg.domains, vec!["web.example.com"]);
        assert!(cfg.tls_enabled);
    }

    #[tokio::test]
    async fn create_without_id_twice_reuses_existing_id() {
        let state = test_state();
        let project = Project::new("team-stable", None).unwrap();
        state.projects.put_project(project.clone()).unwrap();

        let app = build_router(state);

        let first = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/services")
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .header("Content-Type", "application/json")
                    .body(Body::from(service_create_body(project.id, "stable")))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let first_cfg: ServiceConfig = serde_json::from_str(&body_string(first).await).unwrap();

        let second = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/services")
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .header("Content-Type", "application/json")
                    .body(Body::from(service_create_body(project.id, "stable")))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::OK);
        let second_cfg: ServiceConfig = serde_json::from_str(&body_string(second).await).unwrap();

        assert_eq!(
            second_cfg.id, first_cfg.id,
            "re-POST with same (project_id, name) and nil id must reuse the existing id"
        );
    }

    #[tokio::test]
    async fn put_service_rejects_body_id_that_differs_from_existing_name_row() {
        let state = test_state();
        let victim_project = Project::new("team-victim", None).unwrap();
        let attacker_project = Project::new("team-attacker", None).unwrap();
        state.projects.put_project(victim_project.clone()).unwrap();
        state
            .projects
            .put_project(attacker_project.clone())
            .unwrap();

        let app = build_router(state);
        let victim = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/services")
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .header("Content-Type", "application/json")
                    .body(Body::from(service_create_body(victim_project.id, "victim")))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(victim.status(), StatusCode::OK);
        let victim_cfg: ServiceConfig = serde_json::from_str(&body_string(victim).await).unwrap();

        let attacker = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/services")
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .header("Content-Type", "application/json")
                    .body(Body::from(service_create_body(
                        attacker_project.id,
                        "attacker",
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(attacker.status(), StatusCode::OK);

        let poisoned = serde_json::to_vec(&serde_json::json!({
            "id": victim_cfg.id,
            "project_id": attacker_project.id,
            "name": "attacker",
            "domains": ["attacker.example.com"],
            "source": { "type": "external_image", "image": "nginx", "credential": null },
            "internal_port": 80,
            "health_check": { "path": "/health", "timeout_seconds": 5 }
        }))
        .unwrap();

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/services")
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .header("Content-Type", "application/json")
                    .body(Body::from(poisoned))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn put_service_rejects_legacy_external_image_credential_ref() {
        let state = test_state();
        let project = Project::new("team-legacy-cred", None).unwrap();
        state.projects.put_project(project.clone()).unwrap();

        let body = serde_json::to_vec(&serde_json::json!({
            "project_id": project.id,
            "name": "legacycred",
            "domains": ["legacycred.example.com"],
            "source": {
                "type": "external_image",
                "image": "ghcr.io/acme/web:1",
                "credential": "legacy-cred"
            },
            "internal_port": 80,
            "health_check": { "path": "/health", "timeout_seconds": 5 }
        }))
        .unwrap();

        let resp = build_router(state)
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

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn create_service_seeds_pending_domain_rows() {
        use crate::domain::{DomainStatus, ServiceDomain};

        let state = test_state();
        let project = Project::new("team-domains", None).unwrap();
        state.projects.put_project(project.clone()).unwrap();

        let app = build_router(state);
        let create = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/services")
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .header("Content-Type", "application/json")
                    .body(Body::from(service_create_body(project.id, "web")))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create.status(), StatusCode::OK);
        let created: ServiceConfig = serde_json::from_str(&body_string(create).await).unwrap();

        // The hostname typed in the create form must land in the first-class
        // `service_domains` table as a Pending row, not only in config_json.
        let domains = app
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/services/{}/domains", created.id))
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(domains.status(), StatusCode::OK);
        let rows: Vec<ServiceDomain> =
            serde_json::from_str(&body_string(domains).await).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].hostname, "web.example.com");
        assert_eq!(rows[0].status, DomainStatus::Pending);
    }

    #[tokio::test]
    async fn re_post_same_domain_does_not_duplicate_rows() {
        use crate::domain::ServiceDomain;

        let state = test_state();
        let project = Project::new("team-dom-idem", None).unwrap();
        state.projects.put_project(project.clone()).unwrap();

        let app = build_router(state);
        let mut created_id = None;
        for _ in 0..2 {
            let resp = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/v1/services")
                        .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                        .header("Content-Type", "application/json")
                        .body(Body::from(service_create_body(project.id, "web")))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            let cfg: ServiceConfig = serde_json::from_str(&body_string(resp).await).unwrap();
            created_id = Some(cfg.id);
        }

        let domains = app
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/services/{}/domains", created_id.unwrap()))
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(domains.status(), StatusCode::OK);
        let rows: Vec<ServiceDomain> =
            serde_json::from_str(&body_string(domains).await).unwrap();
        assert_eq!(rows.len(), 1, "re-POST must not duplicate the domain row");
    }
}
