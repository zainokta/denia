//! Credential aggregate sqlite repo.
//!
//! Shared SQL lives in `*_q` free functions; `SqliteStore` and
//! `SqliteCredentialRepo` both delegate.

use rusqlite::{Connection, params};
use uuid::Uuid;

use crate::domain::{Credential, CredentialKind};
use crate::repo::credential_repo::CredentialRepo;
use crate::repo::error::RepoError;
use crate::repo::sqlite::pool::SqlitePool;
use crate::secrets::SecretRef;
use crate::state::{SqliteStore, StateError};

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
}

#[allow(dead_code)]
pub struct SqliteCredentialRepo {
    pool: SqlitePool,
}

#[allow(dead_code)]
impl SqliteCredentialRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl CredentialRepo for SqliteCredentialRepo {
    fn put_credential(
        &self,
        name: String,
        kind: CredentialKind,
        secret_ref: SecretRef,
    ) -> Result<Credential, RepoError> {
        let conn = self.pool.connection()?;
        put_credential_q(&conn, name, kind, secret_ref)
    }
}
