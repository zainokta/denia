//! Service console API: replica discovery, single-use ticket minting, and the
//! ticket-authenticated websocket upgrade. See ADR-033.
//!
//! Tickets and session-limit semaphores live in module-local statics rather
//! than on `AppState`: the console is a transient interactive surface, not
//! control-plane state, and keeping them here avoids widening the shared
//! application state (see the plan's safety notes on `AppState`).

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use axum::{
    Json, Router,
    extract::{
        Path, Query, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    response::IntoResponse,
    routing::{get, post},
};
use chrono::{DateTime, Utc};
use rand::RngExt;
use tokio::sync::Semaphore;
use uuid::Uuid;

use crate::api::ApiError;
use crate::app::AppState;
use crate::auth::{Principal, ensure_role};
use crate::domain::{
    ConsoleControlFrame, ConsoleReplicaView, ConsoleTicketResponse, CreateConsoleTicketRequest,
    Role,
};
use crate::runtime::{RuntimeConsoleRequest, RuntimeConsoleSession};

const TICKET_TTL: Duration = Duration::from_secs(30);
const MAX_CONSOLE_SESSIONS: usize = 16;
const MAX_CONSOLE_SESSIONS_PER_SERVICE: usize = 2;

type TicketStore = Arc<Mutex<HashMap<String, ConsoleTicket>>>;
type ServiceLimitStore = Arc<Mutex<HashMap<Uuid, Arc<Semaphore>>>>;

static TICKETS: LazyLock<TicketStore> = LazyLock::new(|| Arc::new(Mutex::new(HashMap::new())));
static CONSOLE_LIMIT: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(MAX_CONSOLE_SESSIONS)));
static SERVICE_LIMITS: LazyLock<ServiceLimitStore> =
    LazyLock::new(|| Arc::new(Mutex::new(HashMap::new())));

#[derive(Debug, Clone)]
struct ConsoleTicket {
    service_id: Uuid,
    service_name: String,
    deployment_id: Uuid,
    replica_index: u32,
    principal_label: String,
    cols: u16,
    rows: u16,
    expires_at: DateTime<Utc>,
}

/// Bearer-authenticated console routes (replica discovery + ticket minting).
pub fn router() -> Router<AppState> {
    Router::new()
        .route(
            "/services/{service_id}/console/replicas",
            get(list_console_replicas),
        )
        .route(
            "/services/{service_id}/console/tickets",
            post(create_console_ticket),
        )
}

/// Ticket-authenticated websocket route. Registered OUTSIDE the bearer-auth
/// layer because browser websockets cannot send an `Authorization` header — the
/// single-use ticket minted by [`create_console_ticket`] is the credential.
pub fn public_router() -> Router<AppState> {
    Router::new().route("/services/{service_id}/console/ws", get(console_ws))
}

fn new_ticket() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill(&mut bytes);
    hex::encode(bytes)
}

async fn list_console_replicas(
    State(state): State<AppState>,
    principal: Principal,
    Path(service_id): Path<Uuid>,
) -> Result<Json<Vec<ConsoleReplicaView>>, ApiError> {
    let service = state
        .services
        .get_service(service_id)?
        .ok_or_else(|| ApiError::NotFound("service not found".to_string()))?;
    ensure_role(&state, &principal, service.project_id, Role::Operator)?;

    let promoted = state
        .deployments
        .promoted_deployment(service.id)?
        .ok_or_else(|| ApiError::Conflict("service has no promoted deployment".to_string()))?;

    let running = state.runtime.list_running().await?;
    let replicas = running
        .into_iter()
        .filter(|status| status.service_id == service.id && status.deployment_id == promoted)
        .map(|status| ConsoleReplicaView {
            service_id: status.service_id,
            service_name: status.service_name,
            deployment_id: status.deployment_id,
            replica_index: status.replica_index,
            state: status.state,
        })
        .collect();
    Ok(Json(replicas))
}

