//! Service-domain aggregate repository trait (ADR-013).

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::domain::{DomainStatus, ServiceDomain};
use crate::repo::error::RepoError;

#[allow(dead_code)]
pub trait DomainRepo: Send + Sync + 'static {
    fn put_service_domain(&self, d: &ServiceDomain) -> Result<(), RepoError>;
    fn get_service_domain(&self, id: Uuid) -> Result<Option<ServiceDomain>, RepoError>;
    fn get_service_domain_by_token(&self, token: &str) -> Result<Option<ServiceDomain>, RepoError>;
    fn list_service_domains_by_service(
        &self,
        service_id: Uuid,
    ) -> Result<Vec<ServiceDomain>, RepoError>;
    fn update_service_domain_status(
        &self,
        id: Uuid,
        status: DomainStatus,
        verified_at: Option<DateTime<Utc>>,
        last_error: Option<String>,
    ) -> Result<(), RepoError>;
    fn delete_service_domain(&self, id: Uuid) -> Result<(), RepoError>;
    fn list_all_service_domains(&self) -> Result<Vec<ServiceDomain>, RepoError>;
}
