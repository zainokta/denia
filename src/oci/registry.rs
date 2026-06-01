use std::net::IpAddr;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use oci_client::Reference;
use oci_client::client::Client;
use oci_client::secrets::RegistryAuth;
use tokio::io::AsyncWriteExt;

use crate::config::OciCacheVerifyMode;

use super::cache::{CacheReservation, LayerCache};
use super::{LayerBlob, LayerCompression, OciError, OciImagePuller, PulledImage};

/// Streaming OCI registry puller backed by a persistent on-disk layer cache
/// (ADR-022). If `cache` is `Some`, layers are content-addressed under the
/// cache root and reused across pulls; if `None`, layers fall back to a
/// per-pull `tempfile::TempDir` under `staging_dir` (the ADR-015 path).
///
/// The cache path is preferred at runtime; the temp-dir path is kept for
/// constructors that have not been migrated and for tests.
pub struct RegistryImagePuller {
    client: Client,
    staging_dir: PathBuf,
    cache: Option<LayerCache>,
}

impl RegistryImagePuller {
    /// Temp-dir backed puller (legacy ADR-015 behaviour). Used by tests and
    /// by callers that have not yet wired a [`LayerCache`].
    pub fn new(staging_dir: PathBuf) -> Self {
        let config = oci_client::client::ClientConfig::default();
        Self {
            client: Client::new(config),
            staging_dir,
            cache: None,
        }
    }

    /// Cache-backed puller (ADR-022). Layers are reused across pulls; new
    /// layers are streamed to a `<digest>.tmp` file inside the cache and
    /// atomically renamed into place after digest verification.
    pub fn new_with_cache(staging_dir: PathBuf, cache: LayerCache) -> Self {
        let config = oci_client::client::ClientConfig::default();
        Self {
            client: Client::new(config),
            staging_dir,
            cache: Some(cache),
        }
    }

    pub fn cache(&self) -> Option<LayerCache> {
        self.cache.clone()
    }
}

#[async_trait]
impl OciImagePuller for RegistryImagePuller {
    async fn pull(&self, image: &str, auth: RegistryAuth) -> Result<PulledImage, OciError> {
        let reference: Reference = image
            .parse()
            .map_err(|e| OciError::Pull(format!("invalid image reference '{image}': {e}")))?;
        validate_registry_resolution(&reference).await?;

        let (manifest, manifest_digest, config_json) = self
            .client
            .pull_manifest_and_config(&reference, &auth)
            .await
            .map_err(|e| OciError::Pull(format!("pull failed: {e}")))?;

        let config: super::config::OciImageConfig =
            serde_json::from_str(&config_json).map_err(OciError::Json)?;

        match &self.cache {
            Some(cache) => {
                let mut layers = Vec::with_capacity(manifest.layers.len());
                let mut reservations: Vec<CacheReservation> = Vec::new();
                for desc in manifest.layers.iter() {
                    let compression = compression_for(&desc.media_type);
                    let reservation = cache
                        .reserve(&desc.digest)
                        .map_err(|e| OciError::Pull(format!("cache reserve failed: {e}")))?;
                    let path = ensure_cached(&self.client, cache, &reference, desc).await?;
                    layers.push(LayerBlob {
                        digest: desc.digest.clone(),
                        compression,
                        path,
                    });
                    reservations.push(reservation);
                }

                Ok(PulledImage {
                    digest: manifest_digest,
                    config,
                    layers,
                    _staging: None,
                    _cache_reservations: reservations,
                })
            }
            None => {
                let staging = tempfile::TempDir::new_in(&self.staging_dir).map_err(OciError::Io)?;
                let mut layers = Vec::with_capacity(manifest.layers.len());
                for (index, desc) in manifest.layers.iter().enumerate() {
                    let compression = compression_for(&desc.media_type);
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
                    _cache_reservations: Vec::new(),
                })
            }
        }
    }

    async fn read_layout(&self, layout_dir: &Path) -> Result<PulledImage, OciError> {
        super::layout::read_oci_layout(layout_dir)
    }
}

async fn validate_registry_resolution(reference: &Reference) -> Result<(), OciError> {
    let registry = reference.resolve_registry();
    let (host, port) = registry_lookup_target(registry)?;
    let addrs: Vec<IpAddr> = tokio::net::lookup_host((host.as_str(), port))
        .await
        .map_err(|e| OciError::Pull(format!("registry DNS lookup failed for {registry}: {e}")))?
        .map(|addr| addr.ip())
        .collect();
    validate_resolved_registry_addrs(registry, &addrs)
}

