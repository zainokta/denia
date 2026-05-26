pub mod config;
pub mod credentials;
pub mod layout;
pub mod registry;
pub mod unpack;

use async_trait::async_trait;
use std::path::Path;
use thiserror::Error;

pub use config::OciImageConfig;
pub use oci_client::secrets::RegistryAuth;

#[derive(Debug, Clone)]
pub enum LayerCompression {
    Gzip,
    Zstd,
    None,
}

#[derive(Debug)]
pub struct LayerBlob {
    pub digest: String,
    pub compression: LayerCompression,
    pub path: std::path::PathBuf,
}

#[derive(Debug)]
pub struct PulledImage {
    pub digest: String,
    pub config: OciImageConfig,
    pub layers: Vec<LayerBlob>,
    pub _staging: Option<tempfile::TempDir>,
}

#[derive(Debug, Error)]
pub enum OciError {
    #[error("registry pull failed: {0}")]
    Pull(String),
    #[error("digest mismatch: expected {expected}, got {actual}")]
    DigestMismatch { expected: String, actual: String },
    #[error("unsafe path in layer: {0}")]
    UnsafePath(String),
    #[error("oci layout error: {0}")]
    Layout(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

#[async_trait]
pub trait OciImagePuller: Send + Sync {
    async fn pull(&self, image: &str, auth: RegistryAuth) -> Result<PulledImage, OciError>;
    async fn read_layout(&self, layout_dir: &Path) -> Result<PulledImage, OciError>;
}

pub trait OciRootfsUnpacker: Send + Sync {
    fn unpack(&self, layers: &[LayerBlob], rootfs_dir: &Path) -> Result<(), OciError>;
}
