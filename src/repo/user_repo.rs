//! User / session / membership repository trait.

use uuid::Uuid;

use crate::domain::{ProjectMembership, Role, Session, User};
use crate::repo::error::RepoError;

#[allow(dead_code)]
pub trait UserRepo: Send + Sync + 'static {
    fn create_user(
        &self,
        username: &str,
        password_hash: &str,
        is_super_admin: bool,
    ) -> Result<User, RepoError>;
    fn get_user(&self, user_id: Uuid) -> Result<Option<User>, RepoError>;
    fn list_users(&self) -> Result<Vec<User>, RepoError>;
    fn delete_user(&self, user_id: Uuid) -> Result<(), RepoError>;
    fn verify_login(&self, username: &str, password: &str) -> Result<User, RepoError>;
    fn create_session(&self, user_id: Uuid, ttl_hours: i64) -> Result<Session, RepoError>;
    fn user_for_session(&self, token_hash: &str) -> Result<Option<User>, RepoError>;
    fn delete_session(&self, token_hash: &str) -> Result<(), RepoError>;
    fn set_membership(&self, user_id: Uuid, project_id: Uuid, role: Role) -> Result<(), RepoError>;
    fn role_for(&self, user_id: Uuid, project_id: Uuid) -> Result<Option<Role>, RepoError>;
    fn list_members(&self, project_id: Uuid) -> Result<Vec<ProjectMembership>, RepoError>;
    fn remove_membership(&self, user_id: Uuid, project_id: Uuid) -> Result<(), RepoError>;
    fn list_memberships_for_user(&self, user_id: Uuid)
    -> Result<Vec<ProjectMembership>, RepoError>;
}
