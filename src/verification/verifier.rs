use crate::verification::error::DomainVerifyError;

#[async_trait::async_trait]
pub trait DomainVerifier: Send + Sync {
    async fn verify(&self, hostname: &str, token: &str) -> Result<(), DomainVerifyError>;
}
