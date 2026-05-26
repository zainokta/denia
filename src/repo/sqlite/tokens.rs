//! `SqliteStore` impl block for API token aggregate methods.

use chrono::Utc;
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use crate::domain::{ApiToken, User};
use crate::state::{SqliteStore, StateError};

impl SqliteStore {
    pub fn create_api_token(&self, user_id: Uuid, name: &str) -> Result<ApiToken, StateError> {
        let token = crate::auth::generate_token();
        let api_token = ApiToken {
            id: Uuid::now_v7(),
            user_id,
            name: name.to_string(),
            token,
            created_at: Utc::now(),
        };
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO api_tokens (id, user_id, name, token_hash, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                api_token.id.to_string(),
                user_id.to_string(),
                api_token.name,
                crate::auth::hash_token(&api_token.token),
            api_token.created_at.to_rfc3339(),
            ],
        )?;
        Ok(api_token)
    }

    pub fn user_for_api_token(&self, token_hash: &str) -> Result<Option<User>, StateError> {
        let user_id_str = {
            let connection = self.connection()?;
            connection
                .query_row(
                    "SELECT user_id FROM api_tokens WHERE token_hash = ?1",
                    params![token_hash],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
        };
        let user_id_str = match user_id_str {
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
                token: th,
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
}