async fn create_console_ticket(
    State(state): State<AppState>,
    principal: Principal,
    Path(service_id): Path<Uuid>,
    Json(input): Json<CreateConsoleTicketRequest>,
) -> Result<Json<ConsoleTicketResponse>, ApiError> {
    let input = input.normalized();
    let service = state
        .services
        .get_service(service_id)?
        .ok_or_else(|| ApiError::NotFound("service not found".to_string()))?;
    ensure_role(&state, &principal, service.project_id, Role::Operator)?;

    let promoted = state
        .deployments
        .promoted_deployment(service.id)?
        .ok_or_else(|| ApiError::Conflict("service has no promoted deployment".to_string()))?;

    let running = state.runtime.list_running().await?;
    let target = running
        .into_iter()
        .find(|status| {
            status.service_id == service.id
                && status.deployment_id == promoted
                && status.replica_index == input.replica_index
        })
        .ok_or_else(|| ApiError::Conflict("selected replica is not running".to_string()))?;

    let ticket = new_ticket();
    let expires_at = Utc::now() + chrono::TimeDelta::seconds(TICKET_TTL.as_secs() as i64);
    let principal_label = match principal.user_id {
        Some(user_id) => user_id.to_string(),
        None => "super-admin-token".to_string(),
    };
    let value = ConsoleTicket {
        service_id: service.id,
        service_name: service.name,
        deployment_id: target.deployment_id,
        replica_index: target.replica_index,
        principal_label,
        cols: input.cols,
        rows: input.rows,
        expires_at,
    };

    TICKETS
        .lock()
        .map_err(|_| ApiError::Conflict("console ticket store unavailable".to_string()))?
        .insert(ticket.clone(), value);

    Ok(Json(ConsoleTicketResponse {
        ws_path: format!("/v1/services/{service_id}/console/ws?ticket={ticket}"),
        ticket,
        expires_at,
    }))
}

fn consume_ticket(service_id: Uuid, ticket: &str) -> Result<ConsoleTicket, ApiError> {
    let mut tickets = TICKETS
        .lock()
        .map_err(|_| ApiError::Conflict("console ticket store unavailable".to_string()))?;
    let ticket_value = tickets
        .remove(ticket)
        .ok_or_else(|| ApiError::Unauthorized("invalid console ticket".to_string()))?;
    if ticket_value.service_id != service_id {
        return Err(ApiError::Unauthorized(
            "console ticket service mismatch".to_string(),
        ));
    }
    if ticket_value.expires_at <= Utc::now() {
        return Err(ApiError::Unauthorized("console ticket expired".to_string()));
    }
    Ok(ticket_value)
}

fn service_limit(service_id: Uuid) -> Result<Arc<Semaphore>, ApiError> {
    let mut limits = SERVICE_LIMITS
        .lock()
        .map_err(|_| ApiError::Conflict("console service limit unavailable".to_string()))?;
    Ok(limits
        .entry(service_id)
        .or_insert_with(|| Arc::new(Semaphore::new(MAX_CONSOLE_SESSIONS_PER_SERVICE)))
        .clone())
}

async fn console_ws(
    State(state): State<AppState>,
    Path(service_id): Path<Uuid>,
    Query(params): Query<HashMap<String, String>>,
    ws: WebSocketUpgrade,
) -> Result<impl IntoResponse, ApiError> {
    let ticket = params
        .get("ticket")
        .ok_or_else(|| ApiError::Unauthorized("missing console ticket".to_string()))?;
    let ticket = consume_ticket(service_id, ticket)?;
    let global_permit = CONSOLE_LIMIT
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::TooManyRequests("too many console sessions".to_string()))?;
    let service_permit = service_limit(service_id)?
        .try_acquire_owned()
        .map_err(|_| {
            ApiError::TooManyRequests("too many console sessions for service".to_string())
        })?;

    Ok(ws.on_upgrade(move |socket| async move {
        let _global_permit = global_permit;
        let _service_permit = service_permit;
        handle_console_socket(state, socket, ticket).await;
    }))
}

