use std::sync::{Arc, Mutex};

use chrono::Utc;
use rusqlite::{Connection, params};
use thiserror::Error;
use uuid::Uuid;

use crate::{
    artifacts::ArtifactRecord,
    domain::{
        Credential, CredentialKind, Deployment, DeploymentRequest, DeploymentStatus, ServiceConfig,
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
            "#,
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
            INSERT INTO services (id, name, config_json)
            VALUES (?1, ?2, ?3)
            ON CONFLICT(name) DO UPDATE SET
                config_json = excluded.config_json
            "#,
            params![
                config.id.to_string(),
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
