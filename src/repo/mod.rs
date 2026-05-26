//! Per-aggregate repository traits (Task 7 skeleton).
//!
//! This module is additive: it does not yet replace `state::SqliteStore`. The
//! traits live here so Task 8/9/10 can introduce concrete sqlite-backed
//! implementations and wire `AppState` to `Arc<dyn ...Repo>` without touching
//! call sites first. See `docs/superpowers/plans/2026-05-25-src-modularization.md`.

pub mod credential_repo;
pub mod deployment_repo;
pub mod domain_repo;
pub mod error;
pub mod job_repo;
pub mod project_repo;
pub mod registry_repo;
pub mod service_repo;
pub mod sqlite;
pub mod token_repo;
pub mod user_repo;

pub use credential_repo::CredentialRepo;
pub use deployment_repo::DeploymentRepo;
pub use domain_repo::DomainRepo;
pub use error::RepoError;
pub use job_repo::JobRepo;
pub use project_repo::ProjectRepo;
pub use registry_repo::RegistryRepo;
pub use service_repo::ServiceRepo;
pub use token_repo::TokenRepo;
pub use user_repo::UserRepo;
