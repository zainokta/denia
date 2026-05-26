//! Registry aggregate sqlite repo.
//!
//! Shared SQL lives in `*_q` free functions; both `SqliteStore` and
//! `SqliteRegistryRepo` delegate.

use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::domain::{Registry, ServiceConfig};
use crate::repo::error::RepoError;
use crate::repo::registry_repo::RegistryRepo;
use crate::repo::sqlite::pool::SqlitePool;
use crate::state::{SqliteStore, StateError};

pub(super) fn create_registry_q(conn: &Connection, registry: &Registry) -> Result<(), RepoError> {
    conn.execute(
        "INSERT INTO registries (id, project_id, name, config_json) VALUES (?1, ?2, ?3, ?4)",
        params![
            registry.id.to_string(),
            registry.project_id.to_string(),
            registry.name,
            serde_json::to_string(registry)?,
        ],
    )?;
    Ok(())
}

pub(super) fn update_registry_q(conn: &Connection, registry: &Registry) -> Result<(), RepoError> {
    let n = conn.execute(
        "UPDATE registries SET name = ?2, config_json = ?3 WHERE id = ?1",
        params![
            registry.id.to_string(),
            registry.name,
            serde_json::to_string(registry)?,
        ],
    )?;
    if n == 0 {
        return Err(RepoError::RegistryNotFound);
    }
    Ok(())
}

pub(super) fn registry_q(conn: &Connection, id: Uuid) -> Result<Option<Registry>, RepoError> {
    let json: Option<String> = conn
        .query_row(
            "SELECT config_json FROM registries WHERE id = ?1",
            params![id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    json.map(|j| serde_json::from_str(&j))
        .transpose()
        .map_err(Into::into)
}

pub(super) fn registries_for_project_q(
    conn: &Connection,
    project_id: Uuid,
) -> Result<Vec<Registry>, RepoError> {
    let mut stmt =
        conn.prepare("SELECT config_json FROM registries WHERE project_id = ?1 ORDER BY name")?;
    let rows = stmt.query_map(params![project_id.to_string()], |row| {
        row.get::<_, String>(0)
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(serde_json::from_str(&row?)?);
    }
    Ok(out)
}

pub(super) fn delete_registry_q(conn: &Connection, id: Uuid) -> Result<(), RepoError> {
    let json: Option<String> = conn
        .query_row(
            "SELECT config_json FROM registries WHERE id = ?1",
            params![id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    let registry: Registry = match json {
        Some(j) => serde_json::from_str(&j)?,
        None => return Err(RepoError::RegistryNotFound),
    };
    let mut stmt = conn.prepare("SELECT config_json FROM services WHERE project_id = ?1")?;
    let rows = stmt.query_map(params![registry.project_id.to_string()], |row| {
        row.get::<_, String>(0)
    })?;
    for row in rows {
        let svc: ServiceConfig = serde_json::from_str(&row?)?;
        if let crate::domain::ServiceSource::ExternalImage(src) = &svc.source
            && src.registry_id == Some(id)
        {
            return Err(RepoError::RegistryInUse);
        }
    }
    drop(stmt);
    conn.execute(
        "DELETE FROM registries WHERE id = ?1",
        params![id.to_string()],
    )?;
    Ok(())
}

impl SqliteStore {
    pub fn create_registry(&self, registry: &Registry) -> Result<(), StateError> {
        let connection = self.connection()?;
        create_registry_q(&connection, registry).map_err(StateError::from)
    }

    pub fn update_registry(&self, registry: &Registry) -> Result<(), StateError> {
        let connection = self.connection()?;
        update_registry_q(&connection, registry).map_err(StateError::from)
    }

    pub fn registry(&self, id: Uuid) -> Result<Option<Registry>, StateError> {
        let connection = self.connection()?;
        registry_q(&connection, id).map_err(StateError::from)
    }

    pub fn registries_for_project(&self, project_id: Uuid) -> Result<Vec<Registry>, StateError> {
        let connection = self.connection()?;
        registries_for_project_q(&connection, project_id).map_err(StateError::from)
    }

    pub fn delete_registry(&self, id: Uuid) -> Result<(), StateError> {
        let connection = self.connection()?;
        delete_registry_q(&connection, id).map_err(StateError::from)
    }
}

pub struct SqliteRegistryRepo {
    pool: SqlitePool,
}

impl SqliteRegistryRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl RegistryRepo for SqliteRegistryRepo {
    fn create_registry(&self, registry: &Registry) -> Result<(), RepoError> {
        let conn = self.pool.connection()?;
        create_registry_q(&conn, registry)
    }

    fn update_registry(&self, registry: &Registry) -> Result<(), RepoError> {
        let conn = self.pool.connection()?;
        update_registry_q(&conn, registry)
    }

    fn registry(&self, id: Uuid) -> Result<Option<Registry>, RepoError> {
        let conn = self.pool.connection()?;
        registry_q(&conn, id)
    }

    fn registries_for_project(&self, project_id: Uuid) -> Result<Vec<Registry>, RepoError> {
        let conn = self.pool.connection()?;
        registries_for_project_q(&conn, project_id)
    }

    fn delete_registry(&self, id: Uuid) -> Result<(), RepoError> {
        let conn = self.pool.connection()?;
        delete_registry_q(&conn, id)
    }
}
