//! API-token repository trait.

use uuid::Uuid;

use crate::domain::{ApiToken, User};
use crate::repo::error::RepoError;

#[allow(dead_code)]
pub trait TokenRepo: Send + Sync + 'static {
    fn create_api_token(&self, user_id: Uuid, name: &str) -> Result<ApiToken, RepoError>;
    fn user_for_api_token(&self, token_hash: &str) -> Result<Option<User>, RepoError>;
    fn list_api_tokens(&self, user_id: Uuid) -> Result<Vec<ApiToken>, RepoError>;
    fn revoke_api_token(&self, token_id: Uuid) -> Result<(), RepoError>;
}
