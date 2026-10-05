//! Operator prune endpoints behind `denia clean` (ADR-040). Super-admin only.
//!
//! `GET /v1/system/prune` plans (deletes nothing); `POST /v1/system/prune`
//! executes and reports what was actually reclaimed.

use axum::{Json, Router, extract::State, routing::get};

use crate::api::ApiError;
use crate::app::AppState;
use crate::auth::{Principal, ensure_super_admin};
use crate::prune::{PruneError, PruneReport};

pub fn router() -> Router<AppState> {
    Router::new().route("/system/prune", get(plan_prune).post(execute_prune))
}

async fn plan_prune(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<PruneReport>, ApiError> {
    ensure_super_admin(&principal)?;
    run(&state, false).await
}

async fn execute_prune(
    State(state): State<AppState>,
    principal: Principal,
) -> Result<Json<PruneReport>, ApiError> {
    ensure_super_admin(&principal)?;
    run(&state, true).await
}

async fn run(state: &AppState, execute: bool) -> Result<Json<PruneReport>, ApiError> {
    crate::prune::run(state, execute)
        .await
        .map(Json)
        .map_err(|e: PruneError| ApiError::Conflict(e.to_string()))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    use crate::app::{AppState, build_router};
    use crate::artifacts::{ArtifactKind, ArtifactRecord, ArtifactSource};
    use crate::command::{CommandOutput, FakeCommandRunner};
    use crate::config::AppConfig;
    use crate::domain::{
        DeploymentRequest, ExternalImageSource, HealthCheck, ResourceLimits, ServiceConfig,
        ServiceSource,
    };

    const ADMIN_TOKEN: &str = "test-admin-token-0123456789abcdef";
    const KEPT: &str = "sha256:aaaa";
    const STALE: &str = "sha256:bbbb";

    fn buildctl(stdout: &str) -> CommandOutput {
        CommandOutput {
            status: 0,
            stdout: stdout.to_string(),
            stderr: String::new(),
        }
    }

    /// Bundle dir with its marker files backdated past the grace window.
    fn old_bundle(artifact_dir: &std::path::Path, digest: &str) -> std::path::PathBuf {
        let dir = artifact_dir.join(digest.replace(':', "-"));
        std::fs::create_dir_all(dir.join("rootfs")).unwrap();
        std::fs::write(dir.join("rootfs/app"), vec![0u8; 8192]).unwrap();
        std::fs::write(dir.join("layers.json"), "[]").unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3 * 3600);
        for p in [dir.join("layers.json"), dir.join("rootfs"), dir.clone()] {
            std::fs::File::open(&p).unwrap().set_modified(old).unwrap();
        }
        dir
    }

    /// State with a service whose promoted deployment uses `KEPT`, plus an
    /// unreferenced `STALE` bundle on disk.
    fn state_with_bundles(
        root: &std::path::Path,
        runner_outputs: Vec<CommandOutput>,
    ) -> (AppState, std::path::PathBuf, std::path::PathBuf) {
        let mut config = AppConfig::for_test(ADMIN_TOKEN);
        config.data_dir = root.to_path_buf();
        config.artifact_dir = root.join("artifacts");
        config.log_dir = root.join("logs");
        config.uploads_dir = root.join("uploads");
        let kept = old_bundle(&config.artifact_dir, KEPT);
        let stale = old_bundle(&config.artifact_dir, STALE);
        let state = AppState::builder(config)
            .command_runner(Arc::new(FakeCommandRunner::new(runner_outputs)))
            .build();

        let project_id = state.projects.default_project_id().unwrap();
        let service = state
            .services
            .put_service(
                ServiceConfig::new(
                    project_id,
                    "web",
                    vec![],
                    ServiceSource::ExternalImage(ExternalImageSource {
                        image: "ghcr.io/acme/web:latest".to_string(),
                        credential: None,
                        registry_id: None,
                        image_ref: None,
                    }),
                    3000,
                    HealthCheck::new("/ready", 5),
                    Some(ResourceLimits::default()),
                    vec![],
                )
                .unwrap(),
            )
            .unwrap();
        let deployment = state
            .deployments
            .create_deployment(DeploymentRequest::external_image(
                service.id,
                "ghcr.io/acme/web:latest",
            ))
            .unwrap();
        state
            .deployments
            .put_artifact(
                ArtifactRecord::new(
                    KEPT,
                    ArtifactKind::RootfsBundle,
                    ArtifactSource::ExternalRegistry {
                        image: "ghcr.io/acme/web:latest".to_string(),
                    },
                )
                .unwrap(),
            )
            .unwrap();
        state
            .deployments
            .set_deployment_artifact(deployment.id, KEPT)
            .unwrap();
        state
            .deployments
            .promote_deployment(service.id, deployment.id)
            .unwrap();
        (state, kept, stale)
    }

    async fn call(
        state: AppState,
        method: &str,
        token: Option<&str>,
    ) -> (StatusCode, serde_json::Value) {
        let mut req = Request::builder().method(method).uri("/v1/system/prune");
        if let Some(token) = token {
            req = req.header("Authorization", format!("Bearer {token}"));
        }
        let resp = build_router(state)
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    fn category<'a>(body: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
        body["categories"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["category"] == name)
            .unwrap_or_else(|| panic!("missing category {name} in {body}"))
    }

    #[tokio::test]
    async fn plan_reports_stale_bundle_without_deleting() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, kept, stale) = state_with_bundles(
            tmp.path(),
            vec![buildctl(r#"[{"size":1000,"inUse":false}]"#)],
        );

        let (status, body) = call(state, "GET", Some(ADMIN_TOKEN)).await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["executed"], false);
        let bundles = category(&body, "rootfs_bundles");
        assert_eq!(bundles["entries"], 1);
        assert!(bundles["bytes"].as_u64().unwrap() >= 8192);
        assert_eq!(category(&body, "buildkit_cache")["bytes"], 1000);
        assert!(stale.exists() && kept.exists());
    }

    #[tokio::test]
    async fn execute_deletes_stale_bundle_and_keeps_promoted() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, kept, stale) =
            state_with_bundles(tmp.path(), vec![buildctl("{\"size\":40}\n")]);

        let (status, body) = call(state, "POST", Some(ADMIN_TOKEN)).await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["executed"], true);
        assert_eq!(category(&body, "rootfs_bundles")["entries"], 1);
        assert_eq!(category(&body, "buildkit_cache")["bytes"], 40);
        assert!(!stale.exists());
        assert!(kept.exists());
    }

    #[tokio::test]
    async fn buildkit_failure_does_not_block_other_categories() {
        let tmp = tempfile::tempdir().unwrap();
        // No queued output: the fake runner errors like an unreachable buildkitd.
        let (state, _kept, stale) = state_with_bundles(tmp.path(), vec![]);

        let (status, body) = call(state, "POST", Some(ADMIN_TOKEN)).await;

        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(category(&body, "buildkit_cache")["error"].is_string());
        assert!(!stale.exists());
    }

    #[tokio::test]
    async fn prune_requires_auth() {
        let tmp = tempfile::tempdir().unwrap();
        let (state, _kept, stale) = state_with_bundles(tmp.path(), vec![]);

        let (status, _) = call(state, "POST", None).await;

        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert!(stale.exists());
    }
}
