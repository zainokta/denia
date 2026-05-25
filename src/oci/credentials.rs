use async_trait::async_trait;
use oci_client::secrets::RegistryAuth;

use super::OciError;

#[async_trait]
pub trait RegistryCredentialProvider: Send + Sync {
    async fn auth_for(&self, registry: &str) -> Result<RegistryAuth, OciError>;
}

pub struct StaticCredentialProvider {
    credentials: std::collections::HashMap<String, (String, String)>,
}

impl StaticCredentialProvider {
    pub fn new() -> Self {
        Self {
            credentials: std::collections::HashMap::new(),
        }
    }

    pub fn with_credentials(
        mut self,
        registry: impl Into<String>,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        self.credentials
            .insert(registry.into(), (username.into(), password.into()));
        self
    }
}

#[async_trait]
impl RegistryCredentialProvider for StaticCredentialProvider {
    async fn auth_for(&self, registry: &str) -> Result<RegistryAuth, OciError> {
        if let Some((username, password)) = self.credentials.get(registry) {
            Ok(RegistryAuth::Basic(username.clone(), password.clone()))
        } else {
            Ok(RegistryAuth::Anonymous)
        }
    }
}
