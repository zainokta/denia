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
    command::{CommandRunner, TokioCommandRunner},
    config::AppConfig,
    deploy::{DeploymentRepos, SharedRoutes},
    health::{HealthChecker, SocketHealthChecker},
    ingress::pingora::IngressState,
    oci::cache::{LayerCache, LayerCacheGc},
    rate_limit::{AdminRateLimiter, LoginRateLimiter, rate_limit_admin, rate_limit_login},
    repo::sqlite::{
        SqliteCredentialRepo, SqliteDeploymentRepo, SqliteDomainRepo, SqliteJobRepo,
        SqliteProjectRepo, SqliteRegistryRepo, SqliteServiceRepo, SqliteTokenRepo, SqliteUserRepo,
    },
    runtime::{LinuxRuntime, Runtime},
    state::SqliteStore,
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
    pub registry: crate::registry::repo::HostedRegistryRepo,
    pub registry_storage: crate::registry::storage::RegistryStorage,
    /// Conservative hosted-registry garbage collector (ADR-031). Cloneable:
    /// shared status state between the periodic loop and the management
    /// endpoint.
    pub registry_gc: crate::registry::gc::RegistryGc,
    pub(crate) runtime: Arc<dyn Runtime>,
    pub(crate) health: Arc<dyn HealthChecker>,
    pub(crate) command_runner: Arc<dyn CommandRunner>,
    /// Shared in-process ingress control brain (replica pools, health, route
    /// table, cert store). Cloned into the Pingora proxy in `main` (Chunk C).
    pub ingress: Arc<IngressState>,
    pub routes: SharedRoutes,
    pub access_log: AccessLogStore,
    pub domain_verifier: Arc<dyn crate::verification::DomainVerifier>,
    pub verifying_domains: Arc<Mutex<std::collections::HashSet<uuid::Uuid>>>,
    /// Shared ACME HTTP-01 challenge map (token -> key authorization), served by
    /// the `/.well-known/acme-challenge/{token}` handler. Cloned from the
    /// in-process ACME driver in `main` (Chunk C); defaults to an empty store.
    pub acme_challenges: crate::ingress::pingora::acme::ChallengeStore,
    pub(crate) autoscaler:
        Option<Arc<tokio::sync::Mutex<crate::autoscale::controller::Controller>>>,
    /// Persistent OCI layer cache (ADR-022). `None` in test builds that
    /// construct `AppState` via the test builder without a cache wired up.
    pub oci_cache: Option<LayerCache>,
    /// Garbage collector handle used by both the background loop and the
    /// `POST /v1/oci/cache/gc` endpoint. Cloneable: shared status state.
    pub oci_cache_gc: Option<LayerCacheGc>,
    /// Sender onto the job executor's run channel. The daemon injects this after
    /// building the scheduler so `POST /v1/jobs/{id}/run` can enqueue a manual
    /// run for execution (ADR-010). `None` in tests / contexts without a running
    /// executor — the API still persists a Pending run and returns 202.
    pub job_enqueue: Option<tokio::sync::mpsc::UnboundedSender<crate::domain::JobRun>>,
}

