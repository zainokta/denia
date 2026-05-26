//! Service-domain aggregate sqlite repo.
//!
//! Shared SQL lives in `*_q` free functions; both `SqliteStore` and
//! `SqliteDomainRepo` delegate.

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::domain::{DomainStatus, ServiceDomain};
use crate::repo::domain_repo::DomainRepo;
use crate::repo::error::RepoError;
use crate::repo::sqlite::pool::SqlitePool;
use crate::state::{SqliteStore, StateError};

pub(super) fn put_service_domain_q(conn: &Connection, d: &ServiceDomain) -> Result<(), RepoError> {
    conn.execute(
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

pub(super) fn get_service_domain_q(
    conn: &Connection,
    id: Uuid,
) -> Result<Option<ServiceDomain>, RepoError> {
    conn.query_row(
        "SELECT id, service_id, hostname, status, challenge_token, verified_at, last_check_at, last_error, created_at FROM service_domains WHERE id = ?1",
        params![id.to_string()],
        row_to_service_domain,
    )
    .optional()
    .map_err(Into::into)
}

pub(super) fn get_service_domain_by_token_q(
    conn: &Connection,
    token: &str,
) -> Result<Option<ServiceDomain>, RepoError> {
    conn.query_row(
        "SELECT id, service_id, hostname, status, challenge_token, verified_at, last_check_at, last_error, created_at FROM service_domains WHERE challenge_token = ?1",
        params![token],
        row_to_service_domain,
    )
    .optional()
    .map_err(Into::into)
}

pub(super) fn list_service_domains_by_service_q(
    conn: &Connection,
    service_id: Uuid,
) -> Result<Vec<ServiceDomain>, RepoError> {
    let mut stmt = conn.prepare(
        "SELECT id, service_id, hostname, status, challenge_token, verified_at, last_check_at, last_error, created_at FROM service_domains WHERE service_id = ?1 ORDER BY created_at",
    )?;
    let rows = stmt.query_map(params![service_id.to_string()], row_to_service_domain)?;
    rows.collect::<Result<_, _>>().map_err(Into::into)
}

pub(super) fn update_service_domain_status_q(
    conn: &Connection,
    id: Uuid,
    status: DomainStatus,
    verified_at: Option<chrono::DateTime<Utc>>,
    last_error: Option<String>,
) -> Result<(), RepoError> {
    let now = Utc::now().to_rfc3339();
    conn.execute(
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

pub(super) fn delete_service_domain_q(conn: &Connection, id: Uuid) -> Result<(), RepoError> {
    conn.execute(
        "DELETE FROM service_domains WHERE id = ?1",
        params![id.to_string()],
    )?;
    Ok(())
}

pub(super) fn list_verified_hostnames_q(
    conn: &Connection,
    service_id: Uuid,
) -> Result<Vec<String>, RepoError> {
    let mut stmt = conn.prepare(
        "SELECT hostname FROM service_domains WHERE service_id = ?1 AND status = 'verified' ORDER BY hostname",
    )?;
    let rows = stmt.query_map(params![service_id.to_string()], |row| {
        row.get::<_, String>(0)
    })?;
    rows.collect::<Result<_, _>>().map_err(Into::into)
}

pub(super) fn list_all_service_domains_q(
    conn: &Connection,
) -> Result<Vec<ServiceDomain>, RepoError> {
    let mut stmt = conn.prepare(
        "SELECT id, service_id, hostname, status, challenge_token, verified_at, last_check_at, last_error, created_at FROM service_domains ORDER BY created_at",
    )?;
    let rows = stmt.query_map([], row_to_service_domain)?;
    rows.collect::<Result<_, _>>().map_err(Into::into)
}

impl SqliteStore {
    pub fn put_service_domain(&self, d: &ServiceDomain) -> Result<(), StateError> {
        let connection = self.connection()?;
        put_service_domain_q(&connection, d).map_err(StateError::from)
    }

    pub fn get_service_domain(&self, id: Uuid) -> Result<Option<ServiceDomain>, StateError> {
        let connection = self.connection()?;
        get_service_domain_q(&connection, id).map_err(StateError::from)
    }

    pub fn get_service_domain_by_token(
        &self,
        token: &str,
    ) -> Result<Option<ServiceDomain>, StateError> {
        let connection = self.connection()?;
        get_service_domain_by_token_q(&connection, token).map_err(StateError::from)
    }

    pub fn list_service_domains_by_service(
        &self,
        service_id: Uuid,
    ) -> Result<Vec<ServiceDomain>, StateError> {
        let connection = self.connection()?;
        list_service_domains_by_service_q(&connection, service_id).map_err(StateError::from)
    }

    pub fn update_service_domain_status(
        &self,
        id: Uuid,
        status: DomainStatus,
        verified_at: Option<chrono::DateTime<Utc>>,
        last_error: Option<String>,
    ) -> Result<(), StateError> {
        let connection = self.connection()?;
        update_service_domain_status_q(&connection, id, status, verified_at, last_error)
            .map_err(StateError::from)
    }

    pub fn delete_service_domain(&self, id: Uuid) -> Result<(), StateError> {
        let connection = self.connection()?;
        delete_service_domain_q(&connection, id).map_err(StateError::from)
    }

    pub fn list_verified_hostnames(&self, service_id: Uuid) -> Result<Vec<String>, StateError> {
        let connection = self.connection()?;
        list_verified_hostnames_q(&connection, service_id).map_err(StateError::from)
    }

    pub fn list_all_service_domains(&self) -> Result<Vec<ServiceDomain>, StateError> {
        let connection = self.connection()?;
        list_all_service_domains_q(&connection).map_err(StateError::from)
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

#[allow(dead_code)]
pub struct SqliteDomainRepo {
    pool: SqlitePool,
}

#[allow(dead_code)]
impl SqliteDomainRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl DomainRepo for SqliteDomainRepo {
    fn put_service_domain(&self, d: &ServiceDomain) -> Result<(), RepoError> {
        let conn = self.pool.connection()?;
        put_service_domain_q(&conn, d)
    }

    fn get_service_domain(&self, id: Uuid) -> Result<Option<ServiceDomain>, RepoError> {
        let conn = self.pool.connection()?;
        get_service_domain_q(&conn, id)
    }

    fn get_service_domain_by_token(&self, token: &str) -> Result<Option<ServiceDomain>, RepoError> {
        let conn = self.pool.connection()?;
        get_service_domain_by_token_q(&conn, token)
    }

    fn list_service_domains_by_service(
        &self,
        service_id: Uuid,
    ) -> Result<Vec<ServiceDomain>, RepoError> {
        let conn = self.pool.connection()?;
        list_service_domains_by_service_q(&conn, service_id)
    }

    fn update_service_domain_status(
        &self,
        id: Uuid,
        status: DomainStatus,
        verified_at: Option<chrono::DateTime<Utc>>,
        last_error: Option<String>,
    ) -> Result<(), RepoError> {
        let conn = self.pool.connection()?;
        update_service_domain_status_q(&conn, id, status, verified_at, last_error)
    }

    fn delete_service_domain(&self, id: Uuid) -> Result<(), RepoError> {
        let conn = self.pool.connection()?;
        delete_service_domain_q(&conn, id)
    }

    fn list_verified_hostnames(&self, service_id: Uuid) -> Result<Vec<String>, RepoError> {
        let conn = self.pool.connection()?;
        list_verified_hostnames_q(&conn, service_id)
    }

    fn list_all_service_domains(&self) -> Result<Vec<ServiceDomain>, RepoError> {
        let conn = self.pool.connection()?;
        list_all_service_domains_q(&conn)
    }
}
