use std::path::Path;

use async_trait::async_trait;
use oci_client::Reference;
use oci_client::client::Client;
use oci_client::secrets::RegistryAuth;
use sha2::{Digest, Sha256};

use super::{LayerBlob, LayerCompression, OciError, OciImagePuller, PulledImage};

pub struct RegistryImagePuller {
    client: Client,
}

impl RegistryImagePuller {
    pub fn new() -> Self {
        let config = oci_client::client::ClientConfig::default();
        Self {
            client: Client::new(config),
        }
    }
}

impl Default for RegistryImagePuller {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl OciImagePuller for RegistryImagePuller {
    async fn pull(&self, image: &str, auth: RegistryAuth) -> Result<PulledImage, OciError> {
        let reference: Reference = image
            .parse()
            .map_err(|e| OciError::Pull(format!("invalid image reference '{image}': {e}")))?;

        let accepted: Vec<&str> = vec![
            "application/vnd.oci.image.manifest.v1+json",
            "application/vnd.docker.distribution.manifest.v2+json",
        ];

        let image_data = self
            .client
            .pull(&reference, &auth, accepted)
            .await
            .map_err(|e| OciError::Pull(format!("pull failed: {e}")))?;

        let config: super::config::OciImageConfig =
            serde_json::from_slice(&image_data.config.data).map_err(OciError::Json)?;

        let mut layers = Vec::new();
        for layer in &image_data.layers {
            let compression = match layer.media_type.as_str() {
                t if t.contains("gzip") || t.contains("+gzip") => LayerCompression::Gzip,
                t if t.contains("zstd") || t.contains("+zstd") => LayerCompression::Zstd,
                _ => LayerCompression::None,
            };

            let data = layer.data.to_vec();

            let mut hasher = Sha256::new();
            hasher.update(&data);
            let layer_digest = format!("sha256:{}", hex::encode(hasher.finalize()));

            layers.push(LayerBlob {
                digest: layer_digest,
                compression,
                data,
            });
        }

        Ok(PulledImage {
            digest: image_data.digest.unwrap_or_default(),
            config,
            layers,
        })
    }

    async fn read_layout(&self, layout_dir: &Path) -> Result<PulledImage, OciError> {
        super::layout::read_oci_layout(layout_dir)
    }
}
