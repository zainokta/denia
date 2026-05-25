use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    artifacts::{ArtifactError, ArtifactKind, ArtifactRecord, ArtifactSource},
    command::{CommandError, CommandRunner},
    config::AppConfig,
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
}

#[derive(Debug, Clone)]
pub struct ArtifactAcquirer {
    config: AppConfig,
}

impl ArtifactAcquirer {
    pub fn new(config: AppConfig) -> Self {
        Self { config }
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
            .materialize_rootfs_bundle(runner, &image_artifact)
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
        let image_artifact = self.acquire(runner, request).await?;
        let bundle_dir = self
            .materialize_rootfs_bundle(runner, &image_artifact)
            .await?;
        let config = self.inspect_image_config(runner).await?;
        let process = RootfsBundleSpec::try_from(config)?;
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
        runner: &dyn CommandRunner,
        source: &ArtifactSource,
    ) -> Result<String, ArtifactAcquireError> {
        let ArtifactSource::ExternalRegistry { image } = source else {
            unreachable!("external image acquisition requires a registry source");
        };
        let from = format!("docker://{image}");
        let to = format!("oci:{}", self.config.artifact_dir.to_string_lossy());
        let program = self.config.registry_pull_binary.to_string_lossy();
        let args = ["copy", from.as_str(), to.as_str()];

        let output = runner.run(program.as_ref(), &args).await?;
        Ok(output.stdout.trim().to_string())
    }

    async fn materialize_rootfs_bundle(
        &self,
        runner: &dyn CommandRunner,
        artifact: &ArtifactRecord,
    ) -> Result<std::path::PathBuf, ArtifactAcquireError> {
        let bundle_name = safe_artifact_name(&artifact.digest);
        let bundle_dir = self.config.artifact_dir.join(bundle_name);
        std::fs::create_dir_all(&bundle_dir)?;

        let image = format!("oci:{}", self.config.artifact_dir.to_string_lossy());
        let bundle = bundle_dir.to_string_lossy();
        let program = self.config.oci_unpack_binary.to_string_lossy();
        runner
            .run(
                program.as_ref(),
                &["unpack", "--image", image.as_str(), bundle.as_ref()],
            )
            .await?;
        std::fs::create_dir_all(bundle_dir.join("rootfs"))?;

        let base = self.config.userns_base.to_string();
        let owner = format!("{base}:{base}");
        let rootfs = bundle_dir.join("rootfs");
        let rootfs_str = rootfs.to_string_lossy().into_owned();
        runner
            .run("chown", &["-R", "--no-dereference", &owner, &rootfs_str])
            .await?;

        Ok(bundle_dir)
    }

    async fn inspect_image_config(
        &self,
        runner: &dyn CommandRunner,
    ) -> Result<OciImageConfig, ArtifactAcquireError> {
        let image = format!("oci:{}", self.config.artifact_dir.to_string_lossy());
        let program = self.config.registry_pull_binary.to_string_lossy();
        let output = runner
            .run(program.as_ref(), &["inspect", "--config", image.as_str()])
            .await?;
        Ok(serde_json::from_str(&output.stdout)?)
    }
}

impl TryFrom<OciImageConfig> for RootfsBundleSpec {
    type Error = ArtifactAcquireError;

    fn try_from(value: OciImageConfig) -> Result<Self, Self::Error> {
        let mut argv = value.config.entrypoint;
        argv.extend(value.config.cmd);
        if argv.is_empty() {
            return Err(ArtifactAcquireError::MissingProcessArgv);
        }

        let mut env = Vec::new();
        for entry in value.config.env {
            let Some((key, value)) = entry.split_once('=') else {
                return Err(ArtifactAcquireError::InvalidEnvironmentEntry { entry });
            };
            if key.is_empty() {
                return Err(ArtifactAcquireError::InvalidEnvironmentEntry { entry });
            }
            env.push((key.to_string(), value.to_string()));
        }

        Ok(Self {
            argv,
            env,
            workdir: value.config.workdir,
        })
    }
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
