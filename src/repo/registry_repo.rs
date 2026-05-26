//! Registry aggregate repository trait (ADR-014).

use uuid::Uuid;

use crate::domain::Registry;
use crate::repo::error::RepoError;

pub trait RegistryRepo: Send + Sync + 'static {
    fn create_registry(&self, registry: &Registry) -> Result<(), RepoError>;
    fn update_registry(&self, registry: &Registry) -> Result<(), RepoError>;
    fn registry(&self, id: Uuid) -> Result<Option<Registry>, RepoError>;
    fn registries_for_project(&self, project_id: Uuid) -> Result<Vec<Registry>, RepoError>;
    fn delete_registry(&self, id: Uuid) -> Result<(), RepoError>;
}
