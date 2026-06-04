use axum::{Json, Router, extract::State, routing::get};
use serde::Serialize;

use crate::access_log::AccessEntry;
use crate::api::ApiError;
use crate::app::AppState;
use crate::auth::{Principal, ensure_role, ensure_super_admin};
use crate::domain::Role;
use crate::metrics::CgroupMetricsReader;
use crate::node_metrics::{NodeMetricsReader, NodeSnapshot};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/metrics/node", get(get_node_metrics))
        .route("/workloads", get(list_workloads))
        .route(
            "/services/{service_id}/requests",
            get(list_service_requests),
        )
}

#[derive(Debug, Serialize)]
struct WorkloadView {
    service_id: uuid::Uuid,
    service_name: String,
    project_id: uuid::Uuid,
    deployment_id: Option<uuid::Uuid>,
    status: Option<crate::domain::DeploymentStatus>,
    cpu_usage_usec: Option<u64>,
    memory_current_bytes: Option<u64>,
    replica_count: u32,
    healthy_replicas: u32,
}

async fn get_node_metrics(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<NodeSnapshot>, ApiError> {
    ensure_super_admin(&principal)?;
    let reader = NodeMetricsReader::new(state.config.node_disk_path.clone());
    // procfs + statvfs reads are blocking; run them under `block_in_place` so
    // they do not stall the async executor thread, matching the SSE log path.
    // See review 07 (LOW: blocking fs on async handlers).
    let snapshot = tokio::task::block_in_place(|| reader.read())?;
    Ok(Json(snapshot))
}

async fn list_workloads(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<Vec<WorkloadView>>, ApiError> {
    let services = state.services.list_services()?;
    let allowed = if principal.is_super_admin {
        None
    } else {
        let user_id = principal
            .user_id
            .ok_or_else(|| ApiError::Forbidden("authenticated user required".to_string()))?;
        let memberships = state.users.list_memberships_for_user(user_id)?;
        Some(
            memberships
                .into_iter()
                .map(|m| m.project_id)
                .collect::<std::collections::HashSet<_>>(),
        )
    };
    let reader = CgroupMetricsReader::new(state.config.cgroup_root.clone());
    let mut workloads = Vec::new();
    for service in services {
        if let Some(ref a) = allowed
            && !a.contains(&service.project_id)
        {
            continue;
        }
        let deployment_id = state.deployments.promoted_deployment(service.id)?;
        let (cpu, mem) = match deployment_id {
            // cgroup v2 read is blocking fs I/O; keep it off the async executor
            // thread via `block_in_place` (review 07 LOW). Shape unchanged.
            Some(d) => match tokio::task::block_in_place(|| {
                reader.read_by_id(&service.name, service.id, d)
            }) {
                Ok(snap) => (Some(snap.cpu_usage_usec), Some(snap.memory_current_bytes)),
                Err(_) => (None, None),
            },
            None => (None, None),
        };
        let status = match deployment_id {
            Some(d) => state
                .deployments
                .list_deployments(service.id)?
                .into_iter()
                .find(|dep| dep.id == d)
                .map(|dep| dep.status),
            None => None,
        };
        let (replica_count, healthy_replicas) = match &state.autoscaler {
            Some(ctrl) => {
                let c = ctrl.lock().await;
                (c.replica_count(service.id), c.healthy_replicas(service.id))
            }
            None => (0, 0),
        };
        workloads.push(WorkloadView {
            service_id: service.id,
            service_name: service.name.clone(),
            project_id: service.project_id,
            deployment_id,
            status,
            cpu_usage_usec: cpu,
            memory_current_bytes: mem,
            replica_count,
            healthy_replicas,
        });
    }
    Ok(Json(workloads))
}

async fn list_service_requests(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(service_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<Vec<AccessEntry>>, ApiError> {
    let Some(service) = state.services.get_service(service_id)? else {
        return Err(ApiError::NotFound("service not found".to_string()));
    };
    ensure_role(&state, &principal, service.project_id, Role::Operator)?;
    // Access logs are keyed by service_id (the bridge identity), not name (F-3).
    Ok(Json(state.access_log.recent(&service.id.to_string())))
}
