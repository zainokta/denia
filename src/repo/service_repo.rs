//! Service aggregate repository trait.

use uuid::Uuid;

use crate::domain::ServiceConfig;
use crate::repo::error::RepoError;

#[allow(dead_code)]
pub trait ServiceRepo: Send + Sync + 'static {
    fn put_service(&self, config: ServiceConfig) -> Result<ServiceConfig, RepoError>;
    fn list_services(&self) -> Result<Vec<ServiceConfig>, RepoError>;
    fn get_service(&self, service_id: Uuid) -> Result<Option<ServiceConfig>, RepoError>;
}
