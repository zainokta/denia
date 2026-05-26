use std::io::Read;
use std::path::Path;

use sha2::{Digest, Sha256};

use super::{LayerBlob, LayerCompression, OciError, PulledImage};

pub fn read_oci_layout(layout_dir: &Path) -> Result<PulledImage, OciError> {
    let index_path = layout_dir.join("index.json");
    let index_bytes = std::fs::read(&index_path)
        .map_err(|e| OciError::Layout(format!("cannot read index.json: {e}")))?;

    let index: serde_json::Value = serde_json::from_slice(&index_bytes)
        .map_err(|e| OciError::Layout(format!("invalid index.json: {e}")))?;

    let manifests = index["manifests"]
        .as_array()
        .ok_or_else(|| OciError::Layout("no manifests in index.json".to_string()))?;

    let manifest_desc = manifests
        .first()
        .ok_or_else(|| OciError::Layout("empty manifests array in index.json".to_string()))?;
    let digest = manifest_desc["digest"]
        .as_str()
        .ok_or_else(|| OciError::Layout("manifest missing digest".to_string()))?;

    let digest_hex = digest
        .strip_prefix("sha256:")
        .ok_or_else(|| OciError::Layout(format!("unsupported digest: {digest}")))?;

    let manifest_path = layout_dir.join("blobs").join("sha256").join(digest_hex);
    let manifest_bytes = std::fs::read(&manifest_path)
        .map_err(|e| OciError::Layout(format!("cannot read manifest blob: {e}")))?;

    let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes)
        .map_err(|e| OciError::Layout(format!("invalid manifest: {e}")))?;

    let config_digest = manifest["config"]["digest"]
        .as_str()
        .ok_or_else(|| OciError::Layout("config missing digest".to_string()))?;

    let config_hex = config_digest
        .strip_prefix("sha256:")
        .ok_or_else(|| OciError::Layout(format!("unsupported config digest: {config_digest}")))?;

    let config_path = layout_dir.join("blobs").join("sha256").join(config_hex);
    let config_bytes = std::fs::read(&config_path)
        .map_err(|e| OciError::Layout(format!("cannot read config blob: {e}")))?;

    let config: super::config::OciImageConfig =
        serde_json::from_slice(&config_bytes).map_err(OciError::Json)?;

    let mut layers = Vec::new();
    if let Some(layer_list) = manifest["layers"].as_array() {
        for layer in layer_list {
            let layer_digest = layer["digest"]
                .as_str()
                .ok_or_else(|| OciError::Layout("layer missing digest".to_string()))?;
            let layer_hex = layer_digest.strip_prefix("sha256:").ok_or_else(|| {
                OciError::Layout(format!("unsupported layer digest: {layer_digest}"))
            })?;

            let media_type = layer["mediaType"].as_str().unwrap_or("");
            let compression = match media_type {
                t if t.contains("gzip") || t.contains("+gzip") => LayerCompression::Gzip,
                t if t.contains("zstd") || t.contains("+zstd") => LayerCompression::Zstd,
                _ => LayerCompression::None,
            };

            let blob_path = layout_dir.join("blobs").join("sha256").join(layer_hex);
            verify_blob_digest(&blob_path, layer_digest)?;

            layers.push(LayerBlob {
                digest: layer_digest.to_string(),
                compression,
                path: blob_path,
            });
        }
    }

    Ok(PulledImage {
        digest: digest.to_string(),
        config,
        layers,
        _staging: None,
    })
}

fn verify_blob_digest(path: &Path, expected: &str) -> Result<(), OciError> {
    let mut file = std::fs::File::open(path)
        .map_err(|e| OciError::Layout(format!("cannot read layer blob: {e}")))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(OciError::Io)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    let actual = format!("sha256:{}", hex::encode(hasher.finalize()));
    if actual != expected {
        return Err(OciError::DigestMismatch {
            expected: expected.to_string(),
            actual,
        });
    }
    Ok(())
}
