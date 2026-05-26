//! `SqliteStore` impl block for project aggregate methods.

use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use crate::domain::Project;
use crate::state::{SqliteStore, StateError};

impl SqliteStore {
    pub fn default_project_id(&self) -> Result<Uuid, StateError> {
        let connection = self.connection()?;
        let value: String = connection.query_row(
            "SELECT id FROM projects WHERE name = 'default'",
            [],
            |row| row.get(0),
        )?;
        Uuid::parse_str(&value).map_err(Into::into)
    }

    pub fn put_project(&self, project: Project) -> Result<Project, StateError> {
        let connection = self.connection()?;
        connection.execute(
            r#"
            INSERT INTO projects (id, name, description, config_json)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(name) DO UPDATE SET
                description = excluded.description,
                config_json = excluded.config_json
            "#,
            params![
                project.id.to_string(),
                project.name,
                serde_json::to_string(&project.description)?,
                serde_json::to_string(&project)?,
            ],
        )?;
        Ok(project)
    }

    pub fn get_project(&self, project_id: Uuid) -> Result<Option<Project>, StateError> {
        let connection = self.connection()?;
        let value: Option<String> = connection
            .query_row(
                "SELECT config_json FROM projects WHERE id = ?1",
                params![project_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        value
            .map(|json| serde_json::from_str(&json))
            .transpose()
            .map_err(Into::into)
    }

    pub fn list_projects(&self) -> Result<Vec<Project>, StateError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare("SELECT config_json FROM projects ORDER BY name")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut projects = Vec::new();
        for row in rows {
            projects.push(serde_json::from_str(&row?)?);
        }
        Ok(projects)
    }

    pub fn count_services_in_project(&self, project_id: Uuid) -> Result<i64, StateError> {
        let connection = self.connection()?;
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM services WHERE project_id = ?1",
            params![project_id.to_string()],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    pub fn delete_project(&self, project_id: Uuid) -> Result<(), StateError> {
        if self.count_services_in_project(project_id)? > 0 {
            return Err(StateError::ProjectNotEmpty);
        }
        let connection = self.connection()?;
        connection.execute(
            "DELETE FROM projects WHERE id = ?1",
            params![project_id.to_string()],
        )?;
        Ok(())
    }
}
