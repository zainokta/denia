//! Per-aggregate repositories.
//!
//! Concrete `Sqlite*Repo` types in `sqlite/` are the repositories; `AppState`
//! holds them directly (no trait objects). `RepoError` is the shared error
//! type. The `mock` module provides only test fakes for non-repo seams.

pub mod error;
#[cfg(any(test, feature = "test-support"))]
pub mod mock;
pub mod sqlite;

pub use error::RepoError;
