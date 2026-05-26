//! Sqlite connection pool used by the future repo implementations.
//!
//! Wraps a single `rusqlite::Connection` behind `Arc<Mutex<..>>` so the
//! synchronous repo traits can be called from `Send + Sync` contexts. This
//! mirrors `state::SqliteStore`'s internals so Task 8 can move migration logic
//! over without changing the connection model.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::Connection;

use crate::repo::error::RepoError;

#[allow(dead_code)]
fn apply_sqlite_pragmas(connection: &Connection) -> Result<(), RepoError> {
    connection.execute_batch(
        r#"
        PRAGMA journal_mode = WAL;
        PRAGMA foreign_keys = ON;
        PRAGMA synchronous = NORMAL;
        PRAGMA busy_timeout = 5000;
        "#,
    )?;
    Ok(())
}

#[allow(dead_code)]
#[derive(Clone)]
pub struct SqlitePool {
    pub(crate) inner: Arc<Mutex<Connection>>,
}

#[allow(dead_code)]
impl SqlitePool {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, RepoError> {
        let connection = Connection::open(path.as_ref())?;
        apply_sqlite_pragmas(&connection)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(connection)),
        })
    }

    pub fn open_in_memory() -> Result<Self, RepoError> {
        let connection = Connection::open_in_memory()?;
        apply_sqlite_pragmas(&connection)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(connection)),
        })
    }

    pub(crate) fn connection(&self) -> Result<MutexGuard<'_, Connection>, RepoError> {
        self.inner.lock().map_err(|_| RepoError::LockPoisoned)
    }
}

/// Placeholder. Task 8 moves the migration logic from `state::SqliteStore::migrate`
/// here so the `Repo` builder owns schema setup.
#[allow(dead_code)]
pub fn run_migrations(pool: &SqlitePool) -> Result<(), RepoError> {
    let _ = pool;
    Ok(())
}
