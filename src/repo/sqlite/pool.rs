//! Sqlite connection pool used by repo implementations.
//!
//! Wraps a single `rusqlite::Connection` behind `Arc<Mutex<..>>` so the
//! synchronous repo traits can be called from `Send + Sync` contexts. This
//! mirrors `state::SqliteStore`'s internals so `SqliteStore::migrate` can
//! delegate here without changing the connection model.
//!
//! These synchronous calls run directly on the tokio runtime from async
//! handlers, *without* `spawn_blocking`. That is a deliberate single-node
//! tradeoff: light query volume, local-disk SQLite with `busy_timeout`, and a
//! single shared connection that already serializes all access. Long-running /
//! externally-contended work that touches the store (GC sweeps, upload
//! staging, namespaced spawns) already runs under `spawn_blocking`. See
//! ADR-001 ("Synchronous SQLite On The Async Runtime") for the full rationale
//! and the migration path if write-heavy concurrency ever arrives.

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

/// Runs the full schema migration pipeline against `pool`.
///
/// Each migration step (`if current < N { … }`) runs inside its own
/// `rusqlite` transaction that also writes the `schema_version` bump, so a
/// step is all-or-nothing: a crash, `SQLITE_BUSY`, or I/O error partway through
/// a step rolls the step back entirely and leaves `schema_version` unchanged.
/// The next boot then re-runs the whole step from a clean slate. This makes
/// the non-idempotent steps (step 2's `DROP`+`RENAME`, steps 9/10's
/// `ADD COLUMN`) crash-safe, matching ADR-006 ("each migration step runs …
/// inside a single transaction").
///
/// The `schema_version` table is constrained to hold at most one row (`id`
/// CHECK), so the `DELETE`+`INSERT` bump cannot accumulate duplicate rows even
/// under a partial failure on an older DB.
pub fn run_migrations(pool: &SqlitePool) -> Result<(), RepoError> {
    let mut connection = pool.connection()?;

    connection.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS schema_version (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            version INTEGER NOT NULL
        );
        "#,
    )?;

    // Legacy DBs created before the single-row `id` CHECK have a `schema_version`
    // table without the `id` column. Detect that shape and, if found, collapse it
    // to the canonical single-row form transactionally. New/in-memory DBs created
    // by the CREATE above already have `id`, so this is a no-op for them.
    migrate_schema_version_table(&mut connection)?;

    let current: i64 = connection
        .query_row(
            "SELECT COALESCE((SELECT version FROM schema_version WHERE id = 1), 0)",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    if current < 1 {
        let tx = connection.transaction()?;
        tx.execute_batch(
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
        set_schema_version(&tx, 1)?;
        tx.commit()?;
    }

    if current < 2 {
        let tx = connection.transaction()?;
        let default_project = Project::new("default", None).expect("default project");
        let default_id = default_project.id.to_string();
        let default_json = serde_json::to_string(&default_project)?;

        tx.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS projects (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                description TEXT,
                config_json TEXT NOT NULL
            );
            "#,
        )?;

        let exists: bool = tx
            .query_row(
                "SELECT COUNT(*) > 0 FROM projects WHERE id = ?1",
                params![&default_id],
                |row| row.get(0),
            )
            .unwrap_or(false);

        if !exists {
            tx.execute(
                "INSERT INTO projects (id, name, description, config_json) VALUES (?1, ?2, ?3, ?4)",
                params![
                    &default_id,
                    "default",
                    serde_json::to_string(&default_project.description)?,
                    &default_json
                ],
            )?;
        }

        tx.execute_batch(
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
            let mut stmt = tx.prepare("SELECT id, name, config_json FROM services")?;
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
                    tx.execute(
                        "INSERT OR IGNORE INTO services_new (id, project_id, name, config_json) VALUES (?1, ?2, ?3, ?4)",
                        params![&id, &default_id, &name, &new_json],
                    )?;
                }
            }
        }

        tx.execute_batch(
            r#"
            DROP TABLE IF EXISTS services;
            ALTER TABLE services_new RENAME TO services;
            "#,
        )?;

        set_schema_version(&tx, 2)?;
        tx.commit()?;
    }

    if current < 3 {
        let tx = connection.transaction()?;
        tx.execute_batch(
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

        set_schema_version(&tx, 3)?;
        tx.commit()?;
    }

    if current < 4 {
        let tx = connection.transaction()?;
        tx.execute_batch(
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

        set_schema_version(&tx, 4)?;
        tx.commit()?;
    }

    if current < 5 {
        let tx = connection.transaction()?;
        tx.execute_batch(
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
        let mut stmt = tx.prepare("SELECT id, config_json FROM services")?;
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
                    tx.execute(
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

        set_schema_version(&tx, 5)?;
        tx.commit()?;
    }

    if current < 6 {
        let tx = connection.transaction()?;
        tx.execute_batch(
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
        set_schema_version(&tx, 6)?;
        tx.commit()?;
    }

    if current < 7 {
        let tx = connection.transaction()?;
        tx.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS system_settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            "#,
        )?;
        set_schema_version(&tx, 7)?;
        tx.commit()?;
    }

    if current < 8 {
        let tx = connection.transaction()?;
        tx.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS autoscale_desired (
                service_id TEXT PRIMARY KEY,
                desired_replicas INTEGER NOT NULL
            );
            "#,
        )?;
        set_schema_version(&tx, 8)?;
        tx.commit()?;
    }

    if current < 9 {
        let tx = connection.transaction()?;
        tx.execute_batch(
            r#"
            ALTER TABLE deployments ADD COLUMN artifact_digest TEXT;
            "#,
        )?;
        set_schema_version(&tx, 9)?;
        tx.commit()?;
    }

    if current < 10 {
        let tx = connection.transaction()?;
        // Track session creation time to enforce an absolute lifetime cap
        // (the sliding `expires_at` window alone never expires an active session).
        tx.execute_batch(
            r#"
            ALTER TABLE sessions ADD COLUMN created_at TEXT;
            "#,
        )?;
        let now = Utc::now().to_rfc3339();
        tx.execute(
            "UPDATE sessions SET created_at = ?1 WHERE created_at IS NULL",
            params![now],
        )?;
        set_schema_version(&tx, 10)?;
        tx.commit()?;
    }

    if current < 11 {
        let tx = connection.transaction()?;
        // Backfill: before demote-on-promote existed, every successful deploy
        // left its row `Healthy` forever, so services accumulated many green
        // rows. Demote every non-promoted `Healthy` deployment to `Inactive`
        // so each service shows exactly one live (promoted) deployment.
        super::deployments::demote_stale_healthy_deployments_q(&tx)?;
        set_schema_version(&tx, 11)?;
        tx.commit()?;
    }

    if current < 12 {
        let tx = connection.transaction()?;
        tx.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS hosted_repositories (
              id TEXT PRIMARY KEY,
              project_id TEXT NOT NULL,
              service_id TEXT NOT NULL,
              name TEXT NOT NULL,
              created_at TEXT NOT NULL,
              UNIQUE(project_id, service_id)
            );
            CREATE TABLE IF NOT EXISTS hosted_manifests (
              repository_id TEXT NOT NULL,
              digest TEXT NOT NULL,
              media_type TEXT NOT NULL,
              size INTEGER NOT NULL,
              created_at TEXT NOT NULL,
              PRIMARY KEY(repository_id, digest)
            );
            CREATE TABLE IF NOT EXISTS hosted_tags (
              repository_id TEXT NOT NULL,
              tag TEXT NOT NULL,
              manifest_digest TEXT NOT NULL,
              updated_at TEXT NOT NULL,
              PRIMARY KEY(repository_id, tag)
            );
            CREATE TABLE IF NOT EXISTS hosted_blobs (
              repository_id TEXT NOT NULL,
              digest TEXT NOT NULL,
              size INTEGER NOT NULL,
              created_at TEXT NOT NULL,
              PRIMARY KEY(repository_id, digest)
            );
            CREATE TABLE IF NOT EXISTS hosted_uploads (
              id TEXT PRIMARY KEY,
              repository_id TEXT NOT NULL,
              path TEXT NOT NULL,
              started_at TEXT NOT NULL,
              updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS hosted_registry_gc_runs (
              id TEXT PRIMARY KEY,
              status TEXT NOT NULL,
              scanned_blobs INTEGER NOT NULL,
              deleted_blobs INTEGER NOT NULL,
              deleted_bytes INTEGER NOT NULL,
              started_at TEXT NOT NULL,
              finished_at TEXT
            );
            "#,
        )?;
        set_schema_version(&tx, 12)?;
        tx.commit()?;
    }

    Ok(())
}

/// Write the single `schema_version` row to `version`. Run inside the same
/// transaction as the migration body so the version bump and the schema change
/// commit (or roll back) atomically. The `INSERT OR REPLACE` on the fixed
/// `id = 1` row, combined with the table's `CHECK (id = 1)`, keeps exactly one
/// row regardless of prior partial state.
fn set_schema_version(conn: &Connection, version: i64) -> Result<(), RepoError> {
    conn.execute(
        "INSERT INTO schema_version (id, version) VALUES (1, ?1)
         ON CONFLICT(id) DO UPDATE SET version = excluded.version",
        params![version],
    )?;
    Ok(())
}

/// Collapse a legacy `schema_version` table (created before the single-row
/// `id` CHECK existed) into the canonical `id = 1` single-row form.
///
/// Older databases have a `schema_version(version INTEGER)` table with no `id`
/// column and potentially multiple rows (the pre-fix `DELETE`+`INSERT` bump
/// could leave duplicates after a partial failure). This rebuilds it as
/// `schema_version(id PRIMARY KEY CHECK (id = 1), version)` carrying forward the
/// highest observed version. Runs in its own transaction so the rebuild is
/// atomic. A no-op on databases that already have the `id` column.
fn migrate_schema_version_table(connection: &mut Connection) -> Result<(), RepoError> {
    let has_id_column = connection
        .prepare("PRAGMA table_info(schema_version)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<String>, _>>()?
        .iter()
        .any(|name| name == "id");

    if has_id_column {
        return Ok(());
    }

    let tx = connection.transaction()?;
    let max_version: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_version",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    tx.execute_batch(
        r#"
        DROP TABLE schema_version;
        CREATE TABLE schema_version (
            id INTEGER PRIMARY KEY CHECK (id = 1),
            version INTEGER NOT NULL
        );
        "#,
    )?;
    set_schema_version(&tx, max_version)?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod migration_tests {
    use super::*;

    fn schema_version(pool: &SqlitePool) -> i64 {
        pool.connection()
            .unwrap()
            .query_row(
                "SELECT COALESCE((SELECT version FROM schema_version WHERE id = 1), 0)",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }

    #[test]
    fn run_migrations_reaches_v12_and_is_idempotent() {
        let pool = SqlitePool::open_in_memory().unwrap();
        run_migrations(&pool).unwrap();
        assert_eq!(schema_version(&pool), 12);
        // Re-running is a no-op (every `if current < N` is false) and never errors.
        run_migrations(&pool).unwrap();
        assert_eq!(schema_version(&pool), 12);
    }

    #[test]
    fn schema_version_table_holds_exactly_one_row() {
        let pool = SqlitePool::open_in_memory().unwrap();
        run_migrations(&pool).unwrap();
        let count: i64 = pool
            .connection()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM schema_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1, "schema_version must never accumulate rows");
        // The CHECK constraint forbids a second row.
        let conn = pool.connection().unwrap();
        let second = conn.execute(
            "INSERT INTO schema_version (id, version) VALUES (2, 99)",
            [],
        );
        assert!(second.is_err(), "CHECK (id = 1) must reject a second row");
    }

    /// Prove the per-step transaction semantics that make a crash mid-step
    /// safe: a schema change and its `schema_version` bump are written inside
    /// one transaction, and aborting before commit rolls *both* back atomically.
    /// This is exactly the "DROP/ADD COLUMN happened but version not yet bumped"
    /// window the HIGH finding was about — under transactions that window leaves
    /// the DB untouched, so the next boot re-runs the whole step cleanly.
    #[test]
    fn aborted_step_rolls_back_schema_and_version_together() {
        let mut conn = Connection::open_in_memory().unwrap();
        apply_sqlite_pragmas(&conn).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE schema_version (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                version INTEGER NOT NULL
            );
            INSERT INTO schema_version (id, version) VALUES (1, 8);
            CREATE TABLE deployments (id TEXT PRIMARY KEY);
            "#,
        )
        .unwrap();

        // Begin a step: ADD COLUMN + version bump, then abort (simulating crash
        // before commit) by dropping the transaction without committing.
        {
            let tx = conn.transaction().unwrap();
            tx.execute_batch("ALTER TABLE deployments ADD COLUMN artifact_digest TEXT;")
                .unwrap();
            set_schema_version(&tx, 9).unwrap();
            // tx dropped here without commit() -> rollback.
        }

        // Schema change was rolled back: the column is gone.
        let has_col = conn
            .prepare("PRAGMA table_info(deployments)")
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<String>, _>>()
            .unwrap()
            .iter()
            .any(|c| c == "artifact_digest");
        assert!(!has_col, "aborted ADD COLUMN must roll back");

        // Version was rolled back too: still 8, not 9.
        let version: i64 = conn
            .query_row("SELECT version FROM schema_version WHERE id = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(version, 8, "aborted version bump must roll back");
    }

    #[test]
    fn legacy_schema_version_table_is_migrated_in_place() {
        let pool = SqlitePool::open_in_memory().unwrap();
        {
            // Build the pre-fix table shape: no `id` column, and (as a partial
            // failure could leave) multiple rows. Version 0 so the full ladder
            // still runs and builds every table from scratch.
            let conn = pool.connection().unwrap();
            conn.execute_batch(
                r#"
                CREATE TABLE schema_version (version INTEGER NOT NULL);
                INSERT INTO schema_version (version) VALUES (0);
                INSERT INTO schema_version (version) VALUES (0);
                "#,
            )
            .unwrap();
        }

        // run_migrations must collapse it to one row, then run the ladder to v12.
        run_migrations(&pool).unwrap();
        assert_eq!(schema_version(&pool), 12);
        let count: i64 = pool
            .connection()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM schema_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1, "legacy multi-row table collapsed to one row");
    }
}
