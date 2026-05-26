//! `SqliteStore` impl block for deployment + artifact aggregate methods.

use chrono::Utc;
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use crate::artifacts::ArtifactRecord;
use crate::domain::{Deployment, DeploymentRequest, DeploymentStatus};
use crate::state::{SqliteStore, StateError};

struct DeploymentRow {
    id: String,
    service_id: String,
    request_json: String,
    status_json: String,
    created_at: String,
}

impl SqliteStore {
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
}
