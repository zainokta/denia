//! OCI layer cache observability + manual GC (ADR-021). Super-admin gated.

use axum::{Json, Router, extract::State, routing::get, routing::post};
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::api::ApiError;
use crate::app::AppState;
use crate::auth::{Principal, ensure_super_admin};
use crate::oci::cache::{CacheError, CacheStatus, GcStatus, LayerCache, LayerCacheGc};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/oci/cache", get(get_cache_status))
        .route("/oci/cache/gc", post(post_cache_gc))
}

/// Response shape for `GET /v1/oci/cache` and `POST /v1/oci/cache/gc`.
#[derive(Debug, Serialize)]
pub struct CacheStatusView {
    pub entries: u64,
    pub total_bytes: u64,
    pub oldest_entry_age_secs: Option<u64>,
    pub last_gc_at: Option<DateTime<Utc>>,
    pub last_gc_deleted_bytes: u64,
    pub last_gc_deleted_entries: u64,
}

/// Additional fields surfaced by the synchronous `POST /v1/oci/cache/gc`.
#[derive(Debug, Serialize)]
pub struct CacheGcRunView {
    #[serde(flatten)]
    pub status: CacheStatusView,
    pub deleted_entries: u64,
    pub deleted_bytes: u64,
    pub scanned_entries: u64,
    pub kept_in_use_entries: u64,
    pub kept_recent_entries: u64,
}

fn merge_status(cache: &CacheStatus, gc: &GcStatus) -> CacheStatusView {
    CacheStatusView {
        entries: cache.entries,
        total_bytes: cache.total_bytes,
        oldest_entry_age_secs: cache.oldest_entry_age_secs,
        last_gc_at: gc.last_gc_at,
        last_gc_deleted_bytes: gc.last_gc_deleted_bytes,
        last_gc_deleted_entries: gc.last_gc_deleted_entries,
    }
}

fn cache_error_to_api(e: CacheError) -> ApiError {
    // Don't leak filesystem internals to non-admin paths — but this endpoint
    // is already super-admin gated, so the error message is safe.
    ApiError::Conflict(format!("oci cache error: {e}"))
}

fn require_cache(state: &AppState) -> Result<(LayerCache, Option<LayerCacheGc>), ApiError> {
    let Some(cache) = state.oci_cache.clone() else {
        return Err(ApiError::NotFound(
            "oci layer cache is not configured".to_string(),
        ));
    };
    Ok((cache, state.oci_cache_gc.clone()))
}

async fn get_cache_status(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<CacheStatusView>, ApiError> {
    ensure_super_admin(&principal)?;
    let (cache, gc) = require_cache(&state)?;
    let cache_status = cache.status().map_err(cache_error_to_api)?;
    let gc_status = gc.as_ref().map(|g| g.status()).unwrap_or_default();
    Ok(Json(merge_status(&cache_status, &gc_status)))
}

async fn post_cache_gc(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<CacheGcRunView>, ApiError> {
    ensure_super_admin(&principal)?;
    let (cache, gc) = require_cache(&state)?;
    let Some(gc) = gc else {
        return Err(ApiError::NotFound(
            "oci cache gc is not configured".to_string(),
        ));
    };
    // Sweep is blocking I/O; run on the blocking pool.
    let report = tokio::task::spawn_blocking(move || gc.sweep_once())
        .await
        .map_err(|e| ApiError::Conflict(format!("gc join error: {e}")))?
        .map_err(cache_error_to_api)?;
    let cache_status = cache.status().map_err(cache_error_to_api)?;
    let gc_status = GcStatus {
        last_gc_at: report.ran_at,
        last_gc_deleted_bytes: report.deleted_bytes,
        last_gc_deleted_entries: report.deleted_entries,
    };
    Ok(Json(CacheGcRunView {
        status: merge_status(&cache_status, &gc_status),
        deleted_entries: report.deleted_entries,
        deleted_bytes: report.deleted_bytes,
        scanned_entries: report.scanned_entries,
        kept_in_use_entries: report.kept_in_use_entries,
        kept_recent_entries: report.kept_recent_entries,
    }))
}
