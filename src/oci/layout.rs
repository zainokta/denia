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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Write `bytes` into `blobs/sha256/<sha256(bytes)>` and return the
    /// canonical `sha256:<hex>` digest. The blob is filed under its true
    /// content digest, so any descriptor referencing the returned digest is
    /// internally consistent.
    fn write_blob(layout_dir: &Path, bytes: &[u8]) -> String {
        let hex_digest = hex::encode(Sha256::digest(bytes));
        let blobs_dir = layout_dir.join("blobs").join("sha256");
        std::fs::create_dir_all(&blobs_dir).unwrap();
        std::fs::write(blobs_dir.join(&hex_digest), bytes).unwrap();
        format!("sha256:{hex_digest}")
    }

    /// Write `bytes` under `blobs/sha256/<hex>` where `hex` is the hex portion
    /// of `claimed_digest`, regardless of the real content hash. Used to forge
    /// a layer whose file contents do not match its declared digest.
    fn write_blob_at(layout_dir: &Path, claimed_digest: &str, bytes: &[u8]) {
        let hex_digest = claimed_digest.strip_prefix("sha256:").unwrap();
        let blobs_dir = layout_dir.join("blobs").join("sha256");
        std::fs::create_dir_all(&blobs_dir).unwrap();
        std::fs::write(blobs_dir.join(hex_digest), bytes).unwrap();
    }

    #[test]
    fn read_layout_rejects_digest_mismatched_layer() {
        let tmp = tempfile::tempdir().unwrap();
        let dir: &Path = tmp.path();

        // Minimal valid OCI image config: both fields are Option, so `{}`
        // deserializes into OciImageConfig. Filed under its true digest.
        let config_bytes = b"{}";
        let config_digest = write_blob(dir, config_bytes);

        // The layer blob: its real bytes hash to one value, but the manifest
        // will declare a *different* digest for it, so verification must fail.
        let real_layer_bytes = b"actual layer payload";
        let real_layer_digest = format!("sha256:{}", hex::encode(Sha256::digest(real_layer_bytes)));
        // A deliberately wrong digest the manifest will claim for the layer.
        let claimed_layer_digest =
            "sha256:0000000000000000000000000000000000000000000000000000000000000000";
        assert_ne!(real_layer_digest, claimed_layer_digest);
        // File the layer bytes under the *claimed* hex path so the blob exists
        // where read_oci_layout looks, but its contents do not hash to it.
        write_blob_at(dir, claimed_layer_digest, real_layer_bytes);

        // Manifest references the (correct) config digest and the (wrong)
        // layer digest. Filed under its own true digest so the index→manifest
        // and manifest→config path lookups all resolve.
        let manifest_json = serde_json::json!({
            "config": {
                "digest": config_digest,
                "mediaType": "application/vnd.oci.image.config.v1+json"
            },
            "layers": [
                {
                    "digest": claimed_layer_digest,
                    "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip"
                }
            ]
        });
        let manifest_bytes = serde_json::to_vec(&manifest_json).unwrap();
        let manifest_digest = write_blob(dir, &manifest_bytes);

        // index.json points at the manifest by its true digest.
        let index_json = serde_json::json!({
            "manifests": [ { "digest": manifest_digest } ]
        });
        std::fs::write(
            dir.join("index.json"),
            serde_json::to_vec(&index_json).unwrap(),
        )
        .unwrap();

        let err = read_oci_layout(dir).expect_err("tampered layer must be rejected");
        match err {
            OciError::DigestMismatch { expected, actual } => {
                assert_eq!(expected, claimed_layer_digest);
                assert_eq!(actual, real_layer_digest);
            }
            other => panic!("expected DigestMismatch, got {other:?}"),
        }

        // Keep PathBuf import meaningful: assert the forged blob existed.
        let forged: PathBuf = dir
            .join("blobs")
            .join("sha256")
            .join(claimed_layer_digest.strip_prefix("sha256:").unwrap());
        assert!(forged.exists());
    }
}