async fn handle_console_socket(state: AppState, mut socket: WebSocket, ticket: ConsoleTicket) {
    let session_id = Uuid::now_v7();
    tracing::info!(
        %session_id,
        service_id = %ticket.service_id,
        deployment_id = %ticket.deployment_id,
        replica_index = ticket.replica_index,
        principal = %ticket.principal_label,
        "console session start"
    );

    let request = RuntimeConsoleRequest {
        session_id,
        service_id: ticket.service_id,
        service_name: ticket.service_name.clone(),
        deployment_id: ticket.deployment_id,
        replica_index: ticket.replica_index,
        cols: ticket.cols,
        rows: ticket.rows,
    };

    let session = match state.runtime.open_console(request).await {
        Ok(session) => session,
        Err(error) => {
            let _ = socket
                .send(Message::Text(
                    serde_json::to_string(&ConsoleControlFrame::Error {
                        message: error.to_string(),
                    })
                    .unwrap_or_else(|_| {
                        "{\"type\":\"error\",\"message\":\"console failed\"}".to_string()
                    })
                    .into(),
                ))
                .await;
            return;
        }
    };

    let _ = socket
        .send(Message::Text(
            serde_json::to_string(&ConsoleControlFrame::Ready {
                session_id,
                replica_index: ticket.replica_index,
                cols: ticket.cols,
                rows: ticket.rows,
            })
            .unwrap_or_else(|_| "{\"type\":\"ready\"}".to_string())
            .into(),
        ))
        .await;

    bridge_console_socket(socket, session).await;

    tracing::info!(
        %session_id,
        service_id = %ticket.service_id,
        deployment_id = %ticket.deployment_id,
        replica_index = ticket.replica_index,
        principal = %ticket.principal_label,
        "console session end"
    );
}

async fn bridge_console_socket(mut socket: WebSocket, mut session: RuntimeConsoleSession) {
    use axum::extract::ws::CloseFrame;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut buf = vec![0_u8; 8192];
    loop {
        tokio::select! {
            read = session.pty.read(&mut buf) => {
                match read {
                    Ok(0) => break,
                    Ok(n) => {
                        if socket
                            .send(Message::Binary(buf[..n].to_vec().into()))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = send_control(
                            &mut socket,
                            ConsoleControlFrame::Error { message: error.to_string() },
                        )
                        .await;
                        break;
                    }
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(Message::Binary(bytes))) => {
                        if session.pty.write_all(&bytes).await.is_err() {
                            break;
                        }
                    }
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<ConsoleControlFrame>(text.as_str()) {
                            Ok(ConsoleControlFrame::Resize { cols, rows }) => {
                                let _ = session.pty.resize(cols, rows);
                            }
                            Ok(ConsoleControlFrame::Close) => break,
                            _ => {}
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {}
                    Some(Err(_)) => break,
                }
            }
        }
    }

    let _ = crate::syscall::signal::kill(session.child_pid, rustix::process::Signal::TERM);
    let _ = send_control(&mut socket, ConsoleControlFrame::Exit { code: None }).await;
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code: axum::extract::ws::close_code::NORMAL,
            reason: "console closed".into(),
        })))
        .await;
}

