use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};

use crate::api::ApiError;
use crate::app::AppState;
use crate::auth::{Principal, ensure_role};
use crate::domain::{Job, JobRun, Role};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/jobs", get(list_jobs).post(create_job))
        .route("/jobs/{job_id}", get(get_job).delete(delete_job))
        .route("/jobs/{job_id}/run", post(run_job))
        .route("/jobs/{job_id}/runs", get(list_job_runs))
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
    Ok(Json(state.jobs.list_jobs(project_id)?))
}

async fn create_job(
    State(state): State<AppState>,
    principal: Principal,
    Json(job): Json<Job>,
) -> Result<(StatusCode, Json<Job>), ApiError> {
    ensure_role(&state, &principal, job.project_id, Role::Operator)?;
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
    ensure_role(&state, &principal, job.project_id, Role::Viewer)?;
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
    ensure_role(&state, &principal, job.project_id, Role::Operator)?;
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
    ensure_role(&state, &principal, job.project_id, Role::Operator)?;
    if state.jobs.active_run(job_id)?.is_some() {
        return Err(ApiError::Conflict(
            "job already has an active run".to_string(),
        ));
    }
    let run = state.jobs.create_job_run(job_id)?;
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
    ensure_role(&state, &principal, job.project_id, Role::Viewer)?;
    Ok(Json(state.jobs.list_job_runs(job_id)?))
}
