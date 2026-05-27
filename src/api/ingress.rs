use axum::{Json, Router, extract::State, routing::get};

use crate::api::ApiError;
use crate::app::AppState;
use crate::auth::{Principal, ensure_super_admin};
use crate::ingress::pingora::RouteSpec;

pub fn router() -> Router<AppState> {
    Router::new().route("/ingress/routes", get(list_ingress_routes))
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
