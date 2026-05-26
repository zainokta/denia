//! `SqliteStore` impl block for service aggregate methods.
//!
//! Split out of `state.rs` in Task 8. Bodies are byte-identical with the
//! original `state::SqliteStore` methods.

use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use crate::domain::ServiceConfig;
use crate::state::{SqliteStore, StateError};

impl SqliteStore {
    pub fn put_service(&self, config: ServiceConfig) -> Result<ServiceConfig, StateError> {
        let connection = self.connection()?;
        connection.execute(
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
                serde_json::to_string(&config)?
            ],
        )?;
        Ok(config)
    }

    pub fn list_services(&self) -> Result<Vec<ServiceConfig>, StateError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare("SELECT config_json FROM services ORDER BY name")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut services = Vec::new();
        for row in rows {
            services.push(serde_json::from_str(&row?)?);
        }
        Ok(services)
    }

    pub fn get_service(&self, service_id: Uuid) -> Result<Option<ServiceConfig>, StateError> {
        let connection = self.connection()?;
        let value: Option<String> = connection
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
}
