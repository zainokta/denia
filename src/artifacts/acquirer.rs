use thiserror::Error;

use crate::{
    artifacts::{ArtifactError, ArtifactKind, ArtifactRecord, ArtifactSource},
    command::{CommandError, CommandRunner},
    config::AppConfig,
};

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
        let git_context = format!("context={repo_url}#{git_ref}:{context_path}");
        let dockerfile = format!("filename={dockerfile_path}");
        let output_path = self.config.artifact_dir.join("buildkit-output");
        let output = format!("type=oci,dest={}", output_path.to_string_lossy());
        let program = self.config.buildkit_binary.to_string_lossy();
        let args = [
            "build",
            "--frontend",
            "dockerfile.v0",
            "--opt",
            git_context.as_str(),
            "--opt",
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
        let image_name = image.replace(['/', ':', '@'], "_");
        let to = format!(
            "oci:{}/{}",
            self.config.artifact_dir.to_string_lossy(),
            image_name
        );
        let program = self.config.registry_pull_binary.to_string_lossy();
        let args = ["copy", from.as_str(), to.as_str()];

        let output = runner.run(program.as_ref(), &args).await?;
        Ok(output.stdout.trim().to_string())
    }
}
