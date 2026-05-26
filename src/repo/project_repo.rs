//! Project aggregate repository trait.

use uuid::Uuid;

use crate::domain::Project;
use crate::repo::error::RepoError;

pub trait ProjectRepo: Send + Sync + 'static {
    fn default_project_id(&self) -> Result<Uuid, RepoError>;
    fn put_project(&self, project: Project) -> Result<Project, RepoError>;
    fn get_project(&self, project_id: Uuid) -> Result<Option<Project>, RepoError>;
    fn list_projects(&self) -> Result<Vec<Project>, RepoError>;
    fn count_services_in_project(&self, project_id: Uuid) -> Result<i64, RepoError>;
    fn delete_project(&self, project_id: Uuid) -> Result<(), RepoError>;
}