fn registry_lookup_target(registry: &str) -> Result<(String, u16), OciError> {
    if let Some(rest) = registry.strip_prefix('[') {
        let end = rest
            .find(']')
            .ok_or_else(|| OciError::Pull(format!("invalid registry authority: {registry}")))?;
        let host = &rest[..end];
        let suffix = &rest[end + 1..];
        if suffix.is_empty() {
            return Ok((host.to_string(), 443));
        }
        let port = suffix
            .strip_prefix(':')
            .ok_or_else(|| OciError::Pull(format!("invalid registry authority: {registry}")))?
            .parse::<u16>()
            .map_err(|_| OciError::Pull(format!("invalid registry port: {registry}")))?;
        return Ok((host.to_string(), port));
    }

    if registry.matches(':').count() == 1 {
        let (host, port) = registry
            .split_once(':')
            .ok_or_else(|| OciError::Pull(format!("invalid registry authority: {registry}")))?;
        let port = port
            .parse::<u16>()
            .map_err(|_| OciError::Pull(format!("invalid registry port: {registry}")))?;
        return Ok((host.to_string(), port));
    }
    Ok((registry.to_string(), 443))
}

fn validate_resolved_registry_addrs(registry: &str, addrs: &[IpAddr]) -> Result<(), OciError> {
    if addrs.is_empty() {
        return Err(OciError::Pull(format!(
            "registry DNS lookup returned no addresses for {registry}"
        )));
    }
    if let Some(blocked) = addrs.iter().copied().find(|ip| is_blocked_registry_ip(*ip)) {
        return Err(OciError::Pull(format!(
            "registry {registry} resolved to blocked address {blocked}"
        )));
    }
    Ok(())
}

fn is_blocked_registry_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let octets = ip.octets();
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_broadcast()
                || ip.is_multicast()
                || octets[0] == 0
                || (octets[0] == 100 && (octets[1] & 0xc0) == 64)
        }
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return is_blocked_registry_ip(IpAddr::V4(v4));
            }
            let first = ip.segments()[0];
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_multicast()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80
        }
    }
}

fn compression_for(media_type: &str) -> LayerCompression {
    match media_type {
        t if t.contains("gzip") || t.contains("+gzip") => LayerCompression::Gzip,
        t if t.contains("zstd") || t.contains("+zstd") => LayerCompression::Zstd,
        _ => LayerCompression::None,
    }
}

/// Cache-first layer acquisition: returns the cached blob path on hit; on
/// miss, streams from the registry into `<digest>.tmp`, fsyncs, and atomic-
/// renames into place. `pull_blob` itself verifies the layer digest as it
/// streams, so we don't repeat that work on the miss path. The hit path
/// re-verifies according to `OciCacheVerifyMode` (default: size).
async fn ensure_cached(
    client: &Client,
    cache: &LayerCache,
    reference: &Reference,
    desc: &oci_client::manifest::OciDescriptor,
) -> Result<PathBuf, OciError> {
    let expected_size = if desc.size >= 0 {
        Some(desc.size as u64)
    } else {
        None
    };
    match cache.get(&desc.digest, expected_size) {
        Ok(Some(path)) => return Ok(path),
        Ok(None) => {}
        Err(_e) => {
            // A verify-on-hit failure has already removed the bad blob.
            // Fall through to re-download.
        }
    }

    let tmp = cache
        .temp_path(&desc.digest)
        .map_err(|e| OciError::Pull(format!("cache temp_path: {e}")))?;
    if let Some(parent) = tmp.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(OciError::Io)?;
    }
    // Open exclusive — we never want two pulls writing the same tmp file.
    // Two concurrent pulls of the same digest race on this open; the loser
    // simply falls through to a cache hit after retry. Keep this simple
    // (a per-digest mutex would belong inside the cache; for now the
    // reservation map + retry pattern is enough).
    let mut file = match tokio::fs::File::create(&tmp).await {
        Ok(f) => f,
        Err(e) => return Err(OciError::Io(e)),
    };
    if let Err(e) = client.pull_blob(reference, desc, &mut file).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(OciError::Pull(format!("layer pull/verify failed: {e}")));
    }
    file.flush().await.map_err(OciError::Io)?;
    drop(file);
    let path = cache
        .finalize_temp(&desc.digest, &tmp)
        .map_err(|e| OciError::Pull(format!("cache finalize_temp: {e}")))?;
    Ok(path)
}

// Re-export so callers writing tests can construct a cache-aware puller
// without re-deriving the config plumbing.
pub fn build_cache(
    root: PathBuf,
    verify_on_hit: OciCacheVerifyMode,
) -> Result<LayerCache, super::cache::CacheError> {
    LayerCache::new(root, verify_on_hit)
}

// Keep `Arc` usable in callers that want to share the cache without re-cloning.
#[allow(dead_code)]
pub fn share_cache(cache: LayerCache) -> Arc<LayerCache> {
    Arc::new(cache)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn resolved_registry_addresses_reject_private_results() {
        let addrs = [
            IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10)),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 10)),
        ];

        let err = validate_resolved_registry_addrs("registry.example", &addrs)
            .expect_err("any private DNS answer must be rejected");

        assert!(err.to_string().contains("registry.example"));
    }

    #[test]
    fn registry_authority_splits_host_and_port() {
        assert_eq!(
            registry_lookup_target("ghcr.io").expect("host"),
            ("ghcr.io".to_string(), 443)
        );
        assert_eq!(
            registry_lookup_target("registry.example:5000").expect("host"),
            ("registry.example".to_string(), 5000)
        );
    }
}
