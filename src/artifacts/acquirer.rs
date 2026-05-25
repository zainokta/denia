use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    artifacts::{ArtifactError, ArtifactKind, ArtifactRecord, ArtifactSource},
    command::{CommandError, CommandRunner},
    config::AppConfig,
    oci::{
        OciError, OciImagePuller, OciRootfsUnpacker, credentials::StaticCredentialProvider,
        registry::RegistryImagePuller, unpack::TarRootfsUnpacker,
    },
    syscall,
};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct OciImageConfig {
    pub config: OciImageProcessConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct OciImageProcessConfig {
    #[serde(default, rename = "Entrypoint")]
    pub entrypoint: Vec<String>,
    #[serde(default, rename = "Cmd")]
    pub cmd: Vec<String>,
    #[serde(default, rename = "Env")]
    pub env: Vec<String>,
    #[serde(default = "default_workdir", rename = "WorkingDir")]
    pub workdir: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootfsBundleSpec {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub workdir: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactAcquireRequest {
    Git {
        repo_url: String,
        git_ref: String,
        dockerfile_path: String,
        context_path: String,
    },
    ExternalImage {
        image: String,
    },
}

#[derive(Debug, Error)]
pub enum ArtifactAcquireError {
    #[error(transparent)]
    Command(#[from] CommandError),
    #[error(transparent)]
    Artifact(#[from] ArtifactError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("image config has no entrypoint or cmd")]
    MissingProcessArgv,
    #[error("image config environment entry is invalid: {entry}")]
    InvalidEnvironmentEntry { entry: String },
    #[error("oci error: {0}")]
    Oci(#[from] OciError),
}

#[derive(Clone)]
pub struct ArtifactAcquirer {
    config: AppConfig,
    puller: Arc<dyn OciImagePuller>,
    unpacker: Arc<dyn OciRootfsUnpacker>,
}

impl ArtifactAcquirer {
    pub fn new(config: AppConfig) -> Self {
        let credentials = Arc::new(StaticCredentialProvider::new());
        Self {
            config,
            puller: Arc::new(RegistryImagePuller::new(credentials)),
            unpacker: Arc::new(TarRootfsUnpacker::new()),
        }
    }

    pub fn with_traits(
        config: AppConfig,
        puller: Arc<dyn OciImagePuller>,
        unpacker: Arc<dyn OciRootfsUnpacker>,
    ) -> Self {
        Self {
            config,
            puller,
            unpacker,
        }
    }

    pub async fn acquire(
        &self,
        runner: &dyn CommandRunner,
        request: ArtifactAcquireRequest,
    ) -> Result<ArtifactRecord, ArtifactAcquireError> {
        match request {
            ArtifactAcquireRequest::Git {
                repo_url,
                git_ref,
                dockerfile_path,
                context_path,
            } => {
                let source = ArtifactSource::BuildKit {
                    repo_url,
                    git_ref,
                    dockerfile_path,
                    context_path,
                };
                let digest = self.acquire_git(runner, &source).await?;
                Ok(ArtifactRecord::new(digest, ArtifactKind::OciImage, source)?)
            }
            ArtifactAcquireRequest::ExternalImage { image } => {
                let source = ArtifactSource::ExternalRegistry { image };
                let digest = self.acquire_external_image(runner, &source).await?;
                Ok(ArtifactRecord::new(digest, ArtifactKind::OciImage, source)?)
            }
        }
    }

    pub async fn acquire_rootfs_bundle(
        &self,
        runner: &dyn CommandRunner,
        request: ArtifactAcquireRequest,
        process: RootfsBundleSpec,
    ) -> Result<ArtifactRecord, ArtifactAcquireError> {
        let image_artifact = self.acquire(runner, request).await?;
        let bundle_dir = self
            .materialize_rootfs_bundle_inprocess(&image_artifact)
            .await?;
        std::fs::write(
            bundle_dir.join("process.json"),
            serde_json::to_vec_pretty(&process)?,
        )?;

        ArtifactRecord::new(
            image_artifact.digest,
            ArtifactKind::RootfsBundle,
            image_artifact.source,
        )
        .map_err(ArtifactAcquireError::Artifact)
    }

    pub async fn acquire_rootfs_bundle_from_image_config(
        &self,
        runner: &dyn CommandRunner,
        request: ArtifactAcquireRequest,
    ) -> Result<ArtifactRecord, ArtifactAcquireError> {
        match &request {
            ArtifactAcquireRequest::ExternalImage { image } => {
                let source = ArtifactSource::ExternalRegistry {
                    image: image.clone(),
                };
                self.pull_and_unpack_external(&source).await
            }
            ArtifactAcquireRequest::Git { .. } => {
                let image_artifact = self.acquire(runner, request).await?;
                let _bundle_dir = self
                    .materialize_rootfs_bundle_inprocess(&image_artifact)
                    .await?;
                ArtifactRecord::new(
                    image_artifact.digest,
                    ArtifactKind::RootfsBundle,
                    image_artifact.source,
                )
                .map_err(ArtifactAcquireError::Artifact)
            }
        }
    }

    async fn pull_and_unpack_external(
        &self,
        source: &ArtifactSource,
    ) -> Result<ArtifactRecord, ArtifactAcquireError> {
        let ArtifactSource::ExternalRegistry { image } = source else {
            unreachable!();
        };
        let pulled = self.puller.pull(image).await?;
        let digest = if pulled.digest.is_empty() {
            short_digest(image)
        } else {
            pulled.digest.clone()
        };
        let bundle_dir = self.write_bundle(&digest, &pulled.layers)?;
        let process = rootfs_bundle_from_oci_config(&pulled.config)?;
        std::fs::write(
            bundle_dir.join("process.json"),
            serde_json::to_vec_pretty(&process)?,
        )?;
        ArtifactRecord::new(digest, ArtifactKind::RootfsBundle, source.clone())
            .map_err(ArtifactAcquireError::Artifact)
    }

    async fn materialize_rootfs_bundle_inprocess(
        &self,
        artifact: &ArtifactRecord,
    ) -> Result<std::path::PathBuf, ArtifactAcquireError> {
        let layout = self.config.artifact_dir.clone();
        let pulled = self.puller.read_layout(&layout).await?;
        let bundle_dir = self.write_bundle(&artifact.digest, &pulled.layers)?;
        let process = rootfs_bundle_from_oci_config(&pulled.config)?;
        std::fs::write(
            bundle_dir.join("process.json"),
            serde_json::to_vec_pretty(&process)?,
        )?;
        Ok(bundle_dir)
    }

    fn write_bundle(
        &self,
        digest: &str,
        layers: &[crate::oci::LayerBlob],
    ) -> Result<std::path::PathBuf, ArtifactAcquireError> {
        let bundle_dir = self.config.artifact_dir.join(safe_artifact_name(digest));
        std::fs::create_dir_all(&bundle_dir)?;
        let rootfs = bundle_dir.join("rootfs");
        self.unpacker.unpack(layers, &rootfs)?;
        let base = self.config.userns_base;
        if let Err(error) = syscall::chown::recursive_lchown(&rootfs, base, base) {
            let io_err = error.to_string();
            if !io_err.contains("Operation not permitted") {
                return Err(ArtifactAcquireError::Io(std::io::Error::other(io_err)));
            }
        }
        Ok(bundle_dir)
    }

    async fn acquire_git(
        &self,
        runner: &dyn CommandRunner,
        source: &ArtifactSource,
    ) -> Result<String, ArtifactAcquireError> {
        let ArtifactSource::BuildKit {
            repo_url,
            git_ref,
            dockerfile_path,
            context_path,
        } = source
        else {
            unreachable!("git acquisition requires a buildkit source");
        };
        let _ = (repo_url, git_ref);
        let context = format!("context={context_path}");
        let dockerfile = format!("dockerfile={dockerfile_path}");
        let output = format!(
            "type=oci,dest={}",
            self.config.artifact_dir.to_string_lossy()
        );
        let program = self.config.buildkit_binary.to_string_lossy();
        let args = [
            "build",
            "--frontend",
            "dockerfile.v0",
            "--local",
            context.as_str(),
            "--local",
            dockerfile.as_str(),
            "--output",
            output.as_str(),
        ];

        let output = runner.run(program.as_ref(), &args).await?;
        Ok(output.stdout.trim().to_string())
    }

    async fn acquire_external_image(
        &self,
        _runner: &dyn CommandRunner,
        source: &ArtifactSource,
    ) -> Result<String, ArtifactAcquireError> {
        let ArtifactSource::ExternalRegistry { image } = source else {
            unreachable!("external image acquisition requires a registry source");
        };
        let pulled = self.puller.pull(image).await?;
        if pulled.digest.is_empty() {
            Ok(short_digest(image))
        } else {
            Ok(pulled.digest)
        }
    }
}

fn rootfs_bundle_from_oci_config(
    cfg: &crate::oci::config::OciImageConfig,
) -> Result<RootfsBundleSpec, ArtifactAcquireError> {
    let oci_spec = crate::oci::config::RootfsBundleSpec::try_from(cfg).map_err(|e| match e {
        crate::oci::config::ConfigError::MissingProcessConfig
        | crate::oci::config::ConfigError::MissingProcessArgv => {
            ArtifactAcquireError::MissingProcessArgv
        }
        crate::oci::config::ConfigError::InvalidEnvironmentEntry(entry) => {
            ArtifactAcquireError::InvalidEnvironmentEntry { entry }
        }
    })?;
    Ok(RootfsBundleSpec {
        argv: oci_spec.argv,
        env: oci_spec.env,
        workdir: oci_spec.workdir,
    })
}

fn short_digest(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(input.as_bytes());
    format!("sha256:{}", hex::encode(h.finalize()))
}

fn safe_artifact_name(digest: &str) -> String {
    digest
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
                character
            } else {
                '-'
            }
        })
        .collect()
}

fn default_workdir() -> String {
    "/".to_string()
}
