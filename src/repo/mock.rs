//! In-memory repository mocks for tests and external test-support consumers.
//!
//! Gated behind `cfg(test)` (for this crate's own unit tests) or the
//! `test-support` cargo feature (for downstream integration tests). They must
//! never compile into the release binary.
//!
//! These mocks are intentionally minimal: just enough fidelity for handler
//! tests and contract round-trips. They do not reproduce every SQL constraint.
#![cfg(any(test, feature = "test-support"))]

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::artifacts::ArtifactRecord;
use crate::domain::{
    ApiToken, Credential, CredentialKind, Deployment, DeploymentRequest, DeploymentStatus,
    DomainStatus, Job, JobRun, JobRunStatus, Project, ProjectMembership, Registry, Role,
    ServiceConfig, ServiceDomain, Session, User,
};
use crate::repo::credential_repo::CredentialRepo;
use crate::repo::deployment_repo::DeploymentRepo;
use crate::repo::domain_repo::DomainRepo;
use crate::repo::error::RepoError;
use crate::repo::job_repo::JobRepo;
use crate::repo::project_repo::ProjectRepo;
use crate::repo::registry_repo::RegistryRepo;
use crate::repo::service_repo::ServiceRepo;
use crate::repo::token_repo::TokenRepo;
use crate::repo::user_repo::UserRepo;
use crate::secrets::SecretRef;
use crate::verification::{DomainVerifier, error::DomainVerifyError};

