use std::sync::{Arc, Mutex};

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    artifacts::ArtifactRecord,
    domain::{
        ApiToken, Credential, CredentialKind, Deployment, DeploymentRequest, DeploymentStatus,
        Project, ProjectMembership, Role, ServiceConfig, Session, User,
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
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error("cannot delete the last super admin")]
    LastSuperAdmin,
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

    pub fn create_user(
        &self,
        username: &str,
        password_hash: &str,
        is_super_admin: bool,
    ) -> Result<User, StateError> {
        let user =
            User::new(username, password_hash.to_string(), is_super_admin).map_err(|_| {
                StateError::Json(serde_json::Error::io(std::io::Error::other("domain error")))
            })?;
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO users (id, username, password_hash, is_super_admin, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                user.id.to_string(),
                user.username,
                user.password_hash,
                user.is_super_admin as i32,
                user.created_at.to_rfc3339(),
            ],
        )?;
        Ok(user)
    }

    pub fn get_user(&self, user_id: Uuid) -> Result<Option<User>, StateError> {
        let connection = self.connection()?;
        let result: Option<(String, String, String, bool, String)> = connection
            .query_row(
                "SELECT id, username, password_hash, is_super_admin, created_at FROM users WHERE id = ?1",
                params![user_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .optional()?;
        result
            .map(
                |(id, username, password_hash, is_super_admin, created_at)| {
                    Ok(User {
                        id: Uuid::parse_str(&id)?,
                        username,
                        password_hash,
                        is_super_admin,
                        created_at: created_at.parse()?,
                    })
                },
            )
            .transpose()
    }

    pub fn list_users(&self) -> Result<Vec<User>, StateError> {
        let connection = self.connection()?;
        let mut stmt = connection.prepare(
            "SELECT id, username, password_hash, is_super_admin, created_at FROM users ORDER BY username",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, bool>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        let mut users = Vec::new();
        for row in rows {
            let (id, username, password_hash, is_super_admin, created_at) = row?;
            users.push(User {
                id: Uuid::parse_str(&id)?,
                username,
                password_hash,
                is_super_admin,
                created_at: created_at.parse()?,
            });
        }
        Ok(users)
    }

    pub fn delete_user(&self, user_id: Uuid) -> Result<(), StateError> {
        let connection = self.connection()?;
        let super_count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM users WHERE is_super_admin = 1",
            [],
            |row| row.get(0),
        )?;
        let target_is_super: bool = connection.query_row(
            "SELECT is_super_admin FROM users WHERE id = ?1",
            params![user_id.to_string()],
            |row| row.get(0),
        )?;
        if super_count <= 1 && target_is_super {
            return Err(StateError::LastSuperAdmin);
        }
        connection.execute(
            "DELETE FROM sessions WHERE user_id = ?1",
            params![user_id.to_string()],
        )?;
        connection.execute(
            "DELETE FROM api_tokens WHERE user_id = ?1",
            params![user_id.to_string()],
        )?;
        connection.execute(
            "DELETE FROM project_members WHERE user_id = ?1",
            params![user_id.to_string()],
        )?;
        connection.execute(
            "DELETE FROM users WHERE id = ?1",
            params![user_id.to_string()],
        )?;
        Ok(())
    }

    pub fn verify_login(&self, username: &str, password: &str) -> Result<User, StateError> {
        let connection = self.connection()?;
        let result: Option<(String, String, String, bool, String)> = connection
            .query_row(
                "SELECT id, username, password_hash, is_super_admin, created_at FROM users WHERE username = ?1",
                params![username],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .optional()?;
        match result {
            Some((id, username, password_hash, is_super_admin, created_at)) => {
                if crate::auth::verify_password(&password_hash, password) {
                    Ok(User {
                        id: Uuid::parse_str(&id)?,
                        username,
                        password_hash,
                        is_super_admin,
                        created_at: created_at.parse()?,
                    })
                } else {
                    Err(StateError::InvalidCredentials)
                }
            }
            None => Err(StateError::InvalidCredentials),
        }
    }

    pub fn create_session(&self, user_id: Uuid, ttl_hours: i64) -> Result<Session, StateError> {
        let token = crate::auth::generate_token();
        let token_hash = crate::auth::hash_token(&token);
        let expires_at = Utc::now() + chrono::TimeDelta::hours(ttl_hours);
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO sessions (token_hash, user_id, expires_at) VALUES (?1, ?2, ?3)",
            params![&token_hash, user_id.to_string(), expires_at.to_rfc3339()],
        )?;
        Ok(Session {
            token_hash: token,
            user_id,
            expires_at,
        })
    }

    pub fn user_for_session(&self, token_hash: &str) -> Result<Option<User>, StateError> {
        let connection = self.connection()?;
        let result: Option<(String, String)> = connection
            .query_row(
                "SELECT user_id, expires_at FROM sessions WHERE token_hash = ?1",
                params![token_hash],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let (user_id_str, expires_at_str) = match result {
            Some(r) => r,
            None => return Ok(None),
        };
        let expires_at: chrono::DateTime<Utc> = expires_at_str.parse()?;
        if Utc::now() > expires_at {
            connection.execute(
                "DELETE FROM sessions WHERE token_hash = ?1",
                params![token_hash],
            )?;
            return Ok(None);
        }
        let user_id = Uuid::parse_str(&user_id_str)?;
        self.get_user(user_id)
    }

    pub fn delete_session(&self, token_hash: &str) -> Result<(), StateError> {
        let connection = self.connection()?;
        connection.execute(
            "DELETE FROM sessions WHERE token_hash = ?1",
            params![token_hash],
        )?;
        Ok(())
    }

    pub fn create_api_token(&self, user_id: Uuid, name: &str) -> Result<ApiToken, StateError> {
        let token = crate::auth::generate_token();
        let api_token = ApiToken {
            id: Uuid::now_v7(),
            user_id,
            name: name.to_string(),
            token_hash: token,
            created_at: Utc::now(),
        };
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO api_tokens (id, user_id, name, token_hash, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                api_token.id.to_string(),
                user_id.to_string(),
                api_token.name,
                crate::auth::hash_token(&api_token.token_hash),
            api_token.created_at.to_rfc3339(),
            ],
        )?;
        Ok(api_token)
    }

    pub fn user_for_api_token(&self, token_hash: &str) -> Result<Option<User>, StateError> {
        let connection = self.connection()?;
        let result: Option<String> = connection
            .query_row(
                "SELECT user_id FROM api_tokens WHERE token_hash = ?1",
                params![token_hash],
                |row| row.get(0),
            )
            .optional()?;
        let user_id_str = match result {
            Some(id) => id,
            None => return Ok(None),
        };
        self.get_user(Uuid::parse_str(&user_id_str)?)
    }

    pub fn list_api_tokens(&self, user_id: Uuid) -> Result<Vec<ApiToken>, StateError> {
        let connection = self.connection()?;
        let mut stmt = connection.prepare(
            "SELECT id, user_id, name, token_hash, created_at FROM api_tokens WHERE user_id = ?1 ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(params![user_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?;
        let mut tokens = Vec::new();
        for row in rows {
            let (id, uid, name, th, created_at) = row?;
            tokens.push(ApiToken {
                id: Uuid::parse_str(&id)?,
                user_id: Uuid::parse_str(&uid)?,
                name,
                token_hash: th,
                created_at: created_at.parse()?,
            });
        }
        Ok(tokens)
    }

    pub fn revoke_api_token(&self, token_id: Uuid) -> Result<(), StateError> {
        let connection = self.connection()?;
        connection.execute(
            "DELETE FROM api_tokens WHERE id = ?1",
            params![token_id.to_string()],
        )?;
        Ok(())
    }

    pub fn set_membership(
        &self,
        user_id: Uuid,
        project_id: Uuid,
        role: Role,
    ) -> Result<(), StateError> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT OR REPLACE INTO project_members (user_id, project_id, role) VALUES (?1, ?2, ?3)",
            params![
                user_id.to_string(),
                project_id.to_string(),
                serde_json::to_string(&role)?,
            ],
        )?;
        Ok(())
    }

    pub fn role_for(&self, user_id: Uuid, project_id: Uuid) -> Result<Option<Role>, StateError> {
        let connection = self.connection()?;
        let role_str: Option<String> = connection
            .query_row(
                "SELECT role FROM project_members WHERE user_id = ?1 AND project_id = ?2",
                params![user_id.to_string(), project_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        role_str
            .map(|r| serde_json::from_str(&r))
            .transpose()
            .map_err(Into::into)
    }

    pub fn list_members(&self, project_id: Uuid) -> Result<Vec<ProjectMembership>, StateError> {
        let connection = self.connection()?;
        let mut stmt = connection.prepare(
            "SELECT user_id, project_id, role FROM project_members WHERE project_id = ?1",
        )?;
        let rows = stmt.query_map(params![project_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut members = Vec::new();
        for row in rows {
            let (uid, pid, role_str) = row?;
            members.push(ProjectMembership {
                user_id: Uuid::parse_str(&uid)?,
                project_id: Uuid::parse_str(&pid)?,
                role: serde_json::from_str(&role_str)?,
            });
        }
        Ok(members)
    }

    pub fn remove_membership(&self, user_id: Uuid, project_id: Uuid) -> Result<(), StateError> {
        let connection = self.connection()?;
        connection.execute(
            "DELETE FROM project_members WHERE user_id = ?1 AND project_id = ?2",
            params![user_id.to_string(), project_id.to_string()],
        )?;
        Ok(())
    }

    pub fn list_memberships_for_user(
        &self,
        user_id: Uuid,
    ) -> Result<Vec<ProjectMembership>, StateError> {
        let connection = self.connection()?;
        let mut stmt = connection
            .prepare("SELECT user_id, project_id, role FROM project_members WHERE user_id = ?1")?;
        let rows = stmt.query_map(params![user_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let mut members = Vec::new();
        for row in rows {
            let (uid, pid, role_str) = row?;
            members.push(ProjectMembership {
                user_id: Uuid::parse_str(&uid)?,
                project_id: Uuid::parse_str(&pid)?,
                role: serde_json::from_str(&role_str)?,
            });
        }
        Ok(members)
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
