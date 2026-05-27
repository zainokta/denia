//! Repository error type.
//!
//! Mirrors `state::StateError` 1:1 so swapping `Result<_, StateError>` for
//! `Result<_, RepoError>` in callers is mechanical. Owned by `repo::` so the
//! repo traits do not depend on `state.rs`.

#[derive(Debug, thiserror::Error)]
pub enum RepoError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("uuid error: {0}")]
    Uuid(#[from] uuid::Error),
    #[error("time parse error: {0}")]
    Time(#[from] chrono::ParseError),
    #[error("state lock poisoned")]
    LockPoisoned,
    #[error("cannot delete project with existing services")]
    ProjectNotEmpty,
    #[error("project not found")]
    UnknownProject,
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error("cannot delete the last super admin")]
    LastSuperAdmin,
    #[error("invalid status: {0}")]
    InvalidStatus(String),
    #[error("registry not found")]
    RegistryNotFound,
    #[error("registry is referenced by one or more services")]
    RegistryInUse,
    #[error("admin already initialized")]
    AdminAlreadyInitialized,
}
