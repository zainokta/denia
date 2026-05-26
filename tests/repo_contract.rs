//! Contract tests for the per-aggregate repository traits, run against the
//! real `Sqlite*Repo` implementations backed by an in-memory SQLite pool.
//!
//! This is the highest-value coverage in Task 13: it catches SQL regressions
//! that the in-memory mocks (which skip SQL constraints) would not. The mocks
//! are exercised indirectly by the handler unit tests in `src/api/*.rs`.
//!
//! Registry and service-domain CRUD/guard paths are already covered by the
//! unit tests in `src/state.rs`, so they are not duplicated here beyond the
//! token-lookup path that the unauthenticated challenge route depends on.

use chrono::Utc;
use uuid::Uuid;

use denia::domain::{
    CredentialKind, DeploymentRequest, DomainStatus, ExternalImageSource, HealthCheck, Job,
    Project, Role, ServiceConfig, ServiceDomain, ServiceSource,
};
use denia::repo::deployment_repo::DeploymentRepo;
use denia::repo::domain_repo::DomainRepo;
use denia::repo::job_repo::JobRepo;
use denia::repo::project_repo::ProjectRepo;
use denia::repo::service_repo::ServiceRepo;
use denia::repo::sqlite::{
    SqliteCredentialRepo, SqliteDeploymentRepo, SqliteDomainRepo, SqliteJobRepo, SqliteProjectRepo,
    SqliteServiceRepo, SqliteTokenRepo, SqliteUserRepo,
};
use denia::repo::token_repo::TokenRepo;
use denia::repo::user_repo::UserRepo;
use denia::secrets::SecretRef;
use denia::state::SqliteStore;

fn migrated_store() -> SqliteStore {
    let store = SqliteStore::open_in_memory().expect("open in-memory store");
    store.migrate().expect("run migrations");
    store
}

fn seed_project(store: &SqliteStore, name: &str) -> Project {
    let repo = SqliteProjectRepo::new(store.pool());
    repo.put_project(Project::new(name, None).unwrap()).unwrap()
}

/// The default project is seeded by migrations; fetch it by name-derived id.
fn default_project(store: &SqliteStore) -> Project {
    let repo = SqliteProjectRepo::new(store.pool());
    let id = repo.default_project_id().unwrap();
    repo.get_project(id).unwrap().unwrap()
}

fn external_service(project_id: Uuid, name: &str) -> ServiceConfig {
    ServiceConfig::new(
        project_id,
        name,
        vec!["example.com".into()],
        ServiceSource::ExternalImage(ExternalImageSource {
            image: "nginx".into(),
            credential: None,
            registry_id: None,
            image_ref: None,
        }),
        80,
        HealthCheck::new("/health", 5),
        None,
        Vec::new(),
    )
    .unwrap()
}

// --- Services -------------------------------------------------------------

#[test]
fn service_roundtrips_via_get_and_list() {
    let store = migrated_store();
    let project = default_project(&store);
    let repo = SqliteServiceRepo::new(store.pool());
    let svc = external_service(project.id, "web");

    repo.put_service(svc.clone()).unwrap();
    let fetched = repo.get_service(svc.id).unwrap().unwrap();
    assert_eq!(fetched.name, "web");
    assert_eq!(repo.list_services().unwrap().len(), 1);
}

#[test]
fn service_get_missing_is_none() {
    let store = migrated_store();
    let repo = SqliteServiceRepo::new(store.pool());
    assert!(repo.get_service(Uuid::now_v7()).unwrap().is_none());
}

// --- Projects -------------------------------------------------------------

#[test]
fn project_roundtrips_and_default_lookup() {
    let store = migrated_store();
    let repo = SqliteProjectRepo::new(store.pool());
    let project = repo
        .put_project(Project::new("team-a", None).unwrap())
        .unwrap();

    assert_eq!(
        repo.get_project(project.id).unwrap().unwrap().name,
        "team-a"
    );
    // The migration-seeded "default" project remains the default.
    assert_eq!(
        repo.get_project(repo.default_project_id().unwrap())
            .unwrap()
            .unwrap()
            .name,
        "default"
    );
}

#[test]
fn project_delete_blocked_when_not_empty() {
    let store = migrated_store();
    let project = seed_project(&store, "team-a");
    SqliteServiceRepo::new(store.pool())
        .put_service(external_service(project.id, "web"))
        .unwrap();

    let repo = SqliteProjectRepo::new(store.pool());
    assert!(repo.delete_project(project.id).is_err());
}

// --- Users / sessions / memberships ---------------------------------------

#[test]
fn user_roundtrip_and_membership() {
    let store = migrated_store();
    let project = default_project(&store);
    let repo = SqliteUserRepo::new(store.pool());
    let user = repo.create_user("alice", "hash", false).unwrap();

    assert_eq!(repo.get_user(user.id).unwrap().unwrap().username, "alice");
    repo.set_membership(user.id, project.id, Role::Operator)
        .unwrap();
    assert_eq!(
        repo.role_for(user.id, project.id).unwrap().unwrap(),
        Role::Operator
    );
    assert_eq!(repo.list_memberships_for_user(user.id).unwrap().len(), 1);
}

