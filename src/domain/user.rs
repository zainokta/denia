use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::project::ProjectMembership;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Viewer = 0,
    Operator = 1,
    Admin = 2,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct User {
    pub id: Uuid,
    pub username: String,
    #[serde(skip_serializing)]
    pub password_hash: String,
    pub is_super_admin: bool,
    pub created_at: DateTime<Utc>,
}

impl User {
    pub fn new(
        username: impl Into<String>,
        password_hash: String,
        is_super_admin: bool,
    ) -> Result<Self, DomainError> {
        let username = username.into();
        if username.trim().is_empty() {
            return Err(DomainError::EmptyName);
        }
        Ok(Self {
            id: Uuid::now_v7(),
            username,
            password_hash,
            is_super_admin,
            created_at: Utc::now(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiToken {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    #[serde(skip_serializing)]
    pub token: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub token: String,
    pub user_id: Uuid,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeUser {
    pub id: Uuid,
    pub username: String,
    pub created_at: DateTime<Utc>,
}

impl From<User> for MeUser {
    fn from(u: User) -> Self {
        Self {
            id: u.id,
            username: u.username,
            created_at: u.created_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PrincipalView {
    User { user: MeUser },
    Bootstrap,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Me {
    pub principal: PrincipalView,
    pub is_super_admin: bool,
    pub admin_initialized: bool,
    pub memberships: Vec<ProjectMembership>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginResult {
    pub token: String,
    pub expires_at: DateTime<Utc>,
}
