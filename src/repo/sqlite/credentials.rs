//! Credential aggregate sqlite repo.
//!
//! Shared SQL lives in `*_q` free functions; `SqliteStore` and
//! `SqliteCredentialRepo` both delegate.

use rusqlite::{Connection, params};
use uuid::Uuid;

use crate::domain::{Credential, CredentialKind};
use crate::repo::error::RepoError;
use crate::repo::sqlite::pool::SqlitePool;
use crate::secrets::SecretRef;
use crate::state::{SqliteStore, StateError};

pub(super) fn list_credentials_q(conn: &Connection) -> Result<Vec<Credential>, RepoError> {
    let mut stmt =
        conn.prepare("SELECT id, name, kind, secret_ref FROM credentials ORDER BY name ASC")?;
    let rows = stmt.query_map([], |row| {
        let id_str: String = row.get(0)?;
        let kind_str: String = row.get(2)?;
        let secret_ref_str: String = row.get(3)?;
        Ok((id_str, row.get::<_, String>(1)?, kind_str, secret_ref_str))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id_str, name, kind_str, secret_ref_str) = row?;
        let id = Uuid::parse_str(&id_str)?;
        let kind: CredentialKind = serde_json::from_str(&kind_str)?;
        let secret_ref = SecretRef::parse(secret_ref_str)
            .map_err(|e| RepoError::InvalidStatus(format!("credentials.secret_ref: {e:?}")))?;
        out.push(Credential {
            id,
            name,
            kind,
            secret_ref,
        });
    }
    Ok(out)
}

pub(super) fn put_credential_q(
    conn: &Connection,
    name: String,
    kind: CredentialKind,
    secret_ref: SecretRef,
) -> Result<Credential, RepoError> {
    let credential = Credential {
        id: Uuid::now_v7(),
        name,
        kind,
        secret_ref,
    };
    conn.execute(
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

impl SqliteStore {
    pub fn put_credential(
        &self,
        name: impl Into<String>,
        kind: CredentialKind,
        secret_ref: SecretRef,
    ) -> Result<Credential, StateError> {
        let connection = self.connection()?;
        put_credential_q(&connection, name.into(), kind, secret_ref).map_err(StateError::from)
    }

    pub fn list_credentials(&self) -> Result<Vec<Credential>, StateError> {
        let connection = self.connection()?;
        list_credentials_q(&connection).map_err(StateError::from)
    }
}

#[derive(Clone)]
pub struct SqliteCredentialRepo {
    pool: SqlitePool,
}

impl SqliteCredentialRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl SqliteCredentialRepo {
    pub fn put_credential(
        &self,
        name: String,
        kind: CredentialKind,
        secret_ref: SecretRef,
    ) -> Result<Credential, RepoError> {
        let conn = self.pool.connection()?;
        put_credential_q(&conn, name, kind, secret_ref)
    }

    pub fn list_credentials(&self) -> Result<Vec<Credential>, RepoError> {
        let conn = self.pool.connection()?;
        list_credentials_q(&conn)
    }
}
