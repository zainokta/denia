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
    let existing_id: Option<String> = conn
        .query_row(
            "SELECT id FROM services WHERE project_id = ?1 AND name = ?2",
            params![config.project_id.to_string(), config.name],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(existing_id) = existing_id
        && existing_id != config.id.to_string()
    {
        return Err(RepoError::InvalidColumn(
            "services.config_json.id does not match existing row id".to_string(),
        ));
    }
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
    let mut statement =
        conn.prepare("SELECT id, project_id, name, config_json FROM services ORDER BY name")?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    let mut services = Vec::new();
    for row in rows {
        let (id, project_id, name, json) = row?;
        services.push(parse_service_row(&id, &project_id, &name, &json)?);
    }
    Ok(services)
}

pub(super) fn get_service_q(
    conn: &Connection,
    service_id: Uuid,
) -> Result<Option<ServiceConfig>, RepoError> {
    let value: Option<(String, String, String, String)> = conn
        .query_row(
            "SELECT id, project_id, name, config_json FROM services WHERE id = ?1",
            params![service_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    value
        .map(|(id, project_id, name, json)| parse_service_row(&id, &project_id, &name, &json))
        .transpose()
}

pub(super) fn delete_service_q(conn: &Connection, service_id: Uuid) -> Result<(), RepoError> {
    conn.execute(
        "DELETE FROM services WHERE id = ?1",
        params![service_id.to_string()],
    )?;
    Ok(())
}

fn parse_service_row(
    id: &str,
    project_id: &str,
    name: &str,
    json: &str,
) -> Result<ServiceConfig, RepoError> {
    let row_id = Uuid::parse_str(id)?;
    let row_project_id = Uuid::parse_str(project_id)?;
    let service: ServiceConfig = serde_json::from_str(json)?;
    if service.id != row_id {
        return Err(RepoError::InvalidColumn(
            "services.config_json.id does not match row id".to_string(),
        ));
    }
    if service.project_id != row_project_id {
        return Err(RepoError::InvalidColumn(
            "services.config_json.project_id does not match row project_id".to_string(),
        ));
    }
    if service.name != name {
        return Err(RepoError::InvalidColumn(
            "services.config_json.name does not match row name".to_string(),
        ));
    }
    Ok(service)
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

    pub fn delete_service(&self, service_id: Uuid) -> Result<(), StateError> {
        let connection = self.connection()?;
        delete_service_q(&connection, service_id).map_err(StateError::from)
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

    pub fn delete_service(&self, service_id: Uuid) -> Result<(), RepoError> {
        let conn = self.pool.connection()?;
        delete_service_q(&conn, service_id)
    }
}
