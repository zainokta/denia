use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domain::error::DomainError;
use crate::domain::service::ResourceLimits;
use crate::domain::user::Role;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub shared_env: Vec<(String, String)>,
    #[serde(default)]
    pub default_resource_limits: Option<ResourceLimits>,
    pub created_at: DateTime<Utc>,
}

impl Project {
    pub fn new(name: impl Into<String>, description: Option<String>) -> Result<Self, DomainError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(DomainError::EmptyName);
        }
        Ok(Self {
            id: Uuid::now_v7(),
            name,
            description,
            shared_env: Vec::new(),
            default_resource_limits: None,
            created_at: Utc::now(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectMembership {
    pub user_id: Uuid,
    pub project_id: Uuid,
    pub role: Role,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_rejects_empty_name() {
        assert_eq!(Project::new("", None).unwrap_err(), DomainError::EmptyName);
    }

    #[test]
    fn project_has_id_and_defaults() {
        let p = Project::new("default", Some("seed".into())).unwrap();
        assert_eq!(p.name, "default");
        assert!(p.shared_env.is_empty());
        assert!(p.default_resource_limits.is_none());
    }
}
