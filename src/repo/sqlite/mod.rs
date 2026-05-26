//! Sqlite-backed building blocks for repo implementations.
//!
//! Task 9 adds per-aggregate `Sqlite*Repo` structs implementing each
//! `repo::*Repo` trait alongside the existing `impl SqliteStore { ... }`
//! blocks. Both code paths funnel through the same `*_q` free functions so
//! SQL is never duplicated. `SqliteStore` will be deleted in Task 10.

pub mod credentials;
pub mod deployments;
pub mod domains;
pub mod jobs;
pub mod pool;
pub mod projects;
pub mod registries;
pub mod services;
pub mod tokens;
pub mod users;

pub use credentials::SqliteCredentialRepo;
pub use deployments::SqliteDeploymentRepo;
pub use domains::SqliteDomainRepo;
pub use jobs::SqliteJobRepo;
pub use pool::{SqlitePool, run_migrations};
pub use projects::SqliteProjectRepo;
pub use registries::SqliteRegistryRepo;
pub use services::SqliteServiceRepo;
pub use tokens::SqliteTokenRepo;
pub use users::SqliteUserRepo;