impl AppState {
    pub fn new(config: AppConfig, store: &SqliteStore) -> Self {
        use crate::autoscale::catalog::RepoServiceCatalog;
        use crate::autoscale::controller::{CgroupUsageSource, Controller};
        use crate::autoscale::ledger::{Headroom, HostCapacity, ResourceLedger};
        use crate::autoscale::registry::ReplicaRegistry;
        use crate::observability::metrics::CgroupMetricsReader;

        let cgroup_root = config.cgroup_root.clone();
        let headroom = Headroom {
            cpu_millis: config.autoscale_headroom_cpu_millis,
            mem_bytes: config.autoscale_headroom_mem_bytes,
        };

        let runtime: Arc<dyn Runtime> = Arc::new(
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
        let ingress = Arc::new(IngressState::with_access_log(access_log.clone()));
        let health: Arc<dyn HealthChecker> = Arc::new(SocketHealthChecker::new());

        let mut state = Self::new_with_deploy_dependencies_and_log(
            config,
            store,
            runtime.clone(),
            health.clone(),
            TokioCommandRunner,
            ingress.clone(),
            access_log,
        );

        let ledger = ResourceLedger::new(HostCapacity::detect(), headroom);
        let usage = Box::new(CgroupUsageSource::new(CgroupMetricsReader::new(
            cgroup_root,
        )));
        let catalog = Arc::new(RepoServiceCatalog::new(
            state.services.clone(),
            state.projects.clone(),
            state.deployments.clone(),
        ));
        let controller = Controller::new(
            ReplicaRegistry::default(),
            ledger,
            runtime,
            ingress.clone(),
            health,
            store.clone(),
            usage,
            catalog,
            std::time::Duration::from_secs(30),
            crate::observability::logs::LogStore::new(&state.config.log_dir),
        );
        state.autoscaler = Some(Arc::new(tokio::sync::Mutex::new(controller)));

        // OCI layer cache + GC (ADR-022). Init failure must not kill the
        // control plane — fall back to a cache-less puller path. Operators
        // can tail `eprintln!` output to see why.
        match LayerCache::new(
            state.config.oci_cache_dir.clone(),
            state.config.oci_cache_verify_on_hit,
        ) {
            Ok(cache) => {
                let deployed =
                    std::sync::Arc::new(crate::oci::cache::deployed::SqliteDeployedDigests::new(
                        state.services.clone(),
                        state.deployments.clone(),
                        state.config.artifact_dir.clone(),
                    ));
                let allowed = vec![
                    state.config.data_dir.clone(),
                    state.config.oci_cache_dir.clone(),
                ];
                let gc = LayerCacheGc::new(
                    cache.clone(),
                    std::time::Duration::from_secs(state.config.oci_gc_retention_secs),
                    deployed,
                    allowed,
                );
                state.oci_cache = Some(cache);
                state.oci_cache_gc = Some(gc);
            }
            Err(e) => {
                eprintln!("oci layer cache init failed (cache disabled): {e}");
            }
        }

        state
    }

    /// Handle for `main` to wire the activator and spawn the periodic control
    /// loop: returns `(ingress, controller)` when the autoscaler was constructed
    /// (only via [`AppState::new`]).
    pub fn autoscaler_handle(
        &self,
    ) -> Option<(
        Arc<IngressState>,
        Arc<tokio::sync::Mutex<crate::autoscale::controller::Controller>>,
    )> {
        self.autoscaler
            .as_ref()
            .map(|c| (self.ingress.clone(), c.clone()))
    }

    pub fn new_with_deploy_dependencies<R, H, C>(
        config: AppConfig,
        store: &SqliteStore,
        runtime: R,
        health: H,
        command_runner: C,
        ingress: Arc<IngressState>,
    ) -> Self
    where
        R: Runtime + 'static,
        H: HealthChecker + 'static,
        C: CommandRunner + 'static,
    {
        Self::new_with_deploy_dependencies_and_log(
            config,
            store,
            runtime,
            health,
            command_runner,
            ingress,
            AccessLogStore::new(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_deploy_dependencies_and_log<R, H, C>(
        config: AppConfig,
        store: &SqliteStore,
        runtime: R,
        health: H,
        command_runner: C,
        ingress: Arc<IngressState>,
        access_log: AccessLogStore,
    ) -> Self
    where
        R: Runtime + 'static,
        H: HealthChecker + 'static,
        C: CommandRunner + 'static,
    {
        let pool = store.pool();
        let registry = crate::registry::repo::HostedRegistryRepo::new(pool.clone());
        let registry_storage =
            crate::registry::storage::RegistryStorage::new(config.data_dir.clone());
        let registry_gc = crate::registry::gc::RegistryGc::new(
            registry_storage.clone(),
            registry.clone(),
            std::time::Duration::from_secs(config.registry_gc_grace_secs),
        );
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
            registry,
            registry_storage,
            registry_gc,
            runtime: Arc::new(runtime),
            health: Arc::new(health),
            command_runner: Arc::new(command_runner),
            ingress,
            routes: Arc::new(Mutex::new(BTreeMap::new())),
            access_log,
            domain_verifier: Arc::new(crate::verification::HttpDomainVerifier::new()),
            verifying_domains: Arc::new(Mutex::new(std::collections::HashSet::new())),
            acme_challenges: crate::ingress::pingora::acme::ChallengeStore::new(),
            autoscaler: None,
            oci_cache: None,
            oci_cache_gc: None,
            job_enqueue: None,
        }
    }

    /// Inject the job-executor run-channel sender (daemon boot). Lets
    /// `POST /v1/jobs/{id}/run` hand a manually-triggered run to the executor.
    pub fn with_job_enqueue(
        mut self,
        sender: tokio::sync::mpsc::UnboundedSender<crate::domain::JobRun>,
    ) -> Self {
        self.job_enqueue = Some(sender);
        self
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

/// Test-only health checker; the `AppStateBuilder` below defaults to it.
#[cfg(any(test, feature = "test-support"))]
use crate::health::FakeHealthChecker;

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
        let registry = crate::registry::repo::HostedRegistryRepo::new(pool.clone());
        let registry_storage =
            crate::registry::storage::RegistryStorage::new(self.config.data_dir.clone());
        let registry_gc = crate::registry::gc::RegistryGc::new(
            registry_storage.clone(),
            registry.clone(),
            std::time::Duration::from_secs(self.config.registry_gc_grace_secs),
        );
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
            registry,
            registry_storage,
            registry_gc,
            runtime: self
                .runtime
                .unwrap_or_else(|| Arc::new(crate::runtime::FakeRuntime::default())),
            health: Arc::new(FakeHealthChecker::healthy()),
            command_runner: Arc::new(TokioCommandRunner),
            ingress: Arc::new(IngressState::default()),
            routes: Arc::new(Mutex::new(BTreeMap::new())),
            access_log: AccessLogStore::new(),
            domain_verifier: self
                .domain_verifier
                .unwrap_or_else(|| Arc::new(crate::verification::HttpDomainVerifier::new())),
            verifying_domains: Arc::new(Mutex::new(std::collections::HashSet::new())),
            acme_challenges: crate::ingress::pingora::acme::ChallengeStore::new(),
            autoscaler: None,
            oci_cache: None,
            oci_cache_gc: None,
            job_enqueue: None,
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
    let admin_rate_limiter = AdminRateLimiter::default();
    let auth_public = api::auth::public_router().route_layer(middleware::from_fn_with_state(
        rate_limiter,
        rate_limit_login,
    ));

    let authed = api::auth::router()
        .merge(api::bootstrap::router())
        .merge(api::users::router())
        .merge(api::tokens::router())
        .merge(api::jobs::router())
        .merge(api::credentials::router())
        .merge(api::services::router())
        .merge(api::console::router())
        .merge(api::deployments::router())
        .merge(api::uploads::router())
        .merge(api::domains::router())
        .merge(api::projects::router())
        .merge(api::members::router())
        .merge(api::registries::router())
        .merge(api::observability::router())
        .merge(api::ingress::router())
        .merge(api::oci::router())
        .merge(api::hosted_registry::router())
        .merge(api::node::router())
        .route_layer(middleware::from_fn_with_state(state.clone(), require_auth))
        .route_layer(middleware::from_fn_with_state(
            admin_rate_limiter,
            rate_limit_admin,
        ));

    let trace_layer = tower_http::trace::TraceLayer::new_for_http()
        .make_span_with(|req: &Request| {
            tracing::info_span!(
                "http",
                method = %req.method(),
                path = %req.uri().path(),
            )
        })
        .on_response(
            |resp: &Response, latency: std::time::Duration, _span: &tracing::Span| {
                let status = resp.status().as_u16();
                if status >= 500 {
                    tracing::error!(status, latency_ms = latency.as_millis() as u64, "response");
                } else if status >= 400 {
                    tracing::warn!(status, latency_ms = latency.as_millis() as u64, "response");
                } else {
                    tracing::info!(status, latency_ms = latency.as_millis() as u64, "response");
                }
            },
        );

    Router::new()
        .route("/healthz", get(api::health::healthz))
        // ACME HTTP-01 and denia domain-verification challenge routes are served
        // WITHOUT the per-IP login rate limiter (audit B1). Let's Encrypt
        // validates from multiple distributed vantage points and retries, so a
        // ~5/min per-IP bucket would return 429 and silently break cert
        // issuance/renewal once `:80` is publicly reachable. Both handlers do
        // exact-match in-memory lookups and serve only non-secret verification
        // tokens / post-validation key authorizations, so they are safe to
        // expose unthrottled.
        .route(
            "/.well-known/denia-challenge/{token}",
            get(api::domains::challenge_handler),
        )
        .route(
            "/.well-known/acme-challenge/{token}",
            get(api::domains::acme_challenge_handler),
        )
        // `console::public_router` (the ticket-authenticated websocket upgrade)
        // is merged OUTSIDE the bearer-auth layer: browser websockets cannot send
        // an `Authorization` header, so the single-use console ticket is the
        // credential. See ADR-033.
        .nest(
            "/v1",
            auth_public
                .merge(authed)
                .merge(api::console::public_router()),
        )
        .nest(
            "/v2",
            crate::registry::api_v2::router()
                .route_layer(middleware::from_fn_with_state(
                    state.clone(),
                    crate::registry::api_v2::registry_auth,
                ))
                // Exempt the registry from the global 1 MiB body cap below:
                // image layer uploads (PATCH/PUT/POST) are far larger. The
                // `/v2` write handlers do NOT buffer the body — they stream it
                // to disk while enforcing their own per-request size cap
                // (`registry_max_blob_bytes` / `registry_max_manifest_bytes`),
                // so disabling axum's buffered-extractor limit here is safe and
                // preserves the ADR-015 bounded-RAM guarantee on the inbound
                // path. This inner layer overrides the outer DefaultBodyLimit
                // for `/v2` only; `/v1` stays capped at 1 MiB.
                .layer(axum::extract::DefaultBodyLimit::disable()),
        )
        .layer(axum::extract::DefaultBodyLimit::max(1024 * 1024))
        .layer(middleware::from_fn(security_headers))
        .layer(trace_layer)
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
    headers.insert(
        header::STRICT_TRANSPORT_SECURITY,
        header::HeaderValue::from_static("max-age=31536000; includeSubDomains"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        header::HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self' ws: wss:; frame-ancestors 'none'; base-uri 'self'; form-action 'self'",
        ),
    );
    headers.insert(
        header::HeaderName::from_static("cross-origin-resource-policy"),
        header::HeaderValue::from_static("same-origin"),
    );
    headers.insert(
        header::HeaderName::from_static("permissions-policy"),
        header::HeaderValue::from_static(
            "geolocation=(), microphone=(), camera=(), payment=(), usb=(), interest-cohort=()",
        ),
    );
    response
}
