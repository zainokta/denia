//! Sqlite connection pool used by repo implementations.
//!
//! Wraps a single `rusqlite::Connection` behind `Arc<Mutex<..>>` so the
//! synchronous repo traits can be called from `Send + Sync` contexts. This
//! mirrors `state::SqliteStore`'s internals so `SqliteStore::migrate` can
//! delegate here without changing the connection model.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::Utc;
use rusqlite::{Connection, params};
use uuid::Uuid;

use crate::domain::{Project, ServiceConfig};
use crate::repo::error::RepoError;

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

#[derive(Clone)]
pub struct SqlitePool {
    pub(crate) inner: Arc<Mutex<Connection>>,
}

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

/// Runs the full schema migration pipeline against `pool`. Body moved verbatim
/// from `state::SqliteStore::migrate` in Task 8 so the repo layer owns schema
/// setup. `SqliteStore::migrate` now delegates here.
pub fn run_migrations(pool: &SqlitePool) -> Result<(), RepoError> {
    let connection = pool.connection()?;

    connection.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS schema_version (
            version INTEGER NOT NULL
        );
        "#,
    )?;

    let current: i64 = connection
        .query_row(
            "SELECT COALESCE((SELECT version FROM schema_version), 0)",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    if current < 1 {
        connection.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS credentials (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                kind TEXT NOT NULL,
                secret_ref TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS services (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                config_json TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS deployments (
                id TEXT PRIMARY KEY,
                service_id TEXT NOT NULL,
                request_json TEXT NOT NULL,
                status TEXT NOT NULL,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS artifacts (
                id TEXT PRIMARY KEY,
                digest TEXT NOT NULL UNIQUE,
                record_json TEXT NOT NULL,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS promoted_deployments (
                service_id TEXT PRIMARY KEY,
                deployment_id TEXT NOT NULL
            );
            "#,
        )?;
        connection.execute("DELETE FROM schema_version", [])?;
        connection.execute("INSERT INTO schema_version (version) VALUES (1)", [])?;
    }

    if current < 2 {
        let default_project = Project::new("default", None).expect("default project");
        let default_id = default_project.id.to_string();
        let default_json = serde_json::to_string(&default_project)?;

        connection.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS projects (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                description TEXT,
                config_json TEXT NOT NULL
            );
            "#,
        )?;

        let exists: bool = connection
            .query_row(
                "SELECT COUNT(*) > 0 FROM projects WHERE id = ?1",
                params![&default_id],
                |row| row.get(0),
            )
            .unwrap_or(false);

        if !exists {
            connection.execute(
                "INSERT INTO projects (id, name, description, config_json) VALUES (?1, ?2, ?3, ?4)",
                params![
                    &default_id,
                    "default",
                    serde_json::to_string(&default_project.description)?,
                    &default_json
                ],
            )?;
        }

        connection.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS services_new (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                name TEXT NOT NULL,
                config_json TEXT NOT NULL,
                UNIQUE(project_id, name)
            );
            "#,
        )?;

        {
            let mut stmt = connection.prepare("SELECT id, name, config_json FROM services")?;
            let rows = stmt.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;

            for row in rows {
                let (id, name, config_json) = row?;
                if let Ok(mut svc) = serde_json::from_str::<ServiceConfig>(&config_json) {
                    svc.project_id = default_project.id;
                    let new_json = serde_json::to_string(&svc)?;
                    connection.execute(
                        "INSERT OR IGNORE INTO services_new (id, project_id, name, config_json) VALUES (?1, ?2, ?3, ?4)",
                        params![&id, &default_id, &name, &new_json],
                    )?;
                }
            }
        }

        connection.execute_batch(
            r#"
            DROP TABLE IF EXISTS services;
            ALTER TABLE services_new RENAME TO services;
            "#,
        )?;

        connection.execute("DELETE FROM schema_version", [])?;
        connection.execute("INSERT INTO schema_version (version) VALUES (2)", [])?;
    }

    if current < 3 {
        connection.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS users (
                id TEXT PRIMARY KEY,
                username TEXT NOT NULL UNIQUE,
                password_hash TEXT NOT NULL,
                is_super_admin INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS sessions (
                token_hash TEXT PRIMARY KEY,
                user_id TEXT NOT NULL,
                expires_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS api_tokens (
                id TEXT PRIMARY KEY,
                user_id TEXT NOT NULL,
                name TEXT NOT NULL,
                token_hash TEXT NOT NULL UNIQUE,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS project_members (
                user_id TEXT NOT NULL,
                project_id TEXT NOT NULL,
                role TEXT NOT NULL,
                PRIMARY KEY (user_id, project_id)
            );
            "#,
        )?;

        connection.execute("DELETE FROM schema_version", [])?;
        connection.execute("INSERT INTO schema_version (version) VALUES (3)", [])?;
    }

    if current < 4 {
        connection.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS jobs (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                name TEXT NOT NULL,
                config_json TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS job_runs (
                id TEXT PRIMARY KEY,
                job_id TEXT NOT NULL,
                status TEXT NOT NULL,
                attempt INTEGER NOT NULL DEFAULT 1,
                exit_code INTEGER,
                started_at TEXT,
                finished_at TEXT,
                created_at TEXT NOT NULL
            );
            "#,
        )?;

        connection.execute("DELETE FROM schema_version", [])?;
        connection.execute("INSERT INTO schema_version (version) VALUES (4)", [])?;
    }

    if current < 5 {
        connection.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS service_domains (
                id TEXT PRIMARY KEY,
                service_id TEXT NOT NULL,
                hostname TEXT NOT NULL UNIQUE,
                status TEXT NOT NULL,
                challenge_token TEXT NOT NULL UNIQUE,
                verified_at TEXT,
                last_check_at TEXT,
                last_error TEXT,
                created_at TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_service_domains_service
                ON service_domains(service_id);
            CREATE INDEX IF NOT EXISTS idx_service_domains_status
                ON service_domains(status);
            "#,
        )?;

        // Backfill: for each service, parse domains from config and insert service_domains rows
        let now = Utc::now().to_rfc3339();
        let mut stmt = connection.prepare("SELECT id, config_json FROM services")?;
        let rows: Vec<(String, String)> = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<_, _>>()?;
        drop(stmt);

        for (service_id, config_json) in rows {
            if let Ok(svc) = serde_json::from_str::<ServiceConfig>(&config_json) {
                for hostname in &svc.domains {
                    let token = crate::verification::generate_token();
                    let id = Uuid::now_v7().to_string();
                    connection.execute(
                        r#"
                        INSERT OR IGNORE INTO service_domains
                          (id, service_id, hostname, status, challenge_token,
                           verified_at, last_check_at, last_error, created_at)
                        VALUES (?1, ?2, ?3, 'verified', ?4, ?5, NULL, NULL, ?5)
                        "#,
                        params![id, service_id, hostname, token, now],
                    )?;
                }
            }
        }

        connection.execute("DELETE FROM schema_version", [])?;
        connection.execute("INSERT INTO schema_version (version) VALUES (5)", [])?;
    }

    if current < 6 {
        connection.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS registries (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                name TEXT NOT NULL,
                config_json TEXT NOT NULL,
                UNIQUE(project_id, name)
            );
            CREATE INDEX IF NOT EXISTS idx_registries_project ON registries(project_id);
            "#,
        )?;
        connection.execute("DELETE FROM schema_version", [])?;
        connection.execute("INSERT INTO schema_version (version) VALUES (6)", [])?;
    }

    if current < 7 {
        connection.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS system_settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            "#,
        )?;
        connection.execute("DELETE FROM schema_version", [])?;
        connection.execute("INSERT INTO schema_version (version) VALUES (7)", [])?;
    }

    if current < 8 {
        connection.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS autoscale_desired (
                service_id TEXT PRIMARY KEY,
                desired_replicas INTEGER NOT NULL
            );
            "#,
        )?;
        connection.execute("DELETE FROM schema_version", [])?;
        connection.execute("INSERT INTO schema_version (version) VALUES (8)", [])?;
    }

    Ok(())
}
