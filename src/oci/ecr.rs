use async_trait::async_trait;
use oci_client::secrets::RegistryAuth;

use super::{OciError, credentials::RegistryCredentialProvider};

/// AWS ECR registry credential provider.
///
/// Behind the `ecr` cargo feature. Reads username + password from
/// `DENIA_ECR_USERNAME` and `DENIA_ECR_PASSWORD`. Operators populate these
/// from `aws ecr get-login-password` (or via an external rotation hook).
///
/// SigV4 token exchange is deferred — landing the full `aws-sdk-ecr`
/// dependency is a separate ADR-011 follow-up.
pub struct EcrCredentialProvider {
    registry_suffix: String,
}

impl EcrCredentialProvider {
    pub fn new() -> Self {
        Self {
            registry_suffix: ".dkr.ecr.".to_string(),
        }
    }
}

impl Default for EcrCredentialProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RegistryCredentialProvider for EcrCredentialProvider {
    async fn auth_for(&self, registry: &str) -> Result<RegistryAuth, OciError> {
        if !registry.contains(&self.registry_suffix) {
            return Ok(RegistryAuth::Anonymous);
        }
        let username = std::env::var("DENIA_ECR_USERNAME").unwrap_or_else(|_| "AWS".to_string());
        match std::env::var("DENIA_ECR_PASSWORD") {
            Ok(password) => Ok(RegistryAuth::Basic(username, password)),
            Err(_) => Err(OciError::Pull(
                "DENIA_ECR_PASSWORD missing — set it via `aws ecr get-login-password`".to_string(),
            )),
        }
    }
}
