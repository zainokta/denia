//! `SqliteStore` impl block for service-domain aggregate methods.

use chrono::Utc;
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use crate::domain::{DomainStatus, ServiceDomain};
use crate::state::{SqliteStore, StateError};

impl SqliteStore {
    pub fn put_service_domain(&self, d: &ServiceDomain) -> Result<(), StateError> {
        let connection = self.connection()?;
        connection.execute(
            r#"
            INSERT INTO service_domains
                (id, service_id, hostname, status, challenge_token,
                 verified_at, last_check_at, last_error, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            "#,
            params![
                d.id.to_string(),
                d.service_id.to_string(),
                d.hostname,
                status_str(d.status),
                d.challenge_token,
                d.verified_at.map(|t| t.to_rfc3339()),
                d.last_check_at.map(|t| t.to_rfc3339()),
                d.last_error,
                d.created_at.to_rfc3339(),
            ],
        )?;
        Ok(())
    }

    pub fn get_service_domain(&self, id: Uuid) -> Result<Option<ServiceDomain>, StateError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT id, service_id, hostname, status, challenge_token, verified_at, last_check_at, last_error, created_at FROM service_domains WHERE id = ?1",
                params![id.to_string()],
                row_to_service_domain,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn get_service_domain_by_token(
        &self,
        token: &str,
    ) -> Result<Option<ServiceDomain>, StateError> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT id, service_id, hostname, status, challenge_token, verified_at, last_check_at, last_error, created_at FROM service_domains WHERE challenge_token = ?1",
                params![token],
                row_to_service_domain,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list_service_domains_by_service(
        &self,
        service_id: Uuid,
    ) -> Result<Vec<ServiceDomain>, StateError> {
        let connection = self.connection()?;
        let mut stmt = connection.prepare(
            "SELECT id, service_id, hostname, status, challenge_token, verified_at, last_check_at, last_error, created_at FROM service_domains WHERE service_id = ?1 ORDER BY created_at",
        )?;
        let rows = stmt.query_map(params![service_id.to_string()], row_to_service_domain)?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    pub fn update_service_domain_status(
        &self,
        id: Uuid,
        status: DomainStatus,
        verified_at: Option<chrono::DateTime<Utc>>,
        last_error: Option<String>,
    ) -> Result<(), StateError> {
        let connection = self.connection()?;
        let now = Utc::now().to_rfc3339();
        connection.execute(
            r#"
            UPDATE service_domains
            SET status = ?2,
                verified_at = ?3,
                last_check_at = ?4,
                last_error = ?5
            WHERE id = ?1
            "#,
            params![
                id.to_string(),
                status_str(status),
                verified_at.map(|t| t.to_rfc3339()),
                now,
                last_error,
            ],
        )?;
        Ok(())
    }

    pub fn delete_service_domain(&self, id: Uuid) -> Result<(), StateError> {
        let connection = self.connection()?;
        connection.execute(
            "DELETE FROM service_domains WHERE id = ?1",
            params![id.to_string()],
        )?;
        Ok(())
    }

    pub fn list_verified_hostnames(&self, service_id: Uuid) -> Result<Vec<String>, StateError> {
        let connection = self.connection()?;
        let mut stmt = connection.prepare(
            "SELECT hostname FROM service_domains WHERE service_id = ?1 AND status = 'verified' ORDER BY hostname",
        )?;
        let rows = stmt.query_map(params![service_id.to_string()], |row| {
            row.get::<_, String>(0)
        })?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    pub fn list_all_service_domains(&self) -> Result<Vec<ServiceDomain>, StateError> {
        let connection = self.connection()?;
        let mut stmt = connection.prepare(
            "SELECT id, service_id, hostname, status, challenge_token, verified_at, last_check_at, last_error, created_at FROM service_domains ORDER BY created_at",
        )?;
        let rows = stmt.query_map([], row_to_service_domain)?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }
}

fn status_str(s: DomainStatus) -> &'static str {
    match s {
        DomainStatus::Pending => "pending",
        DomainStatus::Verified => "verified",
        DomainStatus::Failed => "failed",
    }
}

fn row_to_service_domain(row: &rusqlite::Row<'_>) -> rusqlite::Result<ServiceDomain> {
    let id: String = row.get(0)?;
    let service_id: String = row.get(1)?;
    let hostname: String = row.get(2)?;
    let status_s: String = row.get(3)?;
    let challenge_token: String = row.get(4)?;
    let verified_at: Option<String> = row.get(5)?;
    let last_check_at: Option<String> = row.get(6)?;
    let last_error: Option<String> = row.get(7)?;
    let created_at: String = row.get(8)?;
    Ok(ServiceDomain {
        id: Uuid::parse_str(&id).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        service_id: Uuid::parse_str(&service_id).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?,
        hostname,
        status: match status_s.as_str() {
            "pending" => DomainStatus::Pending,
            "verified" => DomainStatus::Verified,
            "failed" => DomainStatus::Failed,
            _ => {
                return Err(rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    "invalid status".into(),
                ));
            }
        },
        challenge_token,
        verified_at: verified_at
            .map(|s| chrono::DateTime::parse_from_rfc3339(&s).map(|d| d.with_timezone(&Utc)))
            .transpose()
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
        last_check_at: last_check_at
            .map(|s| chrono::DateTime::parse_from_rfc3339(&s).map(|d| d.with_timezone(&Utc)))
            .transpose()
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?,
        last_error,
        created_at: chrono::DateTime::parse_from_rfc3339(&created_at)
            .map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?
            .with_timezone(&Utc),
    })
}
