use std::path::Path;

use super::{OciError, OciImagePuller, OciRootfsUnpacker, RegistryAuth};

fn pinned_digest(image: &str) -> Option<&str> {
    image.split_once('@').map(|(_, d)| d)
}

/// Pull `image`, unpack into `<traefik_dir>/rootfs` (swap into place), verify the binary
/// at `binary_rel` exists, and record the digest in `<traefik_dir>/.image-digest`.
/// Returns the image digest. Skips work if the cached digest matches the pinned
/// digest in `image` and the binary is already present.
pub async fn pull_image_to_dir(
    puller: &dyn OciImagePuller,
    unpacker: &dyn OciRootfsUnpacker,
    image: &str,
    auth: RegistryAuth,
    traefik_dir: &Path,
    binary_rel: &str,
) -> Result<String, OciError> {
    std::fs::create_dir_all(traefik_dir)?;
    let rootfs = traefik_dir.join("rootfs");
    let digest_file = traefik_dir.join(".image-digest");
    let binary = rootfs.join(binary_rel);

    if let Some(pinned) = pinned_digest(image)
        && binary.exists()
        && let Ok(cached) = std::fs::read_to_string(&digest_file)
        && cached.trim() == pinned
    {
        return Ok(pinned.to_string());
    }

    let pulled = puller.pull(image, auth).await?;

    let staging = tempfile::TempDir::new_in(traefik_dir).map_err(OciError::Io)?;
    let staged_rootfs = staging.path().join("rootfs");
    unpacker.unpack(&pulled.layers, &staged_rootfs)?;

    if !staged_rootfs.join(binary_rel).exists() {
        return Err(OciError::Pull(format!(
            "traefik binary missing at {binary_rel} after unpack"
        )));
    }

    if rootfs.exists() {
        std::fs::remove_dir_all(&rootfs)?;
    }
    std::fs::rename(&staged_rootfs, &rootfs)?;
    std::fs::write(&digest_file, &pulled.digest)?;

    Ok(pulled.digest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::super::{LayerBlob, PulledImage};

    struct FakePuller {
        digest: String,
        calls: AtomicUsize,
    }
    #[async_trait]
    impl OciImagePuller for FakePuller {
        async fn pull(&self, _image: &str, _auth: RegistryAuth) -> Result<PulledImage, OciError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(PulledImage {
                digest: self.digest.clone(),
                config: serde_json::from_str("{}").unwrap(),
                layers: vec![LayerBlob {
                    digest: "sha256:layer".into(),
                    compression: super::super::LayerCompression::None,
                    path: PathBuf::from("/dev/null"),
                }],
                _staging: None,
            })
        }
        async fn read_layout(&self, _d: &Path) -> Result<PulledImage, OciError> {
            unreachable!()
        }
    }

    struct FakeUnpacker;
    impl OciRootfsUnpacker for FakeUnpacker {
        fn unpack(&self, _layers: &[LayerBlob], rootfs_dir: &Path) -> Result<(), OciError> {
            let bin = rootfs_dir.join("usr/local/bin");
            fs::create_dir_all(&bin)?;
            fs::write(bin.join("traefik"), b"#!/bin/true\n")?;
            Ok(())
        }
    }

    #[tokio::test]
    async fn pulls_unpacks_and_records_digest() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("traefik");
        let puller = FakePuller {
            digest: "sha256:abc".into(),
            calls: AtomicUsize::new(0),
        };
        let digest = pull_image_to_dir(
            &puller,
            &FakeUnpacker,
            "docker.io/library/traefik:v3.3",
            RegistryAuth::Anonymous,
            &dir,
            "usr/local/bin/traefik",
        )
        .await
        .unwrap();
        assert_eq!(digest, "sha256:abc");
        assert!(dir.join("rootfs/usr/local/bin/traefik").exists());
        assert_eq!(
            fs::read_to_string(dir.join(".image-digest")).unwrap(),
            "sha256:abc"
        );
        assert_eq!(puller.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn cache_hit_skips_pull_for_pinned_digest() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("traefik");
        fs::create_dir_all(dir.join("rootfs/usr/local/bin")).unwrap();
        fs::write(dir.join("rootfs/usr/local/bin/traefik"), b"x").unwrap();
        fs::write(dir.join(".image-digest"), "sha256:pinned").unwrap();
        let puller = FakePuller {
            digest: "sha256:pinned".into(),
            calls: AtomicUsize::new(0),
        };
        let digest = pull_image_to_dir(
            &puller,
            &FakeUnpacker,
            "docker.io/library/traefik@sha256:pinned",
            RegistryAuth::Anonymous,
            &dir,
            "usr/local/bin/traefik",
        )
        .await
        .unwrap();
        assert_eq!(digest, "sha256:pinned");
        assert_eq!(
            puller.calls.load(Ordering::SeqCst),
            0,
            "must skip pull on cache hit"
        );
    }

    #[tokio::test]
    async fn missing_binary_after_unpack_errors() {
        struct EmptyUnpacker;
        impl OciRootfsUnpacker for EmptyUnpacker {
            fn unpack(&self, _l: &[LayerBlob], _r: &Path) -> Result<(), OciError> {
                Ok(())
            }
        }
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("traefik");
        let puller = FakePuller {
            digest: "sha256:x".into(),
            calls: AtomicUsize::new(0),
        };
        let err = pull_image_to_dir(
            &puller,
            &EmptyUnpacker,
            "docker.io/library/traefik:v3.3",
            RegistryAuth::Anonymous,
            &dir,
            "usr/local/bin/traefik",
        )
        .await;
        assert!(err.is_err());
        assert!(
            !dir.join(".image-digest").exists(),
            "digest must not be written on failure"
        );
        assert!(matches!(err.unwrap_err(), OciError::Pull(_)));
    }
}
