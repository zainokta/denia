use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    artifacts::{ArtifactError, ArtifactKind, ArtifactRecord, ArtifactSource},
    command::{CommandError, CommandRunner},
    config::AppConfig,
    oci::{
        OciError, OciImagePuller, OciRootfsUnpacker, RegistryAuth, registry::RegistryImagePuller,
        unpack::TarRootfsUnpacker,
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
    Upload {
        upload_id: String,
        dockerfile_path: String,
        context_path: String,
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
        let staging_dir = config.artifact_dir.clone();
        // Prefer the persistent layer cache (ADR-022). Cache init failure is
        // not fatal — fall back to the per-pull TempDir staging path.
        let puller: Arc<dyn OciImagePuller> = match crate::oci::cache::LayerCache::new(
            config.oci_cache_dir.clone(),
            config.oci_cache_verify_on_hit,
        ) {
            Ok(cache) => Arc::new(RegistryImagePuller::new_with_cache(staging_dir, cache)),
            Err(e) => {
                eprintln!("oci layer cache disabled in acquirer ({e}); using per-pull TempDir");
                Arc::new(RegistryImagePuller::new(staging_dir))
            }
        };
        Self {
            config,
            puller,
            unpacker: Arc::new(TarRootfsUnpacker::new()),
        }
    }

    /// Same as [`Self::new`] but reuses an externally-built cache so the
    /// acquirer, the API observability endpoint, and the GC task all see
    /// the same `LayerCache` handle (and therefore the same reservation
    /// map).
    pub fn new_with_cache(config: AppConfig, cache: crate::oci::cache::LayerCache) -> Self {
        let staging_dir = config.artifact_dir.clone();
        Self {
            config,
            puller: Arc::new(RegistryImagePuller::new_with_cache(staging_dir, cache)),
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

    /// Acquire an artifact for the requested source.
    ///
    /// NOTE: the `ExternalImage` arm calls `puller.pull` with `RegistryAuth::Anonymous`.
    /// This entrypoint is not the deploy path for private registries —
    /// `deploy_external_image_source` uses `acquire_rootfs_bundle_from_image_config`
    /// (which threads explicit auth). If a future caller needs private-registry support
    /// via `acquire`, thread auth in instead of using this anonymous placeholder.
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
                let digest = self
                    .acquire_external_image(runner, &source, RegistryAuth::Anonymous)
                    .await?;
                Ok(ArtifactRecord::new(digest, ArtifactKind::OciImage, source)?)
            }
            ArtifactAcquireRequest::Upload {
                upload_id,
                dockerfile_path,
                context_path,
            } => {
                let source = ArtifactSource::UploadedContext {
                    upload_id,
                    dockerfile_path,
                    context_path,
                };
                let digest = self.acquire_staged(runner, &source).await?;
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

    /// Materializes a rootfs bundle for the given acquisition request.
    ///
    /// `auth` is only consumed on the `ExternalImage` arm; the `Git` arm builds
    /// via BuildKit and reads back from the local OCI layout, so it ignores
    /// `auth` entirely.
    pub async fn acquire_rootfs_bundle_from_image_config(
        &self,
        runner: &dyn CommandRunner,
        request: ArtifactAcquireRequest,
        auth: RegistryAuth,
    ) -> Result<ArtifactRecord, ArtifactAcquireError> {
        match &request {
            ArtifactAcquireRequest::ExternalImage { image } => {
                let source = ArtifactSource::ExternalRegistry {
                    image: image.clone(),
                };
                self.pull_and_unpack_external(&source, auth).await
            }
            ArtifactAcquireRequest::Git { .. } | ArtifactAcquireRequest::Upload { .. } => {
                let _ = auth;
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
        auth: RegistryAuth,
    ) -> Result<ArtifactRecord, ArtifactAcquireError> {
        let ArtifactSource::ExternalRegistry { image } = source else {
            unreachable!();
        };
        let pulled = self.puller.pull(image, auth).await?;
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
        // Persist the layer-digest list as a sidecar so the OCI cache GC can
        // tell which cached blobs are still referenced by promoted deployments
        // (ADR-022). Atomic write: tmp + rename.
        let layer_digests: Vec<String> = layers.iter().map(|l| l.digest.clone()).collect();
        let layers_json = bundle_dir.join("layers.json");
        let layers_tmp = bundle_dir.join("layers.json.tmp");
        std::fs::write(&layers_tmp, serde_json::to_vec_pretty(&layer_digests)?)?;
        std::fs::rename(&layers_tmp, &layers_json)?;
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

        // Clone the *declared* repo/ref into a Denia-owned checkout, then resolve
        // the build paths inside that checkout. This binds the build to the
        // declared git source and stops BuildKit from consuming arbitrary
        // host-local paths as context/dockerfile (F-1).
        let checkout = self
            .config
            .artifact_dir
            .join("git-checkouts")
            .join(uuid::Uuid::now_v7().to_string());
        std::fs::create_dir_all(&checkout)?;
        let checkout_str = checkout.to_string_lossy().into_owned();
        let git = self.config.git_binary.to_string_lossy().into_owned();

        let build_result = self
            .build_from_git_checkout(
                runner,
                &git,
                &checkout,
                &checkout_str,
                repo_url,
                git_ref,
                context_path,
                dockerfile_path,
            )
            .await;
        // Always clean up the checkout, success or failure.
        let _ = std::fs::remove_dir_all(&checkout);
        build_result
    }

    #[allow(clippy::too_many_arguments)]
    async fn build_from_git_checkout(
        &self,
        runner: &dyn CommandRunner,
        git: &str,
        checkout: &std::path::Path,
        checkout_str: &str,
        repo_url: &str,
        git_ref: &str,
        context_path: &str,
        dockerfile_path: &str,
    ) -> Result<String, ArtifactAcquireError> {
        runner
            .run(
                git,
                &[
                    "clone",
                    "--quiet",
                    "--no-checkout",
                    "--",
                    repo_url,
                    checkout_str,
                ],
            )
            .await?;
        runner
            .run(git, &["-C", checkout_str, "checkout", "--quiet", git_ref])
            .await?;

        let context_dir = confine_under(checkout, context_path)?;
        let dockerfile_dir = confine_under(checkout, dockerfile_path)?;

        let context = format!("context={}", context_dir.to_string_lossy());
        let dockerfile = format!("dockerfile={}", dockerfile_dir.to_string_lossy());
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

    async fn acquire_staged(
        &self,
        runner: &dyn CommandRunner,
        source: &ArtifactSource,
    ) -> Result<String, ArtifactAcquireError> {
        let ArtifactSource::UploadedContext {
            upload_id,
            dockerfile_path,
            context_path,
        } = source
        else {
            unreachable!("staged acquisition requires an uploaded-context source");
        };
        let staged = self.config.uploads_dir.join(upload_id).join("context");
        let context_dir = confine_under(&staged, context_path)?;
        let dockerfile_dir = confine_under(&staged, dockerfile_path)?;
        let context = format!("context={}", context_dir.to_string_lossy());
        let dockerfile = format!("dockerfile={}", dockerfile_dir.to_string_lossy());
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
        let out = runner.run(program.as_ref(), &args).await?;
        Ok(out.stdout.trim().to_string())
    }

    async fn acquire_external_image(
        &self,
        _runner: &dyn CommandRunner,
        source: &ArtifactSource,
        auth: RegistryAuth,
    ) -> Result<String, ArtifactAcquireError> {
        let ArtifactSource::ExternalRegistry { image } = source else {
            unreachable!("external image acquisition requires a registry source");
        };
        let pulled = self.puller.pull(image, auth).await?;
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

/// Resolve `rel` under `root`, refusing anything that could escape the checkout.
/// `rel` is already validated at the API boundary (non-empty, relative, no
/// `..`); this re-checks lexically and, when the target exists (post-clone),
/// canonicalizes to also defeat in-repo symlink escapes.
fn confine_under(
    root: &std::path::Path,
    rel: &str,
) -> Result<std::path::PathBuf, ArtifactAcquireError> {
    use std::path::Component;
    let rel_path = std::path::Path::new(rel);
    let escapes = rel_path.is_absolute()
        || rel_path.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        });
    if escapes {
        return Err(git_path_escape(rel));
    }
    let joined = root.join(rel_path);
    if joined.exists() {
        let canonical_root = std::fs::canonicalize(root)?;
        let canonical = std::fs::canonicalize(&joined)?;
        if !canonical.starts_with(&canonical_root) {
            return Err(git_path_escape(rel));
        }
        return Ok(canonical);
    }
    Ok(joined)
}

fn git_path_escape(rel: &str) -> ArtifactAcquireError {
    ArtifactAcquireError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("git build path escapes checkout: {rel}"),
    ))
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

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use async_trait::async_trait;

    use super::*;
    use crate::{
        command::{CommandOutput, FakeCommandRunner},
        config::AppConfig,
        oci::{LayerBlob, OciError, OciImagePuller, OciRootfsUnpacker, PulledImage, RegistryAuth},
    };

    struct FakePuller;
    #[async_trait]
    impl OciImagePuller for FakePuller {
        async fn pull(&self, _image: &str, _auth: RegistryAuth) -> Result<PulledImage, OciError> {
            unreachable!("FakePuller::pull not expected in staged tests")
        }
        async fn read_layout(&self, _d: &Path) -> Result<PulledImage, OciError> {
            unreachable!("FakePuller::read_layout not expected in staged tests")
        }
    }

    struct FakeUnpacker;
    impl OciRootfsUnpacker for FakeUnpacker {
        fn unpack(&self, _layers: &[LayerBlob], _rootfs_dir: &Path) -> Result<(), OciError> {
            unreachable!("FakeUnpacker::unpack not expected in staged tests")
        }
    }

    #[tokio::test]
    async fn acquire_staged_builds_from_upload_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let uploads_dir = tmp.path().join("uploads");
        let upload_id = "test-upload-id-001";
        let context_subdir = uploads_dir.join(upload_id).join("context");
        std::fs::create_dir_all(&context_subdir).unwrap();
        std::fs::write(context_subdir.join("Dockerfile"), b"FROM scratch\n").unwrap();

        let mut config = AppConfig::for_test("test-token");
        config.uploads_dir = uploads_dir.clone();

        let acquirer = ArtifactAcquirer::with_traits(
            config,
            Arc::new(FakePuller),
            Arc::new(FakeUnpacker),
        );

        let runner = FakeCommandRunner::new(vec![CommandOutput {
            stdout: "sha256:abc123staged".to_string(),
            stderr: String::new(),
            status: 0,
        }]);

        let source = ArtifactSource::UploadedContext {
            upload_id: upload_id.to_string(),
            dockerfile_path: ".".to_string(),
            context_path: ".".to_string(),
        };

        let digest = acquirer.acquire_staged(&runner, &source).await.unwrap();

        assert!(!digest.is_empty(), "digest must be non-empty");

        let commands = runner.commands();
        assert_eq!(commands.len(), 1);
        let cmd = &commands[0];
        assert!(
            cmd.contains(&format!("uploads/{upload_id}/context")),
            "buildctl invocation must reference uploads/<id>/context, got: {cmd}"
        );
    }
}
