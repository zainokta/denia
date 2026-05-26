use std::path::Path;
use std::path::PathBuf;

use async_trait::async_trait;
use oci_client::Reference;
use oci_client::client::Client;
use oci_client::secrets::RegistryAuth;

use super::{LayerBlob, LayerCompression, OciError, OciImagePuller, PulledImage};

pub struct RegistryImagePuller {
    client: Client,
    staging_dir: PathBuf,
}

impl RegistryImagePuller {
    pub fn new(staging_dir: PathBuf) -> Self {
        let config = oci_client::client::ClientConfig::default();
        Self {
            client: Client::new(config),
            staging_dir,
        }
    }
}

#[async_trait]
impl OciImagePuller for RegistryImagePuller {
    async fn pull(&self, image: &str, auth: RegistryAuth) -> Result<PulledImage, OciError> {
        let reference: Reference = image
            .parse()
            .map_err(|e| OciError::Pull(format!("invalid image reference '{image}': {e}")))?;

        let (manifest, manifest_digest, config_json) = self
            .client
            .pull_manifest_and_config(&reference, &auth)
            .await
            .map_err(|e| OciError::Pull(format!("pull failed: {e}")))?;

        let config: super::config::OciImageConfig =
            serde_json::from_str(&config_json).map_err(OciError::Json)?;

        let staging = tempfile::TempDir::new_in(&self.staging_dir).map_err(OciError::Io)?;

        let mut layers = Vec::with_capacity(manifest.layers.len());
        for (index, desc) in manifest.layers.iter().enumerate() {
            let compression = match desc.media_type.as_str() {
                t if t.contains("gzip") || t.contains("+gzip") => LayerCompression::Gzip,
                t if t.contains("zstd") || t.contains("+zstd") => LayerCompression::Zstd,
                _ => LayerCompression::None,
            };

            let layer_path = staging.path().join(format!("layer-{index}"));
            let file = tokio::fs::File::create(&layer_path)
                .await
                .map_err(OciError::Io)?;
            self.client
                .pull_blob(&reference, desc, file)
                .await
                .map_err(|e| OciError::Pull(format!("layer pull/verify failed: {e}")))?;

            layers.push(LayerBlob {
                digest: desc.digest.clone(),
                compression,
                path: layer_path,
            });
        }

        Ok(PulledImage {
            digest: manifest_digest,
            config,
            layers,
            _staging: Some(staging),
        })
    }

    async fn read_layout(&self, layout_dir: &Path) -> Result<PulledImage, OciError> {
        super::layout::read_oci_layout(layout_dir)
    }
}
