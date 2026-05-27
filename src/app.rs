use axum::{
    Router,
    extract::Request,
    http::header,
    middleware::{self, Next},
    response::Response,
    routing::get,
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::{
    access_log::AccessLogStore,
    api,
    auth::require_auth,
    bridge::{BridgeAllocator, BridgeManager, LoopbackBridgeSupervisor},
    command::{CommandRunner, TokioCommandRunner},
    config::AppConfig,
    deploy::{DeploymentRepos, SharedRoutes},
    health::{FakeHealthChecker, HealthChecker},
    rate_limit::{LoginRateLimiter, rate_limit_login},
    repo::sqlite::{
        SqliteCredentialRepo, SqliteDeploymentRepo, SqliteDomainRepo, SqliteJobRepo,
        SqliteProjectRepo, SqliteRegistryRepo, SqliteServiceRepo, SqliteTokenRepo, SqliteUserRepo,
    },
    runtime::{LinuxRuntime, Runtime},
    state::SqliteStore,
    traefik::IngressRenderOptions,
};

#[derive(Clone)]
pub struct AppState {
    pub config: AppConfig,
    pub services: SqliteServiceRepo,
    pub domains: SqliteDomainRepo,
    pub registries: SqliteRegistryRepo,
    pub projects: SqliteProjectRepo,
    pub users: SqliteUserRepo,
    pub deployments: SqliteDeploymentRepo,
    pub jobs: SqliteJobRepo,
    pub tokens: SqliteTokenRepo,
    pub credentials: SqliteCredentialRepo,
    pub(crate) runtime: Arc<dyn Runtime>,
    pub(crate) health: Arc<dyn HealthChecker>,
    pub(crate) command_runner: Arc<dyn CommandRunner>,
    pub(crate) bridge_allocator: Arc<Mutex<BridgeAllocator>>,
    pub(crate) bridge_manager: Arc<dyn BridgeManager>,
    pub routes: SharedRoutes,
    pub ingress_options: IngressRenderOptions,
    pub access_log: AccessLogStore,
    pub domain_verifier: Arc<dyn crate::verification::DomainVerifier>,
    pub verifying_domains: Arc<Mutex<std::collections::HashSet<uuid::Uuid>>>,
}

impl AppState {
    pub fn new(config: AppConfig, store: &SqliteStore) -> Self {
        let bridge_start_port = config.bridge_start_port;
        let runtime = Arc::new(
            LinuxRuntime::new_with_paths(
                config.runtime_dir.clone(),
                config.artifact_dir.clone(),
                config.cgroup_root.clone(),
            )
            .with_userns(config.userns_base, config.userns_size)
            .with_socket_proxy(config.socket_proxy_binary.clone())
            .with_log_dir(config.log_dir.clone()),
        );
        let access_log = AccessLogStore::new();
        let supervisor = LoopbackBridgeSupervisor::with_access_log(access_log.clone());
        Self::new_with_deploy_dependencies_and_log(
            config,
            store,
            runtime,
            FakeHealthChecker::healthy(),
            TokioCommandRunner,
            BridgeAllocator::new(bridge_start_port),
            supervisor,
            access_log,
        )
    }

    pub fn new_with_deploy_dependencies<R, H, C, B, M>(
        config: AppConfig,
        store: &SqliteStore,
        runtime: R,
        health: H,
        command_runner: C,
        bridge_allocator: B,
        bridge_manager: M,
    ) -> Self
    where
        R: Runtime + 'static,
        H: HealthChecker + 'static,
        C: CommandRunner + 'static,
        B: Into<BridgeAllocator>,
        M: BridgeManager + 'static,
    {
        Self::new_with_deploy_dependencies_and_log(
            config,
            store,
            runtime,
            health,
            command_runner,
            bridge_allocator,
            bridge_manager,
            AccessLogStore::new(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_deploy_dependencies_and_log<R, H, C, B, M>(
        config: AppConfig,
        store: &SqliteStore,
        runtime: R,
        health: H,
        command_runner: C,
        bridge_allocator: B,
        bridge_manager: M,
        access_log: AccessLogStore,
    ) -> Self
    where
        R: Runtime + 'static,
        H: HealthChecker + 'static,
        C: CommandRunner + 'static,
        B: Into<BridgeAllocator>,
        M: BridgeManager + 'static,
    {
        let ingress_options = IngressRenderOptions {
            acme_resolver: config.acme_resolver.clone(),
            control_domain: config.control_domain.clone(),
            control_tls: config.control_tls,
            control_backend_addr: format!("http://{}", config.bind_addr),
        };
        let pool = store.pool();
        Self {
            config,
            services: SqliteServiceRepo::new(pool.clone()),
            domains: SqliteDomainRepo::new(pool.clone()),
            registries: SqliteRegistryRepo::new(pool.clone()),
            projects: SqliteProjectRepo::new(pool.clone()),
            users: SqliteUserRepo::new(pool.clone()),
            deployments: SqliteDeploymentRepo::new(pool.clone()),
            jobs: SqliteJobRepo::new(pool.clone()),
            tokens: SqliteTokenRepo::new(pool.clone()),
            credentials: SqliteCredentialRepo::new(pool),
            runtime: Arc::new(runtime),
            health: Arc::new(health),
            command_runner: Arc::new(command_runner),
            bridge_allocator: Arc::new(Mutex::new(bridge_allocator.into())),
            bridge_manager: Arc::new(bridge_manager),
            routes: Arc::new(Mutex::new(BTreeMap::new())),
            ingress_options,
            access_log,
            domain_verifier: Arc::new(crate::verification::HttpDomainVerifier::new()),
            verifying_domains: Arc::new(Mutex::new(std::collections::HashSet::new())),
        }
    }

    pub fn with_domain_verifier(
        mut self,
        verifier: Arc<dyn crate::verification::DomainVerifier>,
    ) -> Self {
        self.domain_verifier = verifier;
        self
    }

    /// Build a `DeploymentRepos` bundle from this state for handler-side
    /// coordinator construction.
    pub(crate) fn deployment_repos(&self) -> DeploymentRepos {
        DeploymentRepos {
            deployments: self.deployments.clone(),
            projects: self.projects.clone(),
            registries: self.registries.clone(),
            domains: self.domains.clone(),
        }
    }
}

/// Test-support builder for `AppState`. Lets tests inject arbitrary
/// `Arc<dyn ...Repo>` mocks plus a runtime, defaulting every other field to a
/// fake/no-op implementation. Gated to `cfg(test)` and the `test-support`
/// feature so it never reaches the release binary.
#[cfg(any(test, feature = "test-support"))]
pub struct AppStateBuilder {
    config: AppConfig,
    runtime: Option<Arc<dyn Runtime>>,
    domain_verifier: Option<Arc<dyn crate::verification::DomainVerifier>>,
}

#[cfg(any(test, feature = "test-support"))]
impl AppStateBuilder {
    pub fn new(config: AppConfig) -> Self {
        Self {
            config,
            runtime: None,
            domain_verifier: None,
        }
    }

    pub fn runtime(mut self, runtime: Arc<dyn Runtime>) -> Self {
        self.runtime = Some(runtime);
        self
    }
    pub fn domain_verifier(
        mut self,
        verifier: Arc<dyn crate::verification::DomainVerifier>,
    ) -> Self {
        self.domain_verifier = Some(verifier);
        self
    }

    /// Build an `AppState` backed by an in-memory migrated SQLite store, with
    /// fake/no-op infra dependencies. Used by handler unit tests.
    pub fn build(self) -> AppState {
        let store = SqliteStore::open_in_memory().expect("open in-memory store");
        store.migrate().expect("run migrations");
        let pool = store.pool();
        let ingress_options = IngressRenderOptions {
            acme_resolver: self.config.acme_resolver.clone(),
            control_domain: self.config.control_domain.clone(),
            control_tls: self.config.control_tls,
            control_backend_addr: format!("http://{}", self.config.bind_addr),
        };
        let bridge_start_port = self.config.bridge_start_port;
        AppState {
            config: self.config,
            services: SqliteServiceRepo::new(pool.clone()),
            domains: SqliteDomainRepo::new(pool.clone()),
            registries: SqliteRegistryRepo::new(pool.clone()),
            projects: SqliteProjectRepo::new(pool.clone()),
            users: SqliteUserRepo::new(pool.clone()),
            deployments: SqliteDeploymentRepo::new(pool.clone()),
            jobs: SqliteJobRepo::new(pool.clone()),
            tokens: SqliteTokenRepo::new(pool.clone()),
            credentials: SqliteCredentialRepo::new(pool),
            runtime: self
                .runtime
                .unwrap_or_else(|| Arc::new(crate::runtime::FakeRuntime::default())),
            health: Arc::new(FakeHealthChecker::healthy()),
            command_runner: Arc::new(TokioCommandRunner),
            bridge_allocator: Arc::new(Mutex::new(BridgeAllocator::new(bridge_start_port))),
            bridge_manager: Arc::new(crate::bridge::FakeBridgeManager::default()),
            routes: Arc::new(Mutex::new(BTreeMap::new())),
            ingress_options,
            access_log: AccessLogStore::new(),
            domain_verifier: self
                .domain_verifier
                .unwrap_or_else(|| Arc::new(crate::verification::HttpDomainVerifier::new())),
            verifying_domains: Arc::new(Mutex::new(std::collections::HashSet::new())),
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl AppState {
    /// Entry point for the test-support builder.
    pub fn builder(config: AppConfig) -> AppStateBuilder {
        AppStateBuilder::new(config)
    }
}

pub fn build_router(state: AppState) -> Router {
    let rate_limiter = LoginRateLimiter::default();
    let auth_public = api::auth::public_router().route_layer(middleware::from_fn_with_state(
        rate_limiter,
        rate_limit_login,
    ));

    let authed = api::auth::router()
        .merge(api::users::router())
        .merge(api::tokens::router())
        .merge(api::jobs::router())
        .merge(api::credentials::router())
        .merge(api::services::router())
        .merge(api::deployments::router())
        .merge(api::domains::router())
        .merge(api::projects::router())
        .merge(api::members::router())
        .merge(api::registries::router())
        .merge(api::observability::router())
        .merge(api::ingress::router())
        .route_layer(middleware::from_fn_with_state(state.clone(), require_auth));

    Router::new()
        .route("/healthz", get(api::health::healthz))
        .route(
            "/.well-known/denia-challenge/{token}",
            get(api::domains::challenge_handler),
        )
        .nest("/v1", auth_public.merge(authed))
        .layer(middleware::from_fn(security_headers))
        .fallback(crate::web::static_handler)
        .with_state(state)
}

async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::X_FRAME_OPTIONS,
        header::HeaderValue::from_static("DENY"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        header::HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    response
}
