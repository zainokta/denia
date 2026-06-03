use axum::{Json, Router, extract::State, routing::get};
use serde::Serialize;

use crate::app::AppState;
use crate::auth::Principal;

#[derive(Serialize)]
struct NodeInfo {
    control_domain: Option<String>,
}

pub fn router() -> Router<AppState> {
    Router::new().route("/node", get(node_info))
}

async fn node_info(State(state): State<AppState>, _principal: Principal) -> Json<NodeInfo> {
    Json(NodeInfo {
        control_domain: state.config.control_domain.clone(),
    })
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use crate::app::{AppState, build_router};
    use crate::config::AppConfig;

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

    #[tokio::test]
    async fn node_info_returns_control_domain() {
        let mut config = AppConfig::for_test(ADMIN_TOKEN);
        config.control_domain = Some("denia.example.com".to_string());
        let state = AppState::builder(config).build();

        let resp = build_router(state)
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/node")
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert_eq!(body["control_domain"], "denia.example.com");
    }

    #[tokio::test]
    async fn node_info_null_when_unset() {
        let state = test_state();

        let resp = build_router(state)
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/node")
                    .header("Authorization", format!("Bearer {ADMIN_TOKEN}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        assert!(body["control_domain"].is_null());
    }

    #[tokio::test]
    async fn node_info_requires_auth() {
        let state = test_state();

        let resp = build_router(state)
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/node")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
