//! Project aggregate sqlite repo.
//!
//! Shared SQL bodies live in `*_q` free functions; both `SqliteStore`'s methods
//! and `SqliteProjectRepo`'s trait impl delegate to them.

use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::domain::Project;
use crate::repo::error::RepoError;
use crate::repo::project_repo::ProjectRepo;
use crate::repo::sqlite::pool::SqlitePool;
use crate::state::{SqliteStore, StateError};

pub(super) fn default_project_id_q(conn: &Connection) -> Result<Uuid, RepoError> {
    let value: String = conn.query_row(
        "SELECT id FROM projects WHERE name = 'default'",
        [],
        |row| row.get(0),
    )?;
    Uuid::parse_str(&value).map_err(Into::into)
}

pub(super) fn put_project_q(conn: &Connection, project: &Project) -> Result<(), RepoError> {
    conn.execute(
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
            serde_json::to_string(project)?,
        ],
    )?;
    Ok(())
}

pub(super) fn get_project_q(
    conn: &Connection,
    project_id: Uuid,
) -> Result<Option<Project>, RepoError> {
    let value: Option<String> = conn
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

pub(super) fn list_projects_q(conn: &Connection) -> Result<Vec<Project>, RepoError> {
    let mut statement = conn.prepare("SELECT config_json FROM projects ORDER BY name")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut projects = Vec::new();
    for row in rows {
        projects.push(serde_json::from_str(&row?)?);
    }
    Ok(projects)
}

pub(super) fn count_services_in_project_q(
    conn: &Connection,
    project_id: Uuid,
) -> Result<i64, RepoError> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM services WHERE project_id = ?1",
        params![project_id.to_string()],
        |row| row.get(0),
    )?;
    Ok(count)
}

pub(super) fn delete_project_q(conn: &Connection, project_id: Uuid) -> Result<(), RepoError> {
    if count_services_in_project_q(conn, project_id)? > 0 {
        return Err(RepoError::ProjectNotEmpty);
    }
    conn.execute(
        "DELETE FROM projects WHERE id = ?1",
        params![project_id.to_string()],
    )?;
    Ok(())
}

impl SqliteStore {
    pub fn default_project_id(&self) -> Result<Uuid, StateError> {
        let connection = self.connection()?;
        default_project_id_q(&connection).map_err(StateError::from)
    }

    pub fn put_project(&self, project: Project) -> Result<Project, StateError> {
        let connection = self.connection()?;
        put_project_q(&connection, &project).map_err(StateError::from)?;
        Ok(project)
    }

    pub fn get_project(&self, project_id: Uuid) -> Result<Option<Project>, StateError> {
        let connection = self.connection()?;
        get_project_q(&connection, project_id).map_err(StateError::from)
    }

    pub fn list_projects(&self) -> Result<Vec<Project>, StateError> {
        let connection = self.connection()?;
        list_projects_q(&connection).map_err(StateError::from)
    }

    pub fn count_services_in_project(&self, project_id: Uuid) -> Result<i64, StateError> {
        let connection = self.connection()?;
        count_services_in_project_q(&connection, project_id).map_err(StateError::from)
    }

    pub fn delete_project(&self, project_id: Uuid) -> Result<(), StateError> {
        let connection = self.connection()?;
        delete_project_q(&connection, project_id).map_err(StateError::from)
    }
}

#[allow(dead_code)]
pub struct SqliteProjectRepo {
    pool: SqlitePool,
}

#[allow(dead_code)]
impl SqliteProjectRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl ProjectRepo for SqliteProjectRepo {
    fn default_project_id(&self) -> Result<Uuid, RepoError> {
        let conn = self.pool.connection()?;
        default_project_id_q(&conn)
    }

    fn put_project(&self, project: Project) -> Result<Project, RepoError> {
        let conn = self.pool.connection()?;
        put_project_q(&conn, &project)?;
        Ok(project)
    }

    fn get_project(&self, project_id: Uuid) -> Result<Option<Project>, RepoError> {
        let conn = self.pool.connection()?;
        get_project_q(&conn, project_id)
    }

    fn list_projects(&self) -> Result<Vec<Project>, RepoError> {
        let conn = self.pool.connection()?;
        list_projects_q(&conn)
    }

    fn count_services_in_project(&self, project_id: Uuid) -> Result<i64, RepoError> {
        let conn = self.pool.connection()?;
        count_services_in_project_q(&conn, project_id)
    }

    fn delete_project(&self, project_id: Uuid) -> Result<(), RepoError> {
        let conn = self.pool.connection()?;
        delete_project_q(&conn, project_id)
    }
}
