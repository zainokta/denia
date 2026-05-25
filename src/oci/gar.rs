use async_trait::async_trait;
use oci_client::secrets::RegistryAuth;

use super::{OciError, credentials::RegistryCredentialProvider};

/// Google Artifact Registry credential provider.
///
/// Behind the `gar` cargo feature. Reads an access token from
/// `DENIA_GAR_ACCESS_TOKEN`. Operators populate via
/// `gcloud auth print-access-token` (or a workload-identity hook).
///
/// Metadata-server bearer exchange is deferred — landing the full
/// `gcp_auth` crate is a separate ADR-011 follow-up.
pub struct GarCredentialProvider {
    registry_suffix: String,
}

impl GarCredentialProvider {
    pub fn new() -> Self {
        Self {
            registry_suffix: "-docker.pkg.dev".to_string(),
        }
    }
}

impl Default for GarCredentialProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RegistryCredentialProvider for GarCredentialProvider {
    async fn auth_for(&self, registry: &str) -> Result<RegistryAuth, OciError> {
        if !registry.ends_with(&self.registry_suffix) {
            return Ok(RegistryAuth::Anonymous);
        }
        match std::env::var("DENIA_GAR_ACCESS_TOKEN") {
            Ok(token) => Ok(RegistryAuth::Basic("oauth2accesstoken".to_string(), token)),
            Err(_) => Err(OciError::Pull(
                "DENIA_GAR_ACCESS_TOKEN missing — set via `gcloud auth print-access-token`"
                    .to_string(),
            )),
        }
    }
}
