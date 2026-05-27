//! API token aggregate sqlite repo.
//!
//! Shared SQL lives in `*_q` free functions; both `SqliteStore` and
//! `SqliteTokenRepo` delegate.

use std::sync::Arc;

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::domain::{ApiToken, User};
use crate::repo::error::RepoError;
use crate::repo::sqlite::pool::SqlitePool;
use crate::repo::sqlite::users::get_user_q;
use crate::state::{SqliteStore, StateError};

pub(super) fn create_api_token_q(
    conn: &Connection,
    user_id: Uuid,
    name: &str,
) -> Result<ApiToken, RepoError> {
    let token = crate::auth::generate_token();
    let api_token = ApiToken {
        id: Uuid::now_v7(),
        user_id,
        name: name.to_string(),
        token,
        created_at: Utc::now(),
    };
    conn.execute(
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

pub(super) fn list_api_tokens_q(
    conn: &Connection,
    user_id: Uuid,
) -> Result<Vec<ApiToken>, RepoError> {
    let mut stmt = conn.prepare(
        "SELECT id, user_id, name, token_hash, created_at FROM api_tokens WHERE user_id = ?1 ORDER BY created_at DESC",
    )?;
    let rows = stmt.query_map(params![user_id.to_string()], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    let mut tokens = Vec::new();
    for row in rows {
        let (id, uid, name, created_at) = row?;
        tokens.push(ApiToken {
            id: Uuid::parse_str(&id)?,
            user_id: Uuid::parse_str(&uid)?,
            name,
            token: String::new(),
            created_at: created_at.parse()?,
        });
    }
    Ok(tokens)
}

pub(super) fn revoke_api_token_q(conn: &Connection, token_id: Uuid) -> Result<(), RepoError> {
    conn.execute(
        "DELETE FROM api_tokens WHERE id = ?1",
        params![token_id.to_string()],
    )?;
    Ok(())
}

/// Inner body for `user_for_api_token`: looks up the user id in a short-lived
/// lock scope, then re-locks to fetch the user. Preserves the two-step locking
/// from the original `SqliteStore` impl.
pub(super) fn user_for_api_token_q(
    pool: &SqlitePool,
    token_hash: &str,
) -> Result<Option<User>, RepoError> {
    let user_id_str = {
        let conn = pool.connection()?;
        conn.query_row(
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
    let conn = pool.connection()?;
    get_user_q(&conn, Uuid::parse_str(&user_id_str)?)
}

impl SqliteStore {
    pub fn create_api_token(&self, user_id: Uuid, name: &str) -> Result<ApiToken, StateError> {
        let connection = self.connection()?;
        create_api_token_q(&connection, user_id, name).map_err(StateError::from)
    }

    pub fn user_for_api_token(&self, token_hash: &str) -> Result<Option<User>, StateError> {
        let pool = SqlitePool {
            inner: Arc::clone(&self.connection),
        };
        user_for_api_token_q(&pool, token_hash).map_err(StateError::from)
    }

    pub fn list_api_tokens(&self, user_id: Uuid) -> Result<Vec<ApiToken>, StateError> {
        let connection = self.connection()?;
        list_api_tokens_q(&connection, user_id).map_err(StateError::from)
    }

    pub fn revoke_api_token(&self, token_id: Uuid) -> Result<(), StateError> {
        let connection = self.connection()?;
        revoke_api_token_q(&connection, token_id).map_err(StateError::from)
    }
}

#[derive(Clone)]
pub struct SqliteTokenRepo {
    pool: SqlitePool,
}

impl SqliteTokenRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl SqliteTokenRepo {
    pub fn create_api_token(&self, user_id: Uuid, name: &str) -> Result<ApiToken, RepoError> {
        let conn = self.pool.connection()?;
        create_api_token_q(&conn, user_id, name)
    }

    pub fn user_for_api_token(&self, token_hash: &str) -> Result<Option<User>, RepoError> {
        user_for_api_token_q(&self.pool, token_hash)
    }

    pub fn list_api_tokens(&self, user_id: Uuid) -> Result<Vec<ApiToken>, RepoError> {
        let conn = self.pool.connection()?;
        list_api_tokens_q(&conn, user_id)
    }

    pub fn revoke_api_token(&self, token_id: Uuid) -> Result<(), RepoError> {
        let conn = self.pool.connection()?;
        revoke_api_token_q(&conn, token_id)
    }
}
