//! `SqliteStore` impl block for credential aggregate methods.

use rusqlite::params;
use uuid::Uuid;

use crate::domain::{Credential, CredentialKind};
use crate::secrets::SecretRef;
use crate::state::{SqliteStore, StateError};

impl SqliteStore {
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
}