fn lock<T>(m: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>, RepoError> {
    m.lock().map_err(|_| RepoError::LockPoisoned)
}

// --- Services -------------------------------------------------------------

#[derive(Default)]
pub struct InMemoryServiceRepo {
    inner: Mutex<HashMap<Uuid, ServiceConfig>>,
}

impl ServiceRepo for InMemoryServiceRepo {
    fn put_service(&self, config: ServiceConfig) -> Result<ServiceConfig, RepoError> {
        lock(&self.inner)?.insert(config.id, config.clone());
        Ok(config)
    }

    fn list_services(&self) -> Result<Vec<ServiceConfig>, RepoError> {
        Ok(lock(&self.inner)?.values().cloned().collect())
    }

    fn get_service(&self, service_id: Uuid) -> Result<Option<ServiceConfig>, RepoError> {
        Ok(lock(&self.inner)?.get(&service_id).cloned())
    }
}

// --- Domains --------------------------------------------------------------

#[derive(Default)]
pub struct InMemoryDomainRepo {
    inner: Mutex<HashMap<Uuid, ServiceDomain>>,
}

impl DomainRepo for InMemoryDomainRepo {
    fn put_service_domain(&self, d: &ServiceDomain) -> Result<(), RepoError> {
        lock(&self.inner)?.insert(d.id, d.clone());
        Ok(())
    }

    fn get_service_domain(&self, id: Uuid) -> Result<Option<ServiceDomain>, RepoError> {
        Ok(lock(&self.inner)?.get(&id).cloned())
    }

    fn get_service_domain_by_token(&self, token: &str) -> Result<Option<ServiceDomain>, RepoError> {
        Ok(lock(&self.inner)?
            .values()
            .find(|d| d.challenge_token == token)
            .cloned())
    }

    fn list_service_domains_by_service(
        &self,
        service_id: Uuid,
    ) -> Result<Vec<ServiceDomain>, RepoError> {
        Ok(lock(&self.inner)?
            .values()
            .filter(|d| d.service_id == service_id)
            .cloned()
            .collect())
    }

    fn update_service_domain_status(
        &self,
        id: Uuid,
        status: DomainStatus,
        verified_at: Option<DateTime<Utc>>,
        last_error: Option<String>,
    ) -> Result<(), RepoError> {
        let mut g = lock(&self.inner)?;
        if let Some(d) = g.get_mut(&id) {
            d.status = status;
            d.verified_at = verified_at;
            d.last_check_at = Some(Utc::now());
            d.last_error = last_error;
        }
        Ok(())
    }

    fn delete_service_domain(&self, id: Uuid) -> Result<(), RepoError> {
        lock(&self.inner)?.remove(&id);
        Ok(())
    }

    fn list_verified_hostnames(&self, service_id: Uuid) -> Result<Vec<String>, RepoError> {
        let mut out: Vec<String> = lock(&self.inner)?
            .values()
            .filter(|d| d.service_id == service_id && d.status == DomainStatus::Verified)
            .map(|d| d.hostname.clone())
            .collect();
        out.sort();
        Ok(out)
    }

    fn list_all_service_domains(&self) -> Result<Vec<ServiceDomain>, RepoError> {
        Ok(lock(&self.inner)?.values().cloned().collect())
    }
}

// --- Registries -----------------------------------------------------------

#[derive(Default)]
pub struct InMemoryRegistryRepo {
    inner: Mutex<HashMap<Uuid, Registry>>,
}

impl RegistryRepo for InMemoryRegistryRepo {
    fn create_registry(&self, registry: &Registry) -> Result<(), RepoError> {
        lock(&self.inner)?.insert(registry.id, registry.clone());
        Ok(())
    }

    fn update_registry(&self, registry: &Registry) -> Result<(), RepoError> {
        let mut g = lock(&self.inner)?;
        if !g.contains_key(&registry.id) {
            return Err(RepoError::RegistryNotFound);
        }
        g.insert(registry.id, registry.clone());
        Ok(())
    }

    fn registry(&self, id: Uuid) -> Result<Option<Registry>, RepoError> {
        Ok(lock(&self.inner)?.get(&id).cloned())
    }

    fn registries_for_project(&self, project_id: Uuid) -> Result<Vec<Registry>, RepoError> {
        Ok(lock(&self.inner)?
            .values()
            .filter(|r| r.project_id == project_id)
            .cloned()
            .collect())
    }

    fn delete_registry(&self, id: Uuid) -> Result<(), RepoError> {
        // NOTE: the in-use guard is SQL-specific (it scans the services table).
        // The mock skips it; the live SqliteRegistryRepo contract test covers
        // the `RegistryInUse` guard.
        lock(&self.inner)?.remove(&id);
        Ok(())
    }
}

// --- Projects -------------------------------------------------------------

#[derive(Default)]
pub struct InMemoryProjectRepo {
    inner: Mutex<HashMap<Uuid, Project>>,
}

impl ProjectRepo for InMemoryProjectRepo {
    fn default_project_id(&self) -> Result<Uuid, RepoError> {
        lock(&self.inner)?
            .values()
            .find(|p| p.name == "default")
            .map(|p| p.id)
            .ok_or(RepoError::UnknownProject)
    }

    fn put_project(&self, project: Project) -> Result<Project, RepoError> {
        lock(&self.inner)?.insert(project.id, project.clone());
        Ok(project)
    }

    fn get_project(&self, project_id: Uuid) -> Result<Option<Project>, RepoError> {
        Ok(lock(&self.inner)?.get(&project_id).cloned())
    }

    fn list_projects(&self) -> Result<Vec<Project>, RepoError> {
        Ok(lock(&self.inner)?.values().cloned().collect())
    }

    fn count_services_in_project(&self, _project_id: Uuid) -> Result<i64, RepoError> {
        // Mocks do not cross-reference the service map; tests that need the
        // not-empty guard should use the live SqliteProjectRepo.
        Ok(0)
    }

    fn delete_project(&self, project_id: Uuid) -> Result<(), RepoError> {
        lock(&self.inner)?.remove(&project_id);
        Ok(())
    }
}

// --- Users / sessions / memberships ---------------------------------------

#[derive(Default)]
pub struct InMemoryUserRepo {
    users: Mutex<HashMap<Uuid, User>>,
    sessions: Mutex<HashMap<String, Session>>,
    memberships: Mutex<Vec<ProjectMembership>>,
}

impl UserRepo for InMemoryUserRepo {
    fn create_user(
        &self,
        username: &str,
        password_hash: &str,
        is_super_admin: bool,
    ) -> Result<User, RepoError> {
        let user = User {
            id: Uuid::now_v7(),
            username: username.to_string(),
            password_hash: password_hash.to_string(),
            is_super_admin,
            created_at: Utc::now(),
        };
        lock(&self.users)?.insert(user.id, user.clone());
        Ok(user)
    }

    fn get_user(&self, user_id: Uuid) -> Result<Option<User>, RepoError> {
        Ok(lock(&self.users)?.get(&user_id).cloned())
    }

    fn list_users(&self) -> Result<Vec<User>, RepoError> {
        Ok(lock(&self.users)?.values().cloned().collect())
    }

    fn delete_user(&self, user_id: Uuid) -> Result<(), RepoError> {
        lock(&self.users)?.remove(&user_id);
        Ok(())
    }

    fn verify_login(&self, username: &str, _password: &str) -> Result<User, RepoError> {
        lock(&self.users)?
            .values()
            .find(|u| u.username == username)
            .cloned()
            .ok_or(RepoError::InvalidCredentials)
    }

    fn create_session(&self, user_id: Uuid, ttl_hours: i64) -> Result<Session, RepoError> {
        let session = Session {
            token: Uuid::now_v7().to_string(),
            user_id,
            expires_at: Utc::now() + chrono::TimeDelta::hours(ttl_hours),
        };
        lock(&self.sessions)?.insert(session.token.clone(), session.clone());
        Ok(session)
    }

    fn user_for_session(&self, token_hash: &str) -> Result<Option<User>, RepoError> {
        let user_id = match lock(&self.sessions)?.get(token_hash) {
            Some(s) => s.user_id,
            None => return Ok(None),
        };
        self.get_user(user_id)
    }

    fn delete_session(&self, token_hash: &str) -> Result<(), RepoError> {
        lock(&self.sessions)?.remove(token_hash);
        Ok(())
    }

    fn set_membership(&self, user_id: Uuid, project_id: Uuid, role: Role) -> Result<(), RepoError> {
        let mut g = lock(&self.memberships)?;
        g.retain(|m| !(m.user_id == user_id && m.project_id == project_id));
        g.push(ProjectMembership {
            user_id,
            project_id,
            role,
        });
        Ok(())
    }

    fn role_for(&self, user_id: Uuid, project_id: Uuid) -> Result<Option<Role>, RepoError> {
        Ok(lock(&self.memberships)?
            .iter()
            .find(|m| m.user_id == user_id && m.project_id == project_id)
            .map(|m| m.role))
    }

    fn list_members(&self, project_id: Uuid) -> Result<Vec<ProjectMembership>, RepoError> {
        Ok(lock(&self.memberships)?
            .iter()
            .filter(|m| m.project_id == project_id)
            .cloned()
            .collect())
    }

    fn remove_membership(&self, user_id: Uuid, project_id: Uuid) -> Result<(), RepoError> {
        lock(&self.memberships)?.retain(|m| !(m.user_id == user_id && m.project_id == project_id));
        Ok(())
    }

    fn list_memberships_for_user(
        &self,
        user_id: Uuid,
    ) -> Result<Vec<ProjectMembership>, RepoError> {
        Ok(lock(&self.memberships)?
            .iter()
            .filter(|m| m.user_id == user_id)
            .cloned()
            .collect())
    }
}

// --- Deployments / artifacts ----------------------------------------------

#[derive(Default)]
pub struct InMemoryDeploymentRepo {
    deployments: Mutex<HashMap<Uuid, Deployment>>,
    promoted: Mutex<HashMap<Uuid, Uuid>>,
    artifacts: Mutex<HashMap<Uuid, ArtifactRecord>>,
}

impl DeploymentRepo for InMemoryDeploymentRepo {
    fn create_deployment(&self, request: DeploymentRequest) -> Result<Deployment, RepoError> {
        let deployment = Deployment {
            id: Uuid::now_v7(),
            service_id: request.service_id(),
            request,
            status: DeploymentStatus::Pending,
            created_at: Utc::now(),
        };
        lock(&self.deployments)?.insert(deployment.id, deployment.clone());
        Ok(deployment)
    }

    fn list_deployments(&self, service_id: Uuid) -> Result<Vec<Deployment>, RepoError> {
        Ok(lock(&self.deployments)?
            .values()
            .filter(|d| d.service_id == service_id)
            .cloned()
            .collect())
    }

    fn update_deployment_status(
        &self,
        deployment_id: Uuid,
        status: DeploymentStatus,
    ) -> Result<(), RepoError> {
        if let Some(d) = lock(&self.deployments)?.get_mut(&deployment_id) {
            d.status = status;
        }
        Ok(())
    }

    fn promote_deployment(&self, service_id: Uuid, deployment_id: Uuid) -> Result<(), RepoError> {
        lock(&self.promoted)?.insert(service_id, deployment_id);
        Ok(())
    }

    fn promoted_deployment(&self, service_id: Uuid) -> Result<Option<Uuid>, RepoError> {
        Ok(lock(&self.promoted)?.get(&service_id).copied())
    }

    fn clear_promoted_deployment(&self, service_id: Uuid) -> Result<(), RepoError> {
        lock(&self.promoted)?.remove(&service_id);
        Ok(())
    }

    fn put_artifact(&self, artifact: ArtifactRecord) -> Result<ArtifactRecord, RepoError> {
        lock(&self.artifacts)?.insert(artifact.id, artifact.clone());
        Ok(artifact)
    }

    fn list_artifacts(&self) -> Result<Vec<ArtifactRecord>, RepoError> {
        Ok(lock(&self.artifacts)?.values().cloned().collect())
    }
}

// --- Jobs / job runs ------------------------------------------------------

#[derive(Default)]
pub struct InMemoryJobRepo {
    jobs: Mutex<HashMap<Uuid, Job>>,
    runs: Mutex<HashMap<Uuid, JobRun>>,
}

impl JobRepo for InMemoryJobRepo {
    fn put_job(&self, job: Job) -> Result<Job, RepoError> {
        lock(&self.jobs)?.insert(job.id, job.clone());
        Ok(job)
    }

    fn get_job(&self, job_id: Uuid) -> Result<Option<Job>, RepoError> {
        Ok(lock(&self.jobs)?.get(&job_id).cloned())
    }

    fn list_jobs(&self, project_id: Uuid) -> Result<Vec<Job>, RepoError> {
        Ok(lock(&self.jobs)?
            .values()
            .filter(|j| j.project_id == project_id)
            .cloned()
            .collect())
    }

    fn delete_job(&self, job_id: Uuid) -> Result<(), RepoError> {
        lock(&self.jobs)?.remove(&job_id);
        lock(&self.runs)?.retain(|_, r| r.job_id != job_id);
        Ok(())
    }

    fn create_job_run(&self, job_id: Uuid) -> Result<JobRun, RepoError> {
        let run = JobRun {
            id: Uuid::now_v7(),
            job_id,
            status: JobRunStatus::Pending,
            attempt: 1,
            exit_code: None,
            started_at: None,
            finished_at: None,
            created_at: Utc::now(),
        };
        lock(&self.runs)?.insert(run.id, run.clone());
        Ok(run)
    }

    fn list_job_runs(&self, job_id: Uuid) -> Result<Vec<JobRun>, RepoError> {
        let mut runs: Vec<JobRun> = lock(&self.runs)?
            .values()
            .filter(|r| r.job_id == job_id)
            .cloned()
            .collect();
        runs.sort_by_key(|r| std::cmp::Reverse(r.created_at));
        Ok(runs)
    }

    fn update_job_run(
        &self,
        run_id: Uuid,
        status: JobRunStatus,
        exit_code: Option<i32>,
    ) -> Result<(), RepoError> {
        if let Some(r) = lock(&self.runs)?.get_mut(&run_id) {
            r.status = status;
            r.exit_code = exit_code;
            r.finished_at = Some(Utc::now());
        }
        Ok(())
    }

    fn active_run(&self, job_id: Uuid) -> Result<Option<JobRun>, RepoError> {
        Ok(lock(&self.runs)?
            .values()
            .find(|r| {
                r.job_id == job_id
                    && matches!(r.status, JobRunStatus::Pending | JobRunStatus::Running)
            })
            .cloned())
    }

    fn fail_orphan_runs(&self) -> Result<usize, RepoError> {
        let mut count = 0;
        for r in lock(&self.runs)?.values_mut() {
            if matches!(r.status, JobRunStatus::Pending | JobRunStatus::Running) {
                r.status = JobRunStatus::Failed;
                r.finished_at = Some(Utc::now());
                count += 1;
            }
        }
        Ok(count)
    }

    fn claim_due_jobs(&self, now: DateTime<Utc>) -> Result<Vec<Job>, RepoError> {
        Ok(lock(&self.jobs)?
            .values()
            .filter(|j| j.next_run_at.map(|n| n <= now).unwrap_or(false))
            .cloned()
            .collect())
    }

    fn set_job_next_run(
        &self,
        job_id: Uuid,
        next_run_at: Option<DateTime<Utc>>,
        last_enqueued_at: Option<DateTime<Utc>>,
    ) -> Result<(), RepoError> {
        let mut g = lock(&self.jobs)?;
        let job = g.get_mut(&job_id).ok_or(RepoError::InvalidCredentials)?;
        job.next_run_at = next_run_at;
        job.last_enqueued_at = last_enqueued_at;
        Ok(())
    }
}

// --- API tokens -----------------------------------------------------------

#[derive(Default)]
pub struct InMemoryTokenRepo {
    // token_hash -> (token row, owning user)
    tokens: Mutex<HashMap<Uuid, ApiToken>>,
    hashes: Mutex<HashMap<String, Uuid>>,
    users: Mutex<HashMap<Uuid, User>>,
}

impl InMemoryTokenRepo {
    /// Register a user so `user_for_api_token` can resolve tokens to it.
    pub fn insert_user(&self, user: User) {
        self.users.lock().expect("users lock").insert(user.id, user);
    }

    /// Register a token hash -> token row mapping directly (mock convenience).
    pub fn insert_token_hash(&self, token_hash: &str, token: ApiToken) {
        self.hashes
            .lock()
            .expect("hashes lock")
            .insert(token_hash.to_string(), token.id);
        self.tokens
            .lock()
            .expect("tokens lock")
            .insert(token.id, token);
    }
}

impl TokenRepo for InMemoryTokenRepo {
    fn create_api_token(&self, user_id: Uuid, name: &str) -> Result<ApiToken, RepoError> {
        let token = ApiToken {
            id: Uuid::now_v7(),
            user_id,
            name: name.to_string(),
            token: Uuid::now_v7().to_string(),
            created_at: Utc::now(),
        };
        lock(&self.tokens)?.insert(token.id, token.clone());
        Ok(token)
    }

    fn user_for_api_token(&self, token_hash: &str) -> Result<Option<User>, RepoError> {
        let token_id = match lock(&self.hashes)?.get(token_hash) {
            Some(id) => *id,
            None => return Ok(None),
        };
        let user_id = match lock(&self.tokens)?.get(&token_id) {
            Some(t) => t.user_id,
            None => return Ok(None),
        };
        Ok(lock(&self.users)?.get(&user_id).cloned())
    }

    fn list_api_tokens(&self, user_id: Uuid) -> Result<Vec<ApiToken>, RepoError> {
        Ok(lock(&self.tokens)?
            .values()
            .filter(|t| t.user_id == user_id)
            .cloned()
            .collect())
    }

    fn revoke_api_token(&self, token_id: Uuid) -> Result<(), RepoError> {
        lock(&self.tokens)?.remove(&token_id);
        lock(&self.hashes)?.retain(|_, id| *id != token_id);
        Ok(())
    }
}

// --- Credentials ----------------------------------------------------------

#[derive(Default)]
pub struct InMemoryCredentialRepo {
    inner: Mutex<HashMap<String, Credential>>,
}

impl CredentialRepo for InMemoryCredentialRepo {
    fn put_credential(
        &self,
        name: String,
        kind: CredentialKind,
        secret_ref: SecretRef,
    ) -> Result<Credential, RepoError> {
        let credential = Credential {
            id: Uuid::now_v7(),
            name: name.clone(),
            kind,
            secret_ref,
        };
        lock(&self.inner)?.insert(name, credential.clone());
        Ok(credential)
    }
}

// --- Domain verifier stub -------------------------------------------------

/// A `DomainVerifier` that always succeeds, for handler tests that exercise the
/// verify path without making an outbound HTTP request.
#[derive(Default)]
pub struct StubDomainVerifier;

#[async_trait::async_trait]
impl DomainVerifier for StubDomainVerifier {
    async fn verify(&self, _hostname: &str, _token: &str) -> Result<(), DomainVerifyError> {
        Ok(())
    }
}
