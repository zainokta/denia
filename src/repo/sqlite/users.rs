//! `SqliteStore` impl block for user / session / membership aggregate methods.

use chrono::Utc;
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

use crate::domain::{ProjectMembership, Role, Session, User};
use crate::state::{SqliteStore, StateError};

impl SqliteStore {
    pub fn create_user(
        &self,
        username: &str,
        password_hash: &str,
        is_super_admin: bool,
    ) -> Result<User, StateError> {
        let user =
            User::new(username, password_hash.to_string(), is_super_admin).map_err(|_| {
                StateError::Json(serde_json::Error::io(std::io::Error::other("domain error")))
            })?;
        let connection = self.connection()?;
        connection.execute(
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

    pub fn get_user(&self, user_id: Uuid) -> Result<Option<User>, StateError> {
        let connection = self.connection()?;
        let result: Option<(String, String, String, bool, String)> = connection
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

    pub fn list_users(&self) -> Result<Vec<User>, StateError> {
        let connection = self.connection()?;
        let mut stmt = connection.prepare(
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

    pub fn delete_user(&self, user_id: Uuid) -> Result<(), StateError> {
        let connection = self.connection()?;
        let super_count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM users WHERE is_super_admin = 1",
            [],
            |row| row.get(0),
        )?;
        let target_is_super: bool = connection.query_row(
            "SELECT is_super_admin FROM users WHERE id = ?1",
            params![user_id.to_string()],
            |row| row.get(0),
        )?;
        if super_count <= 1 && target_is_super {
            return Err(StateError::LastSuperAdmin);
        }
        connection.execute(
            "DELETE FROM sessions WHERE user_id = ?1",
            params![user_id.to_string()],
        )?;
        connection.execute(
            "DELETE FROM api_tokens WHERE user_id = ?1",
            params![user_id.to_string()],
        )?;
        connection.execute(
            "DELETE FROM project_members WHERE user_id = ?1",
            params![user_id.to_string()],
        )?;
        connection.execute(
            "DELETE FROM users WHERE id = ?1",
            params![user_id.to_string()],
        )?;
        Ok(())
    }

    pub fn verify_login(&self, username: &str, password: &str) -> Result<User, StateError> {
        let connection = self.connection()?;
        let result: Option<(String, String, String, bool, String)> = connection
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
                    Err(StateError::InvalidCredentials)
                }
            }
            None => Err(StateError::InvalidCredentials),
        }
    }

    pub fn create_session(&self, user_id: Uuid, ttl_hours: i64) -> Result<Session, StateError> {
        let token = crate::auth::generate_token();
        let token_hash = crate::auth::hash_token(&token);
        let expires_at = Utc::now() + chrono::TimeDelta::hours(ttl_hours);
        let connection = self.connection()?;
        connection.execute(
            "INSERT INTO sessions (token_hash, user_id, expires_at) VALUES (?1, ?2, ?3)",
            params![&token_hash, user_id.to_string(), expires_at.to_rfc3339()],
        )?;
        Ok(Session {
            token,
            user_id,
            expires_at,
        })
    }

    pub fn user_for_session(&self, token_hash: &str) -> Result<Option<User>, StateError> {
        let row = {
            let connection = self.connection()?;
            connection
                .query_row(
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
            let connection = self.connection()?;
            connection.execute(
                "DELETE FROM sessions WHERE token_hash = ?1",
                params![token_hash],
            )?;
            return Ok(None);
        }
        let user_id = Uuid::parse_str(&user_id_str)?;
        self.get_user(user_id)
    }

    pub fn delete_session(&self, token_hash: &str) -> Result<(), StateError> {
        let connection = self.connection()?;
        connection.execute(
            "DELETE FROM sessions WHERE token_hash = ?1",
            params![token_hash],
        )?;
        Ok(())
    }

    pub fn set_membership(
        &self,
        user_id: Uuid,
        project_id: Uuid,
        role: Role,
    ) -> Result<(), StateError> {
        let connection = self.connection()?;
        connection.execute(
            "INSERT OR REPLACE INTO project_members (user_id, project_id, role) VALUES (?1, ?2, ?3)",
            params![
                user_id.to_string(),
                project_id.to_string(),
                serde_json::to_string(&role)?,
            ],
        )?;
        Ok(())
    }

    pub fn role_for(&self, user_id: Uuid, project_id: Uuid) -> Result<Option<Role>, StateError> {
        let connection = self.connection()?;
        let role_str: Option<String> = connection
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

    pub fn list_members(&self, project_id: Uuid) -> Result<Vec<ProjectMembership>, StateError> {
        let connection = self.connection()?;
        let mut stmt = connection.prepare(
            "SELECT user_id, project_id, role FROM project_members WHERE project_id = ?1",
        )?;
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

    pub fn remove_membership(&self, user_id: Uuid, project_id: Uuid) -> Result<(), StateError> {
        let connection = self.connection()?;
        connection.execute(
            "DELETE FROM project_members WHERE user_id = ?1 AND project_id = ?2",
            params![user_id.to_string(), project_id.to_string()],
        )?;
        Ok(())
    }

    pub fn list_memberships_for_user(
        &self,
        user_id: Uuid,
    ) -> Result<Vec<ProjectMembership>, StateError> {
        let connection = self.connection()?;
        let mut stmt = connection
            .prepare("SELECT user_id, project_id, role FROM project_members WHERE user_id = ?1")?;
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
}
