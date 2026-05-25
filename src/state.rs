use std::sync::{Arc, Mutex};

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    artifacts::ArtifactRecord,
    domain::{
        Credential, CredentialKind, Deployment, DeploymentRequest, DeploymentStatus, Project,
        ServiceConfig,
    },
    secrets::SecretRef,
};

#[derive(Debug, Error)]
pub enum StateError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("uuid error: {0}")]
    Uuid(#[from] uuid::Error),
    #[error("time parse error: {0}")]
    Time(#[from] chrono::ParseError),
    #[error("state lock poisoned")]
    LockPoisoned,
    #[error("cannot delete project with existing services")]
    ProjectNotEmpty,
    #[error("project not found")]
    UnknownProject,
}

#[derive(Clone)]
pub struct SqliteStore {
    connection: Arc<Mutex<Connection>>,
}

impl SqliteStore {
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self, StateError> {
        Ok(Self {
            connection: Arc::new(Mutex::new(Connection::open(path)?)),
        })
    }

    pub fn open_in_memory() -> Result<Self, StateError> {
        Ok(Self {
            connection: Arc::new(Mutex::new(Connection::open_in_memory()?)),
        })
    }

    pub fn migrate(&self) -> Result<(), StateError> {
        let connection = self.connection()?;

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
                    params![&default_id, "default", serde_json::to_string(&default_project.description)?, &default_json],
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

        Ok(())
    }

    pub fn schema_version(&self) -> Result<i64, StateError> {
        let connection = self.connection()?;
        let v = connection
            .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
            .unwrap_or(0);
        Ok(v)
    }

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

    pub fn put_credential(
        &self,
        name: impl Into<String>,
        kind: CredentialKind,
        secret_ref: SecretRef,
    ) -> Result<Credential, StateError> {
        let credential = Credential {
            id: Uuid::now_v7(),
            name: name.into(),
            kind,
            secret_ref,
        };
        let connection = self.connection()?;
        connection.execute(
            r#"
            INSERT INTO credentials (id, name, kind, secret_ref)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(name) DO UPDATE SET
                kind = excluded.kind,
                secret_ref = excluded.secret_ref
            "#,
            params![
                credential.id.to_string(),
                credential.name,
                serde_json::to_string(&credential.kind)?,
                credential.secret_ref.as_str(),
            ],
        )?;
        Ok(credential)
    }

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

    pub fn create_deployment(&self, request: DeploymentRequest) -> Result<Deployment, StateError> {
        let deployment = Deployment {
            id: Uuid::now_v7(),
            service_id: request.service_id(),
            request,
            status: DeploymentStatus::Pending,
            created_at: Utc::now(),
        };
        let connection = self.connection()?;
        connection.execute(
            r#"
            INSERT INTO deployments (id, service_id, request_json, status, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
            params![
                deployment.id.to_string(),
                deployment.service_id.to_string(),
                serde_json::to_string(&deployment.request)?,
                serde_json::to_string(&deployment.status)?,
                deployment.created_at.to_rfc3339(),
            ],
        )?;
        Ok(deployment)
    }

    pub fn list_deployments(&self, service_id: Uuid) -> Result<Vec<Deployment>, StateError> {
        let connection = self.connection()?;
        let mut statement = connection.prepare(
            r#"
            SELECT id, service_id, request_json, status, created_at
            FROM deployments
            WHERE service_id = ?1
            ORDER BY created_at DESC
            "#,
        )?;
        let rows = statement.query_map(params![service_id.to_string()], |row| {
            Ok(DeploymentRow {
                id: row.get(0)?,
                service_id: row.get(1)?,
                request_json: row.get(2)?,
                status_json: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?;

        let mut deployments = Vec::new();
        for row in rows {
            let row = row?;
            deployments.push(Deployment {
                id: Uuid::parse_str(&row.id)?,
                service_id: Uuid::parse_str(&row.service_id)?,
                request: serde_json::from_str(&row.request_json)?,
                status: serde_json::from_str(&row.status_json)?,
                created_at: row.created_at.parse()?,
            });
        }
        Ok(deployments)
    }

    pub fn update_deployment_status(
        &self,
        deployment_id: Uuid,
        status: DeploymentStatus,
    ) -> Result<(), StateError> {
        let connection = self.connection()?;
        connection.execute(
            "UPDATE deployments SET status = ?1 WHERE id = ?2",
            params![serde_json::to_string(&status)?, deployment_id.to_string(),],
        )?;
        Ok(())
    }

    pub fn promote_deployment(
        &self,
        service_id: Uuid,
        deployment_id: Uuid,
    ) -> Result<(), StateError> {
        let connection = self.connection()?;
        connection.execute(
            r#"
            INSERT INTO promoted_deployments (service_id, deployment_id)
            VALUES (?1, ?2)
            ON CONFLICT(service_id) DO UPDATE SET
                deployment_id = excluded.deployment_id
            "#,
            params![service_id.to_string(), deployment_id.to_string()],
        )?;
        Ok(())
    }

    pub fn promoted_deployment(&self, service_id: Uuid) -> Result<Option<Uuid>, StateError> {
        let connection = self.connection()?;
        let value: Option<String> = connection
            .query_row(
                "SELECT deployment_id FROM promoted_deployments WHERE service_id = ?1",
                params![service_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        value
            .map(|id| Uuid::parse_str(&id))
            .transpose()
            .map_err(Into::into)
    }

    pub fn clear_promoted_deployment(&self, service_id: Uuid) -> Result<(), StateError> {
        let connection = self.connection()?;
        connection.execute(
            "DELETE FROM promoted_deployments WHERE service_id = ?1",
            params![service_id.to_string()],
        )?;
        Ok(())
    }

    pub fn put_artifact(&self, artifact: ArtifactRecord) -> Result<ArtifactRecord, StateError> {
        let connection = self.connection()?;
        connection.execute(
            r#"
            INSERT INTO artifacts (id, digest, record_json, created_at)
            VALUES (?1, ?2, ?3, ?4)
            ON CONFLICT(digest) DO UPDATE SET
                record_json = excluded.record_json
            "#,
            params![
                artifact.id.to_string(),
                artifact.digest,
                serde_json::to_string(&artifact)?,
                artifact.created_at.to_rfc3339(),
            ],
        )?;
        Ok(artifact)
    }

    pub fn list_artifacts(&self) -> Result<Vec<ArtifactRecord>, StateError> {
        let connection = self.connection()?;
        let mut statement =
            connection.prepare("SELECT record_json FROM artifacts ORDER BY created_at DESC")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut artifacts = Vec::new();
        for row in rows {
            artifacts.push(serde_json::from_str(&row?)?);
        }
        Ok(artifacts)
    }

    fn connection(&self) -> Result<std::sync::MutexGuard<'_, Connection>, StateError> {
        self.connection.lock().map_err(|_| StateError::LockPoisoned)
    }
}

struct DeploymentRow {
    id: String,
    service_id: String,
    request_json: String,
    status_json: String,
    created_at: String,
}
