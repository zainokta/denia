//! `SqliteStore` impl block for registry aggregate methods.

use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use crate::domain::{Registry, ServiceConfig};
use crate::state::{SqliteStore, StateError};

impl SqliteStore {
    pub fn create_registry(&self, registry: &Registry) -> Result<(), StateError> {
        let connection = self.connection()?;
        connection.execute(
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

    pub fn update_registry(&self, registry: &Registry) -> Result<(), StateError> {
        let connection = self.connection()?;
        let n = connection.execute(
            "UPDATE registries SET name = ?2, config_json = ?3 WHERE id = ?1",
            params![
                registry.id.to_string(),
                registry.name,
                serde_json::to_string(registry)?,
            ],
        )?;
        if n == 0 {
            return Err(StateError::RegistryNotFound);
        }
        Ok(())
    }

    pub fn registry(&self, id: Uuid) -> Result<Option<Registry>, StateError> {
        let connection = self.connection()?;
        let json: Option<String> = connection
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

    pub fn registries_for_project(&self, project_id: Uuid) -> Result<Vec<Registry>, StateError> {
        let connection = self.connection()?;
        let mut stmt = connection
            .prepare("SELECT config_json FROM registries WHERE project_id = ?1 ORDER BY name")?;
        let rows = stmt.query_map(params![project_id.to_string()], |row| {
            row.get::<_, String>(0)
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(serde_json::from_str(&row?)?);
        }
        Ok(out)
    }

    pub fn delete_registry(&self, id: Uuid) -> Result<(), StateError> {
        let connection = self.connection()?;
        let json: Option<String> = connection
            .query_row(
                "SELECT config_json FROM registries WHERE id = ?1",
                params![id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let registry: Registry = match json {
            Some(j) => serde_json::from_str(&j)?,
            None => return Err(StateError::RegistryNotFound),
        };
        let mut stmt =
            connection.prepare("SELECT config_json FROM services WHERE project_id = ?1")?;
        let rows = stmt.query_map(params![registry.project_id.to_string()], |row| {
            row.get::<_, String>(0)
        })?;
        for row in rows {
            let svc: ServiceConfig = serde_json::from_str(&row?)?;
            if let crate::domain::ServiceSource::ExternalImage(src) = &svc.source
                && src.registry_id == Some(id)
            {
                return Err(StateError::RegistryInUse);
            }
        }
        drop(stmt);
        connection.execute(
            "DELETE FROM registries WHERE id = ?1",
            params![id.to_string()],
        )?;
        Ok(())
    }
}
