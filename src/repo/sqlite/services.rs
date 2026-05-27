//! Service aggregate sqlite repo.
//!
//! Holds the SQL once in `*_q` free functions. Both `SqliteStore::*` (the
//! pre-Task-10 facade) and `SqliteServiceRepo` (the trait impl introduced by
//! Task 9) delegate to those functions so bodies are never duplicated.

use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::domain::ServiceConfig;
use crate::repo::error::RepoError;
use crate::repo::sqlite::pool::SqlitePool;
use crate::state::{SqliteStore, StateError};

pub(super) fn put_service_q(conn: &Connection, config: &ServiceConfig) -> Result<(), RepoError> {
    conn.execute(
        r#"
            INSERT INTO services (id, project_id, name, config_json)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(project_id, name) DO UPDATE SET
                config_json = excluded.config_json
            "#,
        params![
            config.id.to_string(),
            config.project_id.to_string(),
            config.name,
            serde_json::to_string(config)?
        ],
    )?;
    Ok(())
}

pub(super) fn list_services_q(conn: &Connection) -> Result<Vec<ServiceConfig>, RepoError> {
    let mut statement = conn.prepare("SELECT config_json FROM services ORDER BY name")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut services = Vec::new();
    for row in rows {
        services.push(serde_json::from_str(&row?)?);
    }
    Ok(services)
}

pub(super) fn get_service_q(
    conn: &Connection,
    service_id: Uuid,
) -> Result<Option<ServiceConfig>, RepoError> {
    let value: Option<String> = conn
        .query_row(
            "SELECT config_json FROM services WHERE id = ?1",
            params![service_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    value
        .map(|json| serde_json::from_str(&json))
        .transpose()
        .map_err(Into::into)
}

impl SqliteStore {
    pub fn put_service(&self, config: ServiceConfig) -> Result<ServiceConfig, StateError> {
        let connection = self.connection()?;
        put_service_q(&connection, &config).map_err(StateError::from)?;
        Ok(config)
    }

    pub fn list_services(&self) -> Result<Vec<ServiceConfig>, StateError> {
        let connection = self.connection()?;
        list_services_q(&connection).map_err(StateError::from)
    }

    pub fn get_service(&self, service_id: Uuid) -> Result<Option<ServiceConfig>, StateError> {
        let connection = self.connection()?;
        get_service_q(&connection, service_id).map_err(StateError::from)
    }
}

#[derive(Clone)]
pub struct SqliteServiceRepo {
    pool: SqlitePool,
}

impl SqliteServiceRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl SqliteServiceRepo {
    pub fn put_service(&self, config: ServiceConfig) -> Result<ServiceConfig, RepoError> {
        let conn = self.pool.connection()?;
        put_service_q(&conn, &config)?;
        Ok(config)
    }

    pub fn list_services(&self) -> Result<Vec<ServiceConfig>, RepoError> {
        let conn = self.pool.connection()?;
        list_services_q(&conn)
    }

    pub fn get_service(&self, service_id: Uuid) -> Result<Option<ServiceConfig>, RepoError> {
        let conn = self.pool.connection()?;
        get_service_q(&conn, service_id)
    }
}