async fn send_control(
    socket: &mut WebSocket,
    frame: ConsoleControlFrame,
) -> Result<(), axum::Error> {
    socket
        .send(Message::Text(
            serde_json::to_string(&frame)
                .unwrap_or_else(|_| {
                    "{\"type\":\"error\",\"message\":\"control encode failed\"}".to_string()
                })
                .into(),
        ))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use crate::app::{AppState, build_router};
    use crate::artifacts::{ArtifactKind, ArtifactRecord, ArtifactSource};
    use crate::config::AppConfig;
    use crate::domain::{
        DeploymentRequest, ExternalImageSource, HealthCheck, Role, RuntimeStartRequest,
        ServiceConfig, ServiceSource,
    };

    const ADMIN_TOKEN: &str = "test-admin-token-0123456789abcdef";

    fn test_state() -> AppState {
        AppState::builder(AppConfig::for_test(ADMIN_TOKEN)).build()
    }

    async fn body_json(resp: axum::response::Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn service_body(project_id: uuid::Uuid) -> ServiceConfig {
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
        .unwrap()
    }

    async fn start_running_replica(state: &AppState, service: &ServiceConfig) -> uuid::Uuid {
        let deployment = state
            .deployments
            .create_deployment(DeploymentRequest::external_image(service.id, "busybox"))
            .unwrap();
        state
            .deployments
            .promote_deployment(service.id, deployment.id)
            .unwrap();
        let artifact = ArtifactRecord::new(
            "sha256:test",
            ArtifactKind::RootfsBundle,
            ArtifactSource::ExternalRegistry {
                image: "busybox".to_string(),
            },
        )
        .unwrap();
        state
            .runtime
            .start(RuntimeStartRequest {
                service_name: service.name.clone(),
                service_id: service.id,
                deployment_id: deployment.id,
                artifact,
                internal_port: 8080,
                socket_path: "/tmp/test.sock".into(),
                cpu_millis: 100,
                memory_bytes: 64 * 1024 * 1024,
                env: Vec::new(),
                pids_max: None,
                memory_swap_max: None,
                io_weight: None,
                replica_index: 0,
            })
            .await
            .unwrap();
        deployment.id
    }

    #[tokio::test]
    async fn operator_can_create_console_ticket_for_running_replica() {
        let state = test_state();
        let project_id = state.projects.default_project_id().unwrap();
        let service = state
            .services
            .put_service(service_body(project_id))
            .unwrap();
        start_running_replica(&state, &service).await;

        let resp = build_router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/services/{}/console/tickets", service.id))
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"replica_index":0}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert!(body["ticket"].as_str().unwrap().len() >= 32);
        assert_eq!(
            body["ws_path"].as_str().unwrap(),
            format!(
                "/v1/services/{}/console/ws?ticket={}",
                service.id,
                body["ticket"].as_str().unwrap()
            )
        );
    }

    #[tokio::test]
    async fn viewer_cannot_create_console_ticket() {
        let state = test_state();
        let project_id = state.projects.default_project_id().unwrap();
        let viewer = state.users.create_user("viewer", "hash", false).unwrap();
        state
            .users
            .set_membership(viewer.id, project_id, Role::Viewer)
            .unwrap();
        let viewer_token = state
            .tokens
            .create_api_token(viewer.id, "viewer")
            .unwrap()
            .token;
        let service = state
            .services
            .put_service(service_body(project_id))
            .unwrap();

        let resp = build_router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/services/{}/console/tickets", service.id))
                    .header("Authorization", format!("Bearer {viewer_token}"))
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"replica_index":0}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn stopped_service_returns_conflict_for_console_ticket() {
        let state = test_state();
        let project_id = state.projects.default_project_id().unwrap();
        let service = state
            .services
            .put_service(service_body(project_id))
            .unwrap();

        let resp = build_router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/services/{}/console/tickets", service.id))
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"replica_index":0}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    fn ticket_value(service_id: uuid::Uuid) -> ConsoleTicket {
        ConsoleTicket {
            service_id,
            service_name: "web".to_string(),
            deployment_id: uuid::Uuid::now_v7(),
            replica_index: 0,
            principal_label: "tester".to_string(),
            cols: 120,
            rows: 32,
            expires_at: chrono::Utc::now() + chrono::TimeDelta::seconds(30),
        }
    }

    #[test]
    fn consume_ticket_rejects_wrong_service() {
        let service_id = uuid::Uuid::now_v7();
        let other_id = uuid::Uuid::now_v7();
        let ticket = format!("unit-test-wrong-service-{}", uuid::Uuid::now_v7());
        TICKETS
            .lock()
            .unwrap()
            .insert(ticket.clone(), ticket_value(service_id));
        let err = consume_ticket(other_id, &ticket).unwrap_err();
        assert!(matches!(err, ApiError::Unauthorized(message) if message.contains("mismatch")));
    }

    #[test]
    fn consume_ticket_is_single_use() {
        let service_id = uuid::Uuid::now_v7();
        let ticket = format!("unit-test-single-use-{}", uuid::Uuid::now_v7());
        TICKETS
            .lock()
            .unwrap()
            .insert(ticket.clone(), ticket_value(service_id));
        assert!(consume_ticket(service_id, &ticket).is_ok());
        assert!(consume_ticket(service_id, &ticket).is_err());
    }

    #[tokio::test]
    async fn console_ws_route_is_registered() {
        // Regression: `public_router()` must be merged into the app router, or
        // the ticket-authenticated websocket upgrade 404s and the browser
        // console reports "console websocket failed". A plain GET is rejected by
        // the upgrade/ticket checks (4xx) but must NOT be 404.
        let state = test_state();
        let service_id = uuid::Uuid::now_v7();
        let resp = build_router(state)
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/v1/services/{service_id}/console/ws"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(resp.status(), StatusCode::NOT_FOUND);
    }
}
