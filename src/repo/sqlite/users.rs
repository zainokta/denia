//! User / session / membership aggregate sqlite repo.
//!
//! Shared SQL lives in `*_q` free functions; both `SqliteStore` and
//! `SqliteUserRepo` delegate.

use std::sync::Arc;

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::domain::{ProjectMembership, Role, Session, User};
use crate::repo::error::RepoError;
use crate::repo::sqlite::pool::SqlitePool;
use crate::repo::user_repo::UserRepo;
use crate::state::{SqliteStore, StateError};

pub(super) fn create_user_q(
    conn: &Connection,
    username: &str,
    password_hash: &str,
    is_super_admin: bool,
) -> Result<User, RepoError> {
    let user = User::new(username, password_hash.to_string(), is_super_admin).map_err(|_| {
        RepoError::Json(serde_json::Error::io(std::io::Error::other("domain error")))
    })?;
    conn.execute(
        "INSERT INTO users (id, username, password_hash, is_super_admin, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            user.id.to_string(),
            user.username,
            user.password_hash,
            user.is_super_admin as i32,
            user.created_at.to_rfc3339(),
        ],
    )?;
    Ok(user)
}

pub(super) fn get_user_q(conn: &Connection, user_id: Uuid) -> Result<Option<User>, RepoError> {
    let result: Option<(String, String, String, bool, String)> = conn
        .query_row(
            "SELECT id, username, password_hash, is_super_admin, created_at FROM users WHERE id = ?1",
            params![user_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .optional()?;
    result
        .map(
            |(id, username, password_hash, is_super_admin, created_at)| {
                Ok(User {
                    id: Uuid::parse_str(&id)?,
                    username,
                    password_hash,
                    is_super_admin,
                    created_at: created_at.parse()?,
                })
            },
        )
        .transpose()
}

pub(super) fn list_users_q(conn: &Connection) -> Result<Vec<User>, RepoError> {
    let mut stmt = conn.prepare(
        "SELECT id, username, password_hash, is_super_admin, created_at FROM users ORDER BY username",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, bool>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    let mut users = Vec::new();
    for row in rows {
        let (id, username, password_hash, is_super_admin, created_at) = row?;
        users.push(User {
            id: Uuid::parse_str(&id)?,
            username,
            password_hash,
            is_super_admin,
            created_at: created_at.parse()?,
        });
    }
    Ok(users)
}

pub(super) fn delete_user_q(conn: &Connection, user_id: Uuid) -> Result<(), RepoError> {
    let super_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM users WHERE is_super_admin = 1",
        [],
        |row| row.get(0),
    )?;
    let target_is_super: bool = conn.query_row(
        "SELECT is_super_admin FROM users WHERE id = ?1",
        params![user_id.to_string()],
        |row| row.get(0),
    )?;
    if super_count <= 1 && target_is_super {
        return Err(RepoError::LastSuperAdmin);
    }
    conn.execute(
        "DELETE FROM sessions WHERE user_id = ?1",
        params![user_id.to_string()],
    )?;
    conn.execute(
        "DELETE FROM api_tokens WHERE user_id = ?1",
        params![user_id.to_string()],
    )?;
    conn.execute(
        "DELETE FROM project_members WHERE user_id = ?1",
        params![user_id.to_string()],
    )?;
    conn.execute(
        "DELETE FROM users WHERE id = ?1",
        params![user_id.to_string()],
    )?;
    Ok(())
}

pub(super) fn verify_login_q(
    conn: &Connection,
    username: &str,
    password: &str,
) -> Result<User, RepoError> {
    let result: Option<(String, String, String, bool, String)> = conn
        .query_row(
            "SELECT id, username, password_hash, is_super_admin, created_at FROM users WHERE username = ?1",
            params![username],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .optional()?;
    match result {
        Some((id, username, password_hash, is_super_admin, created_at)) => {
            if crate::auth::verify_password(&password_hash, password) {
                Ok(User {
                    id: Uuid::parse_str(&id)?,
                    username,
                    password_hash,
                    is_super_admin,
                    created_at: created_at.parse()?,
                })
            } else {
                Err(RepoError::InvalidCredentials)
            }
        }
        None => Err(RepoError::InvalidCredentials),
    }
}

pub(super) fn create_session_q(
    conn: &Connection,
    user_id: Uuid,
    ttl_hours: i64,
) -> Result<Session, RepoError> {
    let token = crate::auth::generate_token();
    let token_hash = crate::auth::hash_token(&token);
    let expires_at = Utc::now() + chrono::TimeDelta::hours(ttl_hours);
    conn.execute(
        "INSERT INTO sessions (token_hash, user_id, expires_at) VALUES (?1, ?2, ?3)",
        params![&token_hash, user_id.to_string(), expires_at.to_rfc3339()],
    )?;
    Ok(Session {
        token,
        user_id,
        expires_at,
    })
}

pub(super) fn delete_session_q(conn: &Connection, token_hash: &str) -> Result<(), RepoError> {
    conn.execute(
        "DELETE FROM sessions WHERE token_hash = ?1",
        params![token_hash],
    )?;
    Ok(())
}

pub(super) fn set_membership_q(
    conn: &Connection,
    user_id: Uuid,
    project_id: Uuid,
    role: Role,
) -> Result<(), RepoError> {
    conn.execute(
        "INSERT OR REPLACE INTO project_members (user_id, project_id, role) VALUES (?1, ?2, ?3)",
        params![
            user_id.to_string(),
            project_id.to_string(),
            serde_json::to_string(&role)?,
        ],
    )?;
    Ok(())
}

pub(super) fn role_for_q(
    conn: &Connection,
    user_id: Uuid,
    project_id: Uuid,
) -> Result<Option<Role>, RepoError> {
    let role_str: Option<String> = conn
        .query_row(
            "SELECT role FROM project_members WHERE user_id = ?1 AND project_id = ?2",
            params![user_id.to_string(), project_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    role_str
        .map(|r| serde_json::from_str(&r))
        .transpose()
        .map_err(Into::into)
}

pub(super) fn list_members_q(
    conn: &Connection,
    project_id: Uuid,
) -> Result<Vec<ProjectMembership>, RepoError> {
    let mut stmt = conn
        .prepare("SELECT user_id, project_id, role FROM project_members WHERE project_id = ?1")?;
    let rows = stmt.query_map(params![project_id.to_string()], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut members = Vec::new();
    for row in rows {
        let (uid, pid, role_str) = row?;
        members.push(ProjectMembership {
            user_id: Uuid::parse_str(&uid)?,
            project_id: Uuid::parse_str(&pid)?,
            role: serde_json::from_str(&role_str)?,
        });
    }
    Ok(members)
}

pub(super) fn remove_membership_q(
    conn: &Connection,
    user_id: Uuid,
    project_id: Uuid,
) -> Result<(), RepoError> {
    conn.execute(
        "DELETE FROM project_members WHERE user_id = ?1 AND project_id = ?2",
        params![user_id.to_string(), project_id.to_string()],
    )?;
    Ok(())
}

pub(super) fn list_memberships_for_user_q(
    conn: &Connection,
    user_id: Uuid,
) -> Result<Vec<ProjectMembership>, RepoError> {
    let mut stmt =
        conn.prepare("SELECT user_id, project_id, role FROM project_members WHERE user_id = ?1")?;
    let rows = stmt.query_map(params![user_id.to_string()], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut members = Vec::new();
    for row in rows {
        let (uid, pid, role_str) = row?;
        members.push(ProjectMembership {
            user_id: Uuid::parse_str(&uid)?,
            project_id: Uuid::parse_str(&pid)?,
            role: serde_json::from_str(&role_str)?,
        });
    }
    Ok(members)
}

/// Inner body for `user_for_session`: reads the session row and either deletes
/// expired sessions or returns the looked-up user. Acquires/releases the lock
/// twice to match the original `SqliteStore::user_for_session` behavior.
pub(super) fn user_for_session_q(
    pool: &SqlitePool,
    token_hash: &str,
) -> Result<Option<User>, RepoError> {
    let row = {
        let conn = pool.connection()?;
        conn.query_row(
            "SELECT user_id, expires_at FROM sessions WHERE token_hash = ?1",
            params![token_hash],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
    };
    let (user_id_str, expires_at_str) = match row {
        Some(r) => r,
        None => return Ok(None),
    };
    let expires_at: chrono::DateTime<Utc> = expires_at_str.parse()?;
    if Utc::now() > expires_at {
        let conn = pool.connection()?;
        conn.execute(
            "DELETE FROM sessions WHERE token_hash = ?1",
            params![token_hash],
        )?;
        return Ok(None);
    }
    let user_id = Uuid::parse_str(&user_id_str)?;
    let conn = pool.connection()?;
    get_user_q(&conn, user_id)
}

impl SqliteStore {
    pub fn create_user(
        &self,
        username: &str,
        password_hash: &str,
        is_super_admin: bool,
    ) -> Result<User, StateError> {
        let connection = self.connection()?;
        create_user_q(&connection, username, password_hash, is_super_admin)
            .map_err(StateError::from)
    }

    pub fn get_user(&self, user_id: Uuid) -> Result<Option<User>, StateError> {
        let connection = self.connection()?;
        get_user_q(&connection, user_id).map_err(StateError::from)
    }

    pub fn list_users(&self) -> Result<Vec<User>, StateError> {
        let connection = self.connection()?;
        list_users_q(&connection).map_err(StateError::from)
    }

    pub fn delete_user(&self, user_id: Uuid) -> Result<(), StateError> {
        let connection = self.connection()?;
        delete_user_q(&connection, user_id).map_err(StateError::from)
    }

    pub fn verify_login(&self, username: &str, password: &str) -> Result<User, StateError> {
        let connection = self.connection()?;
        verify_login_q(&connection, username, password).map_err(StateError::from)
    }

    pub fn create_session(&self, user_id: Uuid, ttl_hours: i64) -> Result<Session, StateError> {
        let connection = self.connection()?;
        create_session_q(&connection, user_id, ttl_hours).map_err(StateError::from)
    }

    pub fn user_for_session(&self, token_hash: &str) -> Result<Option<User>, StateError> {
        let pool = SqlitePool {
            inner: Arc::clone(&self.connection),
        };
        user_for_session_q(&pool, token_hash).map_err(StateError::from)
    }

    pub fn delete_session(&self, token_hash: &str) -> Result<(), StateError> {
        let connection = self.connection()?;
        delete_session_q(&connection, token_hash).map_err(StateError::from)
    }

    pub fn set_membership(
        &self,
        user_id: Uuid,
        project_id: Uuid,
        role: Role,
    ) -> Result<(), StateError> {
        let connection = self.connection()?;
        set_membership_q(&connection, user_id, project_id, role).map_err(StateError::from)
    }

    pub fn role_for(&self, user_id: Uuid, project_id: Uuid) -> Result<Option<Role>, StateError> {
        let connection = self.connection()?;
        role_for_q(&connection, user_id, project_id).map_err(StateError::from)
    }

    pub fn list_members(&self, project_id: Uuid) -> Result<Vec<ProjectMembership>, StateError> {
        let connection = self.connection()?;
        list_members_q(&connection, project_id).map_err(StateError::from)
    }

    pub fn remove_membership(&self, user_id: Uuid, project_id: Uuid) -> Result<(), StateError> {
        let connection = self.connection()?;
        remove_membership_q(&connection, user_id, project_id).map_err(StateError::from)
    }

    pub fn list_memberships_for_user(
        &self,
        user_id: Uuid,
    ) -> Result<Vec<ProjectMembership>, StateError> {
        let connection = self.connection()?;
        list_memberships_for_user_q(&connection, user_id).map_err(StateError::from)
    }
}

pub struct SqliteUserRepo {
    pool: SqlitePool,
}

impl SqliteUserRepo {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl UserRepo for SqliteUserRepo {
    fn create_user(
        &self,
        username: &str,
        password_hash: &str,
        is_super_admin: bool,
    ) -> Result<User, RepoError> {
        let conn = self.pool.connection()?;
        create_user_q(&conn, username, password_hash, is_super_admin)
    }

    fn get_user(&self, user_id: Uuid) -> Result<Option<User>, RepoError> {
        let conn = self.pool.connection()?;
        get_user_q(&conn, user_id)
    }

    fn list_users(&self) -> Result<Vec<User>, RepoError> {
        let conn = self.pool.connection()?;
        list_users_q(&conn)
    }

    fn delete_user(&self, user_id: Uuid) -> Result<(), RepoError> {
        let conn = self.pool.connection()?;
        delete_user_q(&conn, user_id)
    }

    fn verify_login(&self, username: &str, password: &str) -> Result<User, RepoError> {
        let conn = self.pool.connection()?;
        verify_login_q(&conn, username, password)
    }

    fn create_session(&self, user_id: Uuid, ttl_hours: i64) -> Result<Session, RepoError> {
        let conn = self.pool.connection()?;
        create_session_q(&conn, user_id, ttl_hours)
    }

    fn user_for_session(&self, token_hash: &str) -> Result<Option<User>, RepoError> {
        user_for_session_q(&self.pool, token_hash)
    }

    fn delete_session(&self, token_hash: &str) -> Result<(), RepoError> {
        let conn = self.pool.connection()?;
        delete_session_q(&conn, token_hash)
    }

    fn set_membership(&self, user_id: Uuid, project_id: Uuid, role: Role) -> Result<(), RepoError> {
        let conn = self.pool.connection()?;
        set_membership_q(&conn, user_id, project_id, role)
    }

    fn role_for(&self, user_id: Uuid, project_id: Uuid) -> Result<Option<Role>, RepoError> {
        let conn = self.pool.connection()?;
        role_for_q(&conn, user_id, project_id)
    }

    fn list_members(&self, project_id: Uuid) -> Result<Vec<ProjectMembership>, RepoError> {
        let conn = self.pool.connection()?;
        list_members_q(&conn, project_id)
    }

    fn remove_membership(&self, user_id: Uuid, project_id: Uuid) -> Result<(), RepoError> {
        let conn = self.pool.connection()?;
        remove_membership_q(&conn, user_id, project_id)
    }

    fn list_memberships_for_user(
        &self,
        user_id: Uuid,
    ) -> Result<Vec<ProjectMembership>, RepoError> {
        let conn = self.pool.connection()?;
        list_memberships_for_user_q(&conn, user_id)
    }
}
