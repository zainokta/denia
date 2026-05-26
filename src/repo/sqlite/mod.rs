//! Sqlite-backed building blocks for repo implementations.
//!
//! Task 7 introduces only the pool + placeholder migration entry point. Task 8
//! moves the real migration body and the per-aggregate `Sqlite*Repo` structs
//! into this module.

pub mod pool;

pub use pool::{SqlitePool, run_migrations};
