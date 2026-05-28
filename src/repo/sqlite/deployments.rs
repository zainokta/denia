//! Deployment + artifact aggregate sqlite repo.
//!
//! Shared SQL lives in `*_q` free functions; both `SqliteStore` and
//! `SqliteDeploymentRepo` delegate.

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::artifacts::ArtifactRecord;
use crate::domain::{Deployment, DeploymentRequest, DeploymentStatus};
use crate::repo::error::RepoError;
use crate::repo::sqlite::pool::SqlitePool;
use crate::state::{SqliteStore, StateError};

struct DeploymentRow {
    id: String,
    service_id: String,
    request_json: String,
    status_json: String,
    created_at: String,
}

pub(super) fn create_deployment_q(
    conn: &Connection,
    request: DeploymentRequest,
) -> Result<Deployment, RepoError> {
    let deployment = Deployment {
        id: Uuid::now_v7(),
        service_id: request.service_id(),
        request,
        status: DeploymentStatus::Pending,
        created_at: Utc::now(),
    };
    conn.execute(
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

pub(super) fn list_deployments_q(
    conn: &Connection,
    service_id: Uuid,
) -> Result<Vec<Deployment>, RepoError> {
    let mut statement = conn.prepare(
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

pub(super) fn update_deployment_status_q(
    conn: &Connection,
    deployment_id: Uuid,
    status: DeploymentStatus,
) -> Result<(), RepoError> {
    conn.execute(
        "UPDATE deployments SET status = ?1 WHERE id = ?2",
        params![serde_json::to_string(&status)?, deployment_id.to_string(),],
    )?;
    Ok(())
}

pub(super) fn fail_orphan_deployments_q(conn: &Connection) -> Result<Vec<Uuid>, RepoError> {
    let pending = serde_json::to_string(&DeploymentStatus::Pending)?;
    let building = serde_json::to_string(&DeploymentStatus::Building)?;
    let starting = serde_json::to_string(&DeploymentStatus::Starting)?;
    let failed = serde_json::to_string(&DeploymentStatus::Failed)?;

    let mut stmt =
        conn.prepare("SELECT id FROM deployments WHERE status IN (?1, ?2, ?3)")?;
    let id_strings: Vec<String> = stmt
        .query_map(params![&pending, &building, &starting], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let ids: Vec<Uuid> = id_strings
        .iter()
        .map(|s| Uuid::parse_str(s))
        .collect::<Result<Vec<_>, _>>()?;

    conn.execute(
        "UPDATE deployments SET status = ?1 WHERE status IN (?2, ?3, ?4)",
        params![&failed, &pending, &building, &starting],
    )?;
    Ok(ids)
}

pub(super) fn promote_deployment_q(
    conn: &Connection,
    service_id: Uuid,
    deployment_id: Uuid,
) -> Result<(), RepoError> {
    conn.execute(
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

pub(super) fn promoted_deployment_q(
    conn: &Connection,
    service_id: Uuid,
) -> Result<Option<Uuid>, RepoError> {
    let value: Option<String> = conn
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

pub(super) fn clear_promoted_deployment_q(
    conn: &Connection,
    service_id: Uuid,
) -> Result<(), RepoError> {
    conn.execute(
        "DELETE FROM promoted_deployments WHERE service_id = ?1",
        params![service_id.to_string()],
    )?;
    Ok(())
}

pub(super) fn put_artifact_q(
    conn: &Connection,
    artifact: ArtifactRecord,
) -> Result<ArtifactRecord, RepoError> {
    conn.execute(
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

pub(super) fn set_deployment_artifact_q(
    conn: &Connection,
    deployment_id: Uuid,
    digest: &str,
) -> Result<(), RepoError> {
    conn.execute(
        "UPDATE deployments SET artifact_digest = ?1 WHERE id = ?2",
        params![digest, deployment_id.to_string()],
    )?;
    Ok(())
}

pub(super) fn get_deployment_artifact_q(
    conn: &Connection,
    deployment_id: Uuid,
) -> Result<Option<ArtifactRecord>, RepoError> {
    let record_json: Option<String> = conn
        .query_row(
            r#"
            SELECT a.record_json FROM deployments d
            JOIN artifacts a ON a.digest = d.artifact_digest
            WHERE d.id = ?1
            "#,
            params![deployment_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    record_json
        .map(|json| serde_json::from_str(&json))
        .transpose()
        .map_err(Into::into)
}

pub(super) fn list_artifacts_q(conn: &Connection) -> Result<Vec<ArtifactRecord>, RepoError> {
    let mut statement =
        conn.prepare("SELECT record_json FROM artifacts ORDER BY created_at DESC")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut artifacts = Vec::new();
    for row in rows {
        artifacts.push(serde_json::from_str(&row?)?);
    }
    Ok(artifacts)
}

impl SqliteStore {
    pub fn create_deployment(&self, request: DeploymentRequest) -> Result<Deployment, StateError> {
        let connection = self.connection()?;
        create_deployment_q(&connection, request).map_err(StateError::from)
    }

    pub fn list_deployments(&self, service_id: Uuid) -> Result<Vec<Deployment>, StateError> {
        let connection = self.connection()?;
        list_deployments_q(&connection, service_id).map_err(StateError::from)
    }

    pub fn update_deployment_status(
        &self,
        deployment_id: Uuid,
        status: DeploymentStatus,
    ) -> Result<(), StateError> {
        let connection = self.connection()?;
        update_deployment_status_q(&connection, deployment_id, status).map_err(StateError::from)
    }

    pub fn fail_orphan_deployments(&self) -> Result<Vec<Uuid>, StateError> {
        let connection = self.connection()?;
        fail_orphan_deployments_q(&connection).map_err(StateError::from)
    }

    pub fn promote_deployment(
        &self,
        service_id: Uuid,
        deployment_id: Uuid,
    ) -> Result<(), StateError> {
        let connection = self.connection()?;
        promote_deployment_q(&connection, service_id, deployment_id).map_err(StateError::from)
    }

    pub fn promoted_deployment(&self, service_id: Uuid) -> Result<Option<Uuid>, StateError> {
        let connection = self.connection()?;
        promoted_deployment_q(&connection, service_id).map_err(StateError::from)
    }

    pub fn clear_promoted_deployment(&self, service_id: Uuid) -> Result<(), StateError> {
        let connection = self.connection()?;
        clear_promoted_deployment_q(&connection, service_id).map_err(StateError::from)
    }

    pub fn put_artifact(&self, artifact: ArtifactRecord) -> Result<ArtifactRecord, StateError> {
        let connection = self.connection()?;
        put_artifact_q(&connection, artifact).map_err(StateError::from)
    }

    pub fn set_deployment_artifact(
        &self,
        deployment_id: Uuid,
        digest: &str,
    ) -> Result<(), StateError> {
        let connection = self.connection()?;
        set_deployment_artifact_q(&connection, deployment_id, digest).map_err(StateError::from)
    }

    pub fn get_deployment_artifact(
        &self,
        deployment_id: Uuid,
    ) -> Result<Option<ArtifactRecord>, StateError> {
        let connection = self.connection()?;
        get_deployment_artifact_q(&connection, deployment_id).map_err(StateError::from)
    }

    pub fn list_artifacts(&self) -> Result<Vec<ArtifactRecord>, StateError> {
        let connection = self.connection()?;
        list_artifacts_q(&connection).map_err(StateError::from)
    }
}

#[derive(Clone)]
pub struct SqliteDeploymentRepo {
    pool: SqlitePool,
}

impl SqliteDeploymentRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl SqliteDeploymentRepo {
    pub fn create_deployment(&self, request: DeploymentRequest) -> Result<Deployment, RepoError> {
        let conn = self.pool.connection()?;
        create_deployment_q(&conn, request)
    }

    pub fn list_deployments(&self, service_id: Uuid) -> Result<Vec<Deployment>, RepoError> {
        let conn = self.pool.connection()?;
        list_deployments_q(&conn, service_id)
    }

    pub fn update_deployment_status(
        &self,
        deployment_id: Uuid,
        status: DeploymentStatus,
    ) -> Result<(), RepoError> {
        let conn = self.pool.connection()?;
        update_deployment_status_q(&conn, deployment_id, status)
    }

    pub fn promote_deployment(
        &self,
        service_id: Uuid,
        deployment_id: Uuid,
    ) -> Result<(), RepoError> {
        let conn = self.pool.connection()?;
        promote_deployment_q(&conn, service_id, deployment_id)
    }

    pub fn promoted_deployment(&self, service_id: Uuid) -> Result<Option<Uuid>, RepoError> {
        let conn = self.pool.connection()?;
        promoted_deployment_q(&conn, service_id)
    }

    pub fn clear_promoted_deployment(&self, service_id: Uuid) -> Result<(), RepoError> {
        let conn = self.pool.connection()?;
        clear_promoted_deployment_q(&conn, service_id)
    }

    pub fn put_artifact(&self, artifact: ArtifactRecord) -> Result<ArtifactRecord, RepoError> {
        let conn = self.pool.connection()?;
        put_artifact_q(&conn, artifact)
    }

    pub fn set_deployment_artifact(
        &self,
        deployment_id: Uuid,
        digest: &str,
    ) -> Result<(), RepoError> {
        let conn = self.pool.connection()?;
        set_deployment_artifact_q(&conn, deployment_id, digest)
    }

    pub fn get_deployment_artifact(
        &self,
        deployment_id: Uuid,
    ) -> Result<Option<ArtifactRecord>, RepoError> {
        let conn = self.pool.connection()?;
        get_deployment_artifact_q(&conn, deployment_id)
    }

    pub fn list_artifacts(&self) -> Result<Vec<ArtifactRecord>, RepoError> {
        let conn = self.pool.connection()?;
        list_artifacts_q(&conn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::{ArtifactKind, ArtifactSource};
    use crate::state::SqliteStore;

    #[test]
    fn deployment_artifact_link_round_trips() {
        let store = SqliteStore::open_in_memory().unwrap();
        store.migrate().unwrap();

        let service_id = Uuid::now_v7();
        let deployment = store
            .create_deployment(DeploymentRequest::external_image(service_id, "img"))
            .unwrap();

        let record = ArtifactRecord::new(
            "sha256:abc",
            ArtifactKind::RootfsBundle,
            ArtifactSource::ExternalRegistry {
                image: "img".to_string(),
            },
        )
        .unwrap();

        store.put_artifact(record.clone()).unwrap();
        store
            .set_deployment_artifact(deployment.id, &record.digest)
            .unwrap();

        let linked = store.get_deployment_artifact(deployment.id).unwrap();
        assert!(linked.is_some());
        assert_eq!(linked.unwrap().digest, record.digest);
    }

    #[test]
    fn get_deployment_artifact_none_when_unlinked() {
        let store = SqliteStore::open_in_memory().unwrap();
        store.migrate().unwrap();

        let service_id = Uuid::now_v7();
        let deployment = store
            .create_deployment(DeploymentRequest::external_image(service_id, "img"))
            .unwrap();

        assert!(
            store
                .get_deployment_artifact(deployment.id)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn fail_orphan_deployments_marks_in_flight_failed() {
        let store = SqliteStore::open_in_memory().unwrap();
        store.migrate().unwrap();

        let svc = Uuid::now_v7();
        let d_pending = store
            .create_deployment(DeploymentRequest::external_image(svc, "img"))
            .unwrap();
        let d_starting = store
            .create_deployment(DeploymentRequest::external_image(svc, "img"))
            .unwrap();
        store
            .update_deployment_status(d_starting.id, DeploymentStatus::Starting)
            .unwrap();
        let d_healthy = store
            .create_deployment(DeploymentRequest::external_image(svc, "img"))
            .unwrap();
        store
            .update_deployment_status(d_healthy.id, DeploymentStatus::Healthy)
            .unwrap();

        let ids = store.fail_orphan_deployments().unwrap();
        assert_eq!(ids.len(), 2, "two in-flight rows must be marked failed");
        assert!(ids.contains(&d_pending.id));
        assert!(ids.contains(&d_starting.id));

        let all = store.list_deployments(svc).unwrap();
        let by_id = |id: Uuid| all.iter().find(|d| d.id == id).unwrap().status.clone();
        assert_eq!(by_id(d_pending.id), DeploymentStatus::Failed);
        assert_eq!(by_id(d_starting.id), DeploymentStatus::Failed);
        assert_eq!(by_id(d_healthy.id), DeploymentStatus::Healthy);
    }
}
