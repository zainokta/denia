use axum::{
    Json, Router,
    extract::State,
    http::header,
    response::{IntoResponse, Response},
    routing::get,
};

use crate::app::{ApiError, AppState};
use crate::auth::{Principal, ensure_super_admin};
use crate::traefik::RouteSpec;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/ingress/routes", get(list_ingress_routes))
        .route("/ingress/config", get(get_ingress_config))
}

async fn list_ingress_routes(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<Vec<RouteSpec>>, ApiError> {
    ensure_super_admin(&principal)?;
    let routes = state
        .routes
        .lock()
        .map_err(|_| ApiError::Conflict("routes lock poisoned".to_string()))?;
    Ok(Json(routes.values().cloned().collect()))
}

async fn get_ingress_config(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Response, ApiError> {
    ensure_super_admin(&principal)?;
    let snapshot: Vec<RouteSpec> = {
        let routes = state
            .routes
            .lock()
            .map_err(|_| ApiError::Conflict("routes lock poisoned".to_string()))?;
        routes.values().cloned().collect()
    };
    let body = crate::traefik::render_file_provider_config(&snapshot, &state.ingress_options)
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    Ok(([(header::CONTENT_TYPE, "text/yaml")], body).into_response())
}
