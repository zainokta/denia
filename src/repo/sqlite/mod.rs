//! Sqlite-backed building blocks for repo implementations.
//!
//! Task 8 splits the `SqliteStore` impl across per-aggregate files in this
//! module. The struct itself stays in `state.rs`; each file here adds another
//! `impl SqliteStore { ... }` block. Migration logic lives in `pool::run_migrations`.

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

pub use pool::{SqlitePool, run_migrations};
