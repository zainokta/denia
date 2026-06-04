use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use serde::Deserialize;

use crate::api::ApiError;
use crate::app::AppState;
use crate::auth::{Principal, ensure_role};
use crate::domain::{Job, JobRun, Role, ServiceSource};

#[derive(Debug, Deserialize)]
struct CreateJobRequest {
    project_id: uuid::Uuid,
    name: String,
    source: ServiceSource,
    #[serde(default)]
    command: Option<Vec<String>>,
    #[serde(default)]
    env: Vec<(String, String)>,
    #[serde(default)]
    schedule: Option<String>,
    #[serde(default)]
    max_retries: u32,
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/jobs", get(list_jobs).post(create_job))
        .route("/jobs/{job_id}", get(get_job).delete(delete_job))
        .route("/jobs/{job_id}/run", post(run_job))
        .route("/jobs/{job_id}/runs", get(list_job_runs))
}

fn ensure_job_role(
    state: &AppState,
    principal: &Principal,
    project_id: uuid::Uuid,
    role: Role,
) -> Result<(), ApiError> {
    ensure_role(state, principal, project_id, role).map_err(|error| match error {
        ApiError::Forbidden(_) => ApiError::NotFound("job not found".to_string()),
        other => other,
    })
}

async fn list_jobs(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> Result<Json<Vec<Job>>, ApiError> {
    let project_id = params
        .get("project_id")
        .and_then(|id| uuid::Uuid::parse_str(id).ok())
        .ok_or_else(|| ApiError::BadRequest("project_id query parameter is required".into()))?;
    ensure_role(&state, &principal, project_id, Role::Viewer)?;
    let mut jobs = state.jobs.list_jobs(project_id)?;
    if ensure_role(&state, &principal, project_id, Role::Operator).is_err() {
        for job in &mut jobs {
            job.redact_env();
        }
    }
    Ok(Json(jobs))
}

async fn create_job(
    State(state): State<AppState>,
    principal: Principal,
    Json(input): Json<CreateJobRequest>,
) -> Result<(StatusCode, Json<Job>), ApiError> {
    ensure_role(&state, &principal, input.project_id, Role::Operator)?;
    let mut job = Job::new(input.project_id, input.name, input.source, input.schedule)
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    job.command = input.command;
    job.env = input.env;
    job.max_retries = input.max_retries;
    // Prime the cron cursor at creation. `claim_due_jobs` only returns jobs with
    // `next_run_at <= now`, and the only other writer (`set_job_next_run`) runs
    // *after* a job is already due — so without this a scheduled job would never
    // become due (chicken-and-egg). Compute the first fire time from now (ADR-010).
    if job.schedule.is_some() {
        job.next_run_at = crate::scheduler::compute_next_run(&job, chrono::Utc::now());
    }
    let stored = state.jobs.put_job(job)?;
    Ok((StatusCode::CREATED, Json(stored)))
}

async fn get_job(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(job_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<Job>, ApiError> {
    let job = state
        .jobs
        .get_job(job_id)?
        .ok_or_else(|| ApiError::NotFound("job not found".to_string()))?;
    ensure_job_role(&state, &principal, job.project_id, Role::Viewer)?;
    let mut job = job;
    if ensure_role(&state, &principal, job.project_id, Role::Operator).is_err() {
        job.redact_env();
    }
    Ok(Json(job))
}

async fn delete_job(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(job_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let job = state
        .jobs
        .get_job(job_id)?
        .ok_or_else(|| ApiError::NotFound("job not found".to_string()))?;
    ensure_job_role(&state, &principal, job.project_id, Role::Operator)?;
    state.jobs.delete_job(job_id)?;
    Ok(Json(serde_json::json!({"deleted": true})))
}

async fn run_job(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(job_id): axum::extract::Path<uuid::Uuid>,
) -> Result<(StatusCode, Json<JobRun>), ApiError> {
    let job = state
        .jobs
        .get_job(job_id)?
        .ok_or_else(|| ApiError::NotFound("job not found".to_string()))?;
    ensure_job_role(&state, &principal, job.project_id, Role::Operator)?;
    if state.jobs.active_run(job_id)?.is_some() {
        return Err(ApiError::Conflict(
            "job already has an active run".to_string(),
        ));
    }
    let run = state.jobs.create_job_run(job_id)?;
    // Hand the Pending run to the executor (ADR-010). The 202 is returned
    // immediately; the executor drives it to completion out-of-band. Without a
    // wired executor (tests, or before boot completes) the run stays Pending
    // and is reconciled to Failed by `fail_orphan_runs` on the next restart.
    if let Some(sender) = &state.job_enqueue {
        if sender.send(run.clone()).is_err() {
            tracing::error!(job_id = %job_id, run_id = %run.id, "job executor channel closed; run not enqueued");
        }
    }
    Ok((StatusCode::ACCEPTED, Json(run)))
}

async fn list_job_runs(
    State(state): State<AppState>,
    principal: Principal,
    axum::extract::Path(job_id): axum::extract::Path<uuid::Uuid>,
) -> Result<Json<Vec<JobRun>>, ApiError> {
    let job = state
        .jobs
        .get_job(job_id)?
        .ok_or_else(|| ApiError::NotFound("job not found".to_string()))?;
    ensure_job_role(&state, &principal, job.project_id, Role::Viewer)?;
    Ok(Json(state.jobs.list_job_runs(job_id)?))
}

#[cfg(test)]
mod tests {
    use crate::app::{AppState, build_router};
    use crate::config::AppConfig;
    use crate::domain::{REDACTED_ENV_VALUE, Role};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

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

    fn job_body(project_id: uuid::Uuid, supplied_id: uuid::Uuid) -> serde_json::Value {
        serde_json::json!({
            "id": supplied_id,
            "project_id": project_id,
            "name": "nightly",
            "source": {
                "type": "external_image",
                "image": "busybox",
                "credential": null
            },
            "command": ["/bin/true"],
            "env": [["TOKEN", "secret-value"]],
            "schedule": null,
            "max_retries": 2,
            "next_run_at": "2000-01-01T00:00:00Z",
            "last_enqueued_at": "2000-01-01T00:00:00Z",
            "created_at": "2000-01-01T00:00:00Z"
        })
    }

    #[tokio::test]
    async fn create_job_mints_server_id_and_drops_client_state() {
        let state = test_state();
        let project_id = state.projects.default_project_id().unwrap();
        let supplied_id = uuid::Uuid::now_v7();

        let resp = build_router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/jobs")
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&job_body(project_id, supplied_id)).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::CREATED);
        let created = body_json(resp).await;
        assert_ne!(
            created["id"].as_str(),
            Some(supplied_id.to_string().as_str())
        );
        assert_eq!(created["next_run_at"], serde_json::Value::Null);
        assert_eq!(created["last_enqueued_at"], serde_json::Value::Null);
        assert_ne!(created["created_at"].as_str(), Some("2000-01-01T00:00:00Z"));
    }

    #[tokio::test]
    async fn viewer_get_job_receives_redacted_env() {
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

        let app = build_router(state);
        let create = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/jobs")
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&job_body(project_id, uuid::Uuid::now_v7())).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create.status(), StatusCode::CREATED);
        let created = body_json(create).await;
        let job_id = created["id"].as_str().unwrap();

        let get = app
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/jobs/{job_id}"))
                    .header("Authorization", format!("Bearer {viewer_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(get.status(), StatusCode::OK);
        let fetched = body_json(get).await;
        assert_eq!(fetched["env"][0][0], "TOKEN");
        assert_eq!(fetched["env"][0][1], REDACTED_ENV_VALUE);
    }

    #[tokio::test]
    async fn get_job_in_foreign_project_returns_404() {
        let state = test_state();
        let project_id = state.projects.default_project_id().unwrap();
        let stranger = state.users.create_user("stranger", "hash", false).unwrap();
        let stranger_token = state
            .tokens
            .create_api_token(stranger.id, "stranger")
            .unwrap()
            .token;

        let create = build_router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/jobs")
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&job_body(project_id, uuid::Uuid::now_v7())).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create.status(), StatusCode::CREATED);
        let created = body_json(create).await;
        let job_id = created["id"].as_str().unwrap();

        let get = build_router(state)
            .oneshot(
                Request::builder()
                    .uri(format!("/v1/jobs/{job_id}"))
                    .header("Authorization", format!("Bearer {stranger_token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(get.status(), StatusCode::NOT_FOUND);
    }
}