#[test]
fn user_get_missing_is_none() {
    let store = migrated_store();
    let repo = SqliteUserRepo::new(store.pool());
    assert!(repo.get_user(Uuid::now_v7()).unwrap().is_none());
}

#[test]
fn user_delete_last_super_admin_rejected() {
    let store = migrated_store();
    let repo = SqliteUserRepo::new(store.pool());
    let admin = repo.create_user("root", "hash", true).unwrap();
    assert!(repo.delete_user(admin.id).is_err());
}

// --- Deployments ----------------------------------------------------------

#[test]
fn deployment_create_list_and_promote() {
    let store = migrated_store();
    let project = default_project(&store);
    let svc = external_service(project.id, "web");
    SqliteServiceRepo::new(store.pool())
        .put_service(svc.clone())
        .unwrap();

    let repo = SqliteDeploymentRepo::new(store.pool());
    let deployment = repo
        .create_deployment(DeploymentRequest::external_image(svc.id, "nginx:latest"))
        .unwrap();
    assert_eq!(repo.list_deployments(svc.id).unwrap().len(), 1);

    assert!(repo.promoted_deployment(svc.id).unwrap().is_none());
    repo.promote_deployment(svc.id, deployment.id).unwrap();
    assert_eq!(
        repo.promoted_deployment(svc.id).unwrap().unwrap(),
        deployment.id
    );
    repo.clear_promoted_deployment(svc.id).unwrap();
    assert!(repo.promoted_deployment(svc.id).unwrap().is_none());
}

// --- Jobs -----------------------------------------------------------------

#[test]
fn job_roundtrip_and_run_lifecycle() {
    let store = migrated_store();
    let project = default_project(&store);
    let repo = SqliteJobRepo::new(store.pool());
    let job = Job::new(
        project.id,
        "nightly",
        ServiceSource::ExternalImage(ExternalImageSource {
            image: "busybox".into(),
            credential: None,
            registry_id: None,
            image_ref: None,
        }),
        None,
    )
    .unwrap();
    repo.put_job(job.clone()).unwrap();

    assert_eq!(repo.get_job(job.id).unwrap().unwrap().name, "nightly");
    assert_eq!(repo.list_jobs(project.id).unwrap().len(), 1);

    let run = repo.create_job_run(job.id).unwrap();
    assert!(repo.active_run(job.id).unwrap().is_some());
    repo.update_job_run(run.id, denia::domain::JobRunStatus::Succeeded, Some(0))
        .unwrap();
    assert!(repo.active_run(job.id).unwrap().is_none());
}

#[test]
fn job_get_missing_is_none() {
    let store = migrated_store();
    let repo = SqliteJobRepo::new(store.pool());
    assert!(repo.get_job(Uuid::now_v7()).unwrap().is_none());
}

// --- Tokens ---------------------------------------------------------------

#[test]
fn api_token_create_list_and_revoke() {
    let store = migrated_store();
    let user = SqliteUserRepo::new(store.pool())
        .create_user("alice", "hash", false)
        .unwrap();
    let repo = SqliteTokenRepo::new(store.pool());

    let token = repo.create_api_token(user.id, "ci").unwrap();
    assert_eq!(repo.list_api_tokens(user.id).unwrap().len(), 1);
    repo.revoke_api_token(token.id).unwrap();
    assert!(repo.list_api_tokens(user.id).unwrap().is_empty());
}

// --- Credentials ----------------------------------------------------------

#[test]
fn credential_put_roundtrip() {
    use denia::repo::credential_repo::CredentialRepo;
    let store = migrated_store();
    let repo = SqliteCredentialRepo::new(store.pool());
    let secret = SecretRef::parse("registry-cred").unwrap();
    let cred = repo
        .put_credential("ghcr".into(), CredentialKind::RegistryBasic, secret)
        .unwrap();
    assert_eq!(cred.name, "ghcr");
}

// --- Domains (token lookup path) ------------------------------------------

#[test]
fn service_domain_token_lookup_roundtrip() {
    let store = migrated_store();
    let project = default_project(&store);
    let svc = external_service(project.id, "web");
    SqliteServiceRepo::new(store.pool())
        .put_service(svc.clone())
        .unwrap();

    let repo = SqliteDomainRepo::new(store.pool());
    let domain = ServiceDomain {
        id: Uuid::now_v7(),
        service_id: svc.id,
        hostname: "app.example.com".into(),
        status: DomainStatus::Pending,
        challenge_token: "challenge-tok".into(),
        verified_at: None,
        last_check_at: None,
        last_error: None,
        created_at: Utc::now(),
    };
    repo.put_service_domain(&domain).unwrap();

    let found = repo
        .get_service_domain_by_token("challenge-tok")
        .unwrap()
        .unwrap();
    assert_eq!(found.id, domain.id);
    assert!(
        repo.get_service_domain_by_token("unknown")
            .unwrap()
            .is_none()
    );
}
