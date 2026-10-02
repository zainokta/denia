use std::{os::unix::fs::PermissionsExt, sync::Arc};

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
    runtime::ProcessUser,
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
    #[serde(default, rename = "User")]
    pub user: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootfsBundleSpec {
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
    pub workdir: String,
    #[serde(default)]
    pub user: ProcessUser,
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
    #[error("image config user is invalid: {user} ({reason})")]
    InvalidImageUser { user: String, reason: String },
    #[error("oci error: {0}")]
    Oci(#[from] OciError),
    #[error("invalid upload id")]
    InvalidUploadId,
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
                let output_dir = self.unique_build_dir();
                std::fs::create_dir_all(&output_dir)?;
                let digest = self.acquire_git(runner, &source, &output_dir).await;
                let _ = std::fs::remove_dir_all(&output_dir);
                Ok(ArtifactRecord::new(
                    digest?,
                    ArtifactKind::OciImage,
                    source,
                )?)
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
                let output_dir = self.unique_build_dir();
                std::fs::create_dir_all(&output_dir)?;
                let digest = self.acquire_staged(runner, &source, &output_dir).await;
                let _ = std::fs::remove_dir_all(&output_dir);
                Ok(ArtifactRecord::new(
                    digest?,
                    ArtifactKind::OciImage,
                    source,
                )?)
            }
        }
    }

    /// A fresh, unique OCI-layout output directory for a single build.
    fn unique_build_dir(&self) -> std::path::PathBuf {
        self.config
            .artifact_dir
            .join("builds")
            .join(uuid::Uuid::now_v7().to_string())
    }

    pub async fn acquire_rootfs_bundle(
        &self,
        runner: &dyn CommandRunner,
        request: ArtifactAcquireRequest,
        process: RootfsBundleSpec,
    ) -> Result<ArtifactRecord, ArtifactAcquireError> {
        let (record, bundle_dir) = self.build_and_materialize(runner, request).await?;
        std::fs::write(
            bundle_dir.join("process.json"),
            serde_json::to_vec_pretty(&process)?,
        )?;
        Ok(record)
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
                let (record, _bundle_dir) = self.build_and_materialize(runner, request).await?;
                Ok(record)
            }
        }
    }

    /// Build a Git/Upload request into a UNIQUE per-build OCI layout directory,
    /// read that layout back, and materialize the rootfs bundle from it.
    ///
    /// Each build outputs to `<artifact_dir>/builds/<uuid>` rather than the
    /// shared `artifact_dir`, so concurrent builds cannot clobber each other's
    /// `index.json`/`manifests[0]` and cross-contaminate (a build reading the
    /// wrong image). The temporary layout dir is removed on every path.
    async fn build_and_materialize(
        &self,
        runner: &dyn CommandRunner,
        request: ArtifactAcquireRequest,
    ) -> Result<(ArtifactRecord, std::path::PathBuf), ArtifactAcquireError> {
        let output_dir = self.unique_build_dir();
        std::fs::create_dir_all(&output_dir)?;

        let result = self
            .build_and_materialize_in(runner, request, &output_dir)
            .await;
        // Always clean up the per-build layout dir; the rootfs bundle was
        // already written to its own content-addressed directory.
        let _ = std::fs::remove_dir_all(&output_dir);
        result
    }

    async fn build_and_materialize_in(
        &self,
        runner: &dyn CommandRunner,
        request: ArtifactAcquireRequest,
        output_dir: &std::path::Path,
    ) -> Result<(ArtifactRecord, std::path::PathBuf), ArtifactAcquireError> {
        let (digest, source) = match request {
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
                let digest = self.acquire_git(runner, &source, output_dir).await?;
                (digest, source)
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
                let digest = self.acquire_staged(runner, &source, output_dir).await?;
                (digest, source)
            }
            ArtifactAcquireRequest::ExternalImage { .. } => {
                unreachable!("build_and_materialize is only for Git/Upload sources");
            }
        };

        let image_artifact = ArtifactRecord::new(digest, ArtifactKind::OciImage, source)?;
        let pulled = self.puller.read_layout(output_dir).await?;
        let bundle_dir = self.write_bundle(&image_artifact.digest, &pulled.layers)?;
        let process = rootfs_bundle_from_oci_config(
            &pulled.config,
            &bundle_dir.join("rootfs"),
            self.config.userns_size,
        )?;
        std::fs::write(
            bundle_dir.join("process.json"),
            serde_json::to_vec_pretty(&process)?,
        )?;
        let record = ArtifactRecord::new(
            image_artifact.digest,
            ArtifactKind::RootfsBundle,
            image_artifact.source,
        )?;
        Ok((record, bundle_dir))
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
        let process = rootfs_bundle_from_oci_config(
            &pulled.config,
            &bundle_dir.join("rootfs"),
            self.config.userns_size,
        )?;
        std::fs::write(
            bundle_dir.join("process.json"),
            serde_json::to_vec_pretty(&process)?,
        )?;
        ArtifactRecord::new(digest, ArtifactKind::RootfsBundle, source.clone())
            .map_err(ArtifactAcquireError::Artifact)
    }

    fn write_bundle(
        &self,
        digest: &str,
        layers: &[crate::oci::LayerBlob],
    ) -> Result<std::path::PathBuf, ArtifactAcquireError> {
        let bundle_dir = self.config.artifact_dir.join(safe_artifact_name(digest));
        std::fs::create_dir_all(&bundle_dir)?;
        let rootfs = bundle_dir.join("rootfs");
        let sidecar = bundle_dir.join(ROOTFS_OWNERSHIP_SIDECAR);
        let rootfs_exists = match std::fs::symlink_metadata(&rootfs) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(ArtifactAcquireError::Io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("rootfs path is not a directory: {}", rootfs.display()),
                )));
            }
            Ok(_) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(ArtifactAcquireError::Io(error)),
        };
        let sidecar_valid = rootfs_exists
            && rootfs_ownership_sidecar_matches(
                &sidecar,
                self.config.userns_base,
                self.config.userns_size,
            );
        if rootfs_exists && !sidecar_valid {
            reclaim_current_process_owner_if_possible(&rootfs)?;
            std::fs::remove_dir_all(&rootfs)?;
        }
        if !rootfs_exists || !sidecar_valid {
            let staged_rootfs = bundle_dir.join(format!("rootfs.{}.tmp", uuid::Uuid::now_v7()));
            if let Err(error) = self.unpacker.unpack(layers, &staged_rootfs) {
                let _ = std::fs::remove_dir_all(&staged_rootfs);
                return Err(ArtifactAcquireError::Oci(error));
            }
            if let Err(error) =
                std::fs::set_permissions(&staged_rootfs, std::fs::Permissions::from_mode(0o755))
                && error.kind() != std::io::ErrorKind::PermissionDenied
            {
                let _ = std::fs::remove_dir_all(&staged_rootfs);
                return Err(ArtifactAcquireError::Io(error));
            }
            if crate::syscall::caps::has_effective_cap_chown()
                && let Err(error) = syscall::chown::recursive_lchown_shifted(
                    &staged_rootfs,
                    self.config.userns_base,
                    self.config.userns_size,
                )
            {
                let io_err = error.to_string();
                if !io_err.contains("Operation not permitted") {
                    let _ = std::fs::remove_dir_all(&staged_rootfs);
                    return Err(ArtifactAcquireError::Io(std::io::Error::other(io_err)));
                }
            }
            match std::fs::rename(&staged_rootfs, &rootfs) {
                Ok(()) => {}
                Err(error)
                    if error.kind() == std::io::ErrorKind::AlreadyExists && rootfs.exists() =>
                {
                    let _ = std::fs::remove_dir_all(&staged_rootfs);
                }
                Err(error) => {
                    let _ = std::fs::remove_dir_all(&staged_rootfs);
                    return Err(ArtifactAcquireError::Io(error));
                }
            }
            write_rootfs_ownership_sidecar(
                &sidecar,
                self.config.userns_base,
                self.config.userns_size,
            )?;
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
        output_dir: &std::path::Path,
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
                output_dir,
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
        output_dir: &std::path::Path,
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
        let output = format!("type=oci,dest={}", output_dir.to_string_lossy());
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
        output_dir: &std::path::Path,
    ) -> Result<String, ArtifactAcquireError> {
        let ArtifactSource::UploadedContext {
            upload_id,
            dockerfile_path,
            context_path,
        } = source
        else {
            unreachable!("staged acquisition requires an uploaded-context source");
        };
        let upload_uuid =
            uuid::Uuid::parse_str(upload_id).map_err(|_| ArtifactAcquireError::InvalidUploadId)?;
        let staged = self
            .config
            .uploads_dir
            .join(upload_uuid.to_string())
            .join("context");
        let context_dir = confine_under(&staged, context_path)?;
        let dockerfile_file = confine_under(&staged, dockerfile_path)?;
        let dockerfile_dir = dockerfile_file.parent().ok_or_else(|| {
            ArtifactAcquireError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("dockerfile_path has no parent: {dockerfile_path}"),
            ))
        })?;
        let dockerfile_name = dockerfile_file.file_name().ok_or_else(|| {
            ArtifactAcquireError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("dockerfile_path has no file name: {dockerfile_path}"),
            ))
        })?;
        let context = format!("context={}", context_dir.to_string_lossy());
        let dockerfile = format!("dockerfile={}", dockerfile_dir.to_string_lossy());
        let filename = format!("filename={}", dockerfile_name.to_string_lossy());
        let output = format!("type=oci,dest={}", output_dir.to_string_lossy());
        let program = self.config.buildkit_binary.to_string_lossy();
        let args = [
            "build",
            "--frontend",
            "dockerfile.v0",
            "--local",
            context.as_str(),
            "--local",
            dockerfile.as_str(),
            "--opt",
            filename.as_str(),
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

fn reclaim_current_process_owner_if_possible(
    path: &std::path::Path,
) -> Result<(), ArtifactAcquireError> {
    if !crate::syscall::caps::has_effective_cap_chown() {
        return Ok(());
    }
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };
    syscall::chown::recursive_lchown(path, uid, gid)
        .map_err(|error| ArtifactAcquireError::Io(std::io::Error::other(error.to_string())))
}

fn rootfs_bundle_from_oci_config(
    cfg: &crate::oci::config::OciImageConfig,
    rootfs: &std::path::Path,
    userns_size: u32,
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
    let mut process = RootfsBundleSpec {
        argv: oci_spec.argv,
        env: oci_spec.env,
        workdir: oci_spec.workdir,
        user: resolve_process_user(
            cfg.config
                .as_ref()
                .and_then(|process| process.user.as_deref())
                .unwrap_or(""),
            rootfs,
            userns_size,
        )?,
    };
    resolve_relative_argv0_from_path(&mut process, rootfs);
    Ok(process)
}

const ROOTFS_OWNERSHIP_SIDECAR: &str = "rootfs.ownership.json";
const ROOTFS_OWNERSHIP_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RootfsOwnershipSidecar {
    version: u32,
    userns_base: u32,
    userns_size: u32,
}

fn rootfs_ownership_sidecar_matches(path: &std::path::Path, base: u32, size: u32) -> bool {
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    let Ok(sidecar) = serde_json::from_slice::<RootfsOwnershipSidecar>(&bytes) else {
        return false;
    };
    sidecar
        == RootfsOwnershipSidecar {
            version: ROOTFS_OWNERSHIP_VERSION,
            userns_base: base,
            userns_size: size,
        }
}

fn write_rootfs_ownership_sidecar(
    path: &std::path::Path,
    base: u32,
    size: u32,
) -> Result<(), ArtifactAcquireError> {
    let sidecar = RootfsOwnershipSidecar {
        version: ROOTFS_OWNERSHIP_VERSION,
        userns_base: base,
        userns_size: size,
    };
    std::fs::write(path, serde_json::to_vec_pretty(&sidecar)?)?;
    Ok(())
}

fn resolve_process_user(
    raw_user: &str,
    rootfs: &std::path::Path,
    userns_size: u32,
) -> Result<ProcessUser, ArtifactAcquireError> {
    let raw_user = raw_user.trim();
    if raw_user.is_empty() {
        return Ok(ProcessUser::default());
    }
    let (user_part, group_part) = raw_user.split_once(':').unwrap_or((raw_user, ""));
    if user_part.is_empty() {
        return Err(invalid_image_user(raw_user, "user component is empty"));
    }

    let passwd = read_passwd(rootfs)?;
    let groups = read_group(rootfs)?;
    let (uid, default_gid) = match user_part.parse::<u32>() {
        Ok(uid) => {
            let gid = passwd
                .iter()
                .find(|entry| entry.uid == uid)
                .map(|entry| entry.gid)
                .unwrap_or(0);
            (uid, gid)
        }
        Err(_) => {
            let Some(entry) = passwd.iter().find(|entry| entry.name == user_part) else {
                return Err(invalid_image_user(raw_user, "unknown user name"));
            };
            (entry.uid, entry.gid)
        }
    };

    let gid = if group_part.is_empty() {
        default_gid
    } else {
        match group_part.parse::<u32>() {
            Ok(gid) => gid,
            Err(_) => {
                let Some(entry) = groups.iter().find(|entry| entry.name == group_part) else {
                    return Err(invalid_image_user(raw_user, "unknown group name"));
                };
                entry.gid
            }
        }
    };
    if uid >= userns_size || gid >= userns_size {
        return Err(invalid_image_user(
            raw_user,
            "uid/gid is outside the configured user namespace size",
        ));
    }
    Ok(ProcessUser { uid, gid })
}

fn invalid_image_user(user: &str, reason: &str) -> ArtifactAcquireError {
    ArtifactAcquireError::InvalidImageUser {
        user: user.to_string(),
        reason: reason.to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PasswdEntry {
    name: String,
    uid: u32,
    gid: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GroupEntry {
    name: String,
    gid: u32,
}

fn read_passwd(rootfs: &std::path::Path) -> Result<Vec<PasswdEntry>, ArtifactAcquireError> {
    let path = rootfs.join("etc/passwd");
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(ArtifactAcquireError::Io(error)),
    };
    Ok(content
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(':').collect();
            if fields.len() < 4 {
                return None;
            }
            Some(PasswdEntry {
                name: fields[0].to_string(),
                uid: fields[2].parse().ok()?,
                gid: fields[3].parse().ok()?,
            })
        })
        .collect())
}

fn read_group(rootfs: &std::path::Path) -> Result<Vec<GroupEntry>, ArtifactAcquireError> {
    let path = rootfs.join("etc/group");
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(ArtifactAcquireError::Io(error)),
    };
    Ok(content
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(':').collect();
            if fields.len() < 3 {
                return None;
            }
            Some(GroupEntry {
                name: fields[0].to_string(),
                gid: fields[2].parse().ok()?,
            })
        })
        .collect())
}

fn resolve_relative_argv0_from_path(process: &mut RootfsBundleSpec, rootfs: &std::path::Path) {
    let Some(argv0) = process.argv.first() else {
        return;
    };
    if argv0.starts_with('/') || argv0.contains('/') {
        return;
    }

    let Some((_, path_value)) = process.env.iter().find(|(key, _)| key == "PATH") else {
        return;
    };

    for entry in path_value.split(':') {
        if entry.is_empty() || !entry.starts_with('/') {
            continue;
        }

        let candidate = rootfs.join(entry.trim_start_matches('/')).join(argv0);
        let Ok(metadata) = std::fs::metadata(&candidate) else {
            continue;
        };
        if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
            process.argv[0] = format!("{entry}/{argv0}");
            return;
        }
    }
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
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

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
    async fn acquire_staged_rejects_path_traversal_upload_id() {
        let tmp = tempfile::TempDir::new().unwrap();
        let uploads_dir = tmp.path().join("uploads");
        std::fs::create_dir_all(&uploads_dir).unwrap();

        let mut config = AppConfig::for_test("test-token");
        config.uploads_dir = uploads_dir.clone();

        let acquirer =
            ArtifactAcquirer::with_traits(config, Arc::new(FakePuller), Arc::new(FakeUnpacker));

        let runner = FakeCommandRunner::new(vec![]);

        let source = ArtifactSource::UploadedContext {
            upload_id: "../../../../tmp/escape".to_string(),
            dockerfile_path: ".".to_string(),
            context_path: ".".to_string(),
        };

        let output_dir = tmp.path().join("build-out");
        let result = acquirer.acquire_staged(&runner, &source, &output_dir).await;
        assert!(
            result.is_err(),
            "acquire_staged must reject non-UUID upload_id (path traversal attempt)"
        );
        assert!(
            runner.commands().is_empty(),
            "buildctl must NOT be invoked for a rejected upload_id"
        );
    }

    #[tokio::test]
    async fn acquire_staged_builds_from_upload_dir() {
        let tmp = tempfile::TempDir::new().unwrap();
        let uploads_dir = tmp.path().join("uploads");
        let upload_id = uuid::Uuid::now_v7().to_string();
        let context_subdir = uploads_dir.join(&upload_id).join("context");
        std::fs::create_dir_all(&context_subdir).unwrap();
        std::fs::write(context_subdir.join("Dockerfile"), b"FROM scratch\n").unwrap();

        let mut config = AppConfig::for_test("test-token");
        config.uploads_dir = uploads_dir.clone();

        let acquirer =
            ArtifactAcquirer::with_traits(config, Arc::new(FakePuller), Arc::new(FakeUnpacker));

        let runner = FakeCommandRunner::new(vec![CommandOutput {
            stdout: "sha256:abc123staged".to_string(),
            stderr: String::new(),
            status: 0,
        }]);

        let source = ArtifactSource::UploadedContext {
            upload_id: upload_id.to_string(),
            dockerfile_path: "Dockerfile".to_string(),
            context_path: ".".to_string(),
        };

        let output_dir = tmp.path().join("build-out");
        let digest = acquirer
            .acquire_staged(&runner, &source, &output_dir)
            .await
            .unwrap();

        assert!(!digest.is_empty(), "digest must be non-empty");

        let commands = runner.commands();
        assert_eq!(commands.len(), 1);
        let cmd = &commands[0];
        assert!(
            cmd.contains(&format!("uploads/{upload_id}/context")),
            "buildctl invocation must reference uploads/<id>/context, got: {cmd}"
        );
        assert!(
            cmd.contains(&format!(
                "--local dockerfile={}",
                context_subdir.to_string_lossy()
            )),
            "buildctl dockerfile local input must be the directory containing Dockerfile, got: {cmd}"
        );
        assert!(
            !cmd.contains(&format!(
                "--local dockerfile={}",
                context_subdir.join("Dockerfile").to_string_lossy()
            )),
            "buildctl dockerfile local input must not be the Dockerfile file itself, got: {cmd}"
        );
        assert!(
            cmd.contains("--opt filename=Dockerfile"),
            "buildctl invocation must name the Dockerfile within the dockerfile local context, got: {cmd}"
        );
    }

    #[tokio::test]
    async fn external_image_resolves_bare_entrypoint_from_rootfs_path() {
        struct StaticPuller;
        #[async_trait]
        impl OciImagePuller for StaticPuller {
            async fn pull(
                &self,
                _image: &str,
                _auth: RegistryAuth,
            ) -> Result<PulledImage, OciError> {
                Ok(PulledImage {
                    digest: "sha256:nodepath".to_string(),
                    config: crate::oci::config::OciImageConfig {
                        config: Some(crate::oci::config::OciImageProcessConfig {
                            entrypoint: Some(vec!["docker-entrypoint.sh".to_string()]),
                            cmd: Some(vec!["node".to_string(), "server.mjs".to_string()]),
                            env_vars: Some(vec![
                                ("PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin"
                                    .to_string()),
                            ]),
                            working_dir: Some("/app".to_string()),
                            user: None,
                        }),
                        rootfs: None,
                    },
                    layers: Vec::new(),
                    _staging: None,
                    _cache_reservations: Vec::new(),
                })
            }

            async fn read_layout(&self, _d: &Path) -> Result<PulledImage, OciError> {
                unreachable!("StaticPuller::read_layout not expected")
            }
        }

        struct NodeRootfsUnpacker;
        impl OciRootfsUnpacker for NodeRootfsUnpacker {
            fn unpack(&self, _layers: &[LayerBlob], rootfs_dir: &Path) -> Result<(), OciError> {
                let bin_dir = rootfs_dir.join("usr/local/bin");
                std::fs::create_dir_all(&bin_dir).map_err(OciError::Io)?;
                let entrypoint = bin_dir.join("docker-entrypoint.sh");
                std::fs::write(&entrypoint, b"#!/bin/sh\nexec \"$@\"\n").map_err(OciError::Io)?;
                std::fs::set_permissions(&entrypoint, std::fs::Permissions::from_mode(0o755))
                    .map_err(OciError::Io)
            }
        }

        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = AppConfig::for_test("test-token");
        config.artifact_dir = tmp.path().to_path_buf();
        config.userns_base = std::fs::metadata(tmp.path()).unwrap().uid();
        let acquirer = ArtifactAcquirer::with_traits(
            config.clone(),
            Arc::new(StaticPuller),
            Arc::new(NodeRootfsUnpacker),
        );
        let runner = FakeCommandRunner::new(vec![]);

        acquirer
            .acquire_rootfs_bundle_from_image_config(
                &runner,
                ArtifactAcquireRequest::ExternalImage {
                    image: "ghcr.io/acme/node:latest".to_string(),
                },
                RegistryAuth::Anonymous,
            )
            .await
            .expect("acquire rootfs bundle");

        let process_json = std::fs::read_to_string(
            config
                .artifact_dir
                .join("sha256-nodepath")
                .join("process.json"),
        )
        .unwrap();
        let process: RootfsBundleSpec = serde_json::from_str(&process_json).unwrap();
        assert_eq!(
            process.argv,
            vec![
                "/usr/local/bin/docker-entrypoint.sh".to_string(),
                "node".to_string(),
                "server.mjs".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn repeated_external_image_acquire_reuses_complete_bundle() {
        struct StaticPuller;
        #[async_trait]
        impl OciImagePuller for StaticPuller {
            async fn pull(
                &self,
                _image: &str,
                _auth: RegistryAuth,
            ) -> Result<PulledImage, OciError> {
                Ok(PulledImage {
                    digest: "sha256:reused".to_string(),
                    config: crate::oci::config::OciImageConfig {
                        config: Some(crate::oci::config::OciImageProcessConfig {
                            entrypoint: Some(vec!["/app".to_string()]),
                            cmd: None,
                            env_vars: None,
                            working_dir: None,
                            user: None,
                        }),
                        rootfs: None,
                    },
                    layers: Vec::new(),
                    _staging: None,
                    _cache_reservations: Vec::new(),
                })
            }

            async fn read_layout(&self, _d: &Path) -> Result<PulledImage, OciError> {
                unreachable!("StaticPuller::read_layout not expected")
            }
        }

        #[derive(Default)]
        struct ExistingRootfsFailsUnpacker {
            calls: Mutex<usize>,
        }

        impl OciRootfsUnpacker for ExistingRootfsFailsUnpacker {
            fn unpack(&self, _layers: &[LayerBlob], rootfs_dir: &Path) -> Result<(), OciError> {
                let mut calls = self.calls.lock().unwrap();
                *calls += 1;
                if rootfs_dir.exists() {
                    return Err(OciError::Io(std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        "File exists",
                    )));
                }
                std::fs::create_dir_all(rootfs_dir).map_err(OciError::Io)?;
                std::fs::write(rootfs_dir.join("app"), b"ok").map_err(OciError::Io)
            }
        }

        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = AppConfig::for_test("test-token");
        config.artifact_dir = tmp.path().to_path_buf();
        config.userns_base = std::fs::metadata(tmp.path()).unwrap().uid();
        let unpacker = Arc::new(ExistingRootfsFailsUnpacker::default());
        let acquirer =
            ArtifactAcquirer::with_traits(config, Arc::new(StaticPuller), unpacker.clone());
        let runner = FakeCommandRunner::new(vec![]);
        let request = || ArtifactAcquireRequest::ExternalImage {
            image: "ghcr.io/acme/web:latest".to_string(),
        };

        acquirer
            .acquire_rootfs_bundle_from_image_config(&runner, request(), RegistryAuth::Anonymous)
            .await
            .expect("first acquire materializes rootfs");

        acquirer
            .acquire_rootfs_bundle_from_image_config(&runner, request(), RegistryAuth::Anonymous)
            .await
            .expect("second acquire reuses complete rootfs");

        assert_eq!(
            *unpacker.calls.lock().unwrap(),
            1,
            "complete bundle should be reused without unpacking into existing rootfs"
        );
    }

    #[tokio::test]
    async fn external_image_acquire_reunpacks_bundle_without_ownership_sidecar() {
        struct StaticPuller;
        #[async_trait]
        impl OciImagePuller for StaticPuller {
            async fn pull(
                &self,
                _image: &str,
                _auth: RegistryAuth,
            ) -> Result<PulledImage, OciError> {
                Ok(PulledImage {
                    digest: "sha256:ownership-sidecar".to_string(),
                    config: crate::oci::config::OciImageConfig {
                        config: Some(crate::oci::config::OciImageProcessConfig {
                            entrypoint: Some(vec!["/app".to_string()]),
                            cmd: None,
                            env_vars: None,
                            working_dir: None,
                            user: Some("101:101".to_string()),
                        }),
                        rootfs: None,
                    },
                    layers: Vec::new(),
                    _staging: None,
                    _cache_reservations: Vec::new(),
                })
            }

            async fn read_layout(&self, _d: &Path) -> Result<PulledImage, OciError> {
                unreachable!("StaticPuller::read_layout not expected")
            }
        }

        #[derive(Default)]
        struct CountingUnpacker {
            calls: Mutex<usize>,
        }

        impl OciRootfsUnpacker for CountingUnpacker {
            fn unpack(&self, _layers: &[LayerBlob], rootfs_dir: &Path) -> Result<(), OciError> {
                *self.calls.lock().unwrap() += 1;
                std::fs::create_dir_all(rootfs_dir).map_err(OciError::Io)?;
                std::fs::write(rootfs_dir.join("app"), b"ok").map_err(OciError::Io)
            }
        }

        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = AppConfig::for_test("test-token");
        config.artifact_dir = tmp.path().to_path_buf();
        config.userns_base = std::fs::metadata(tmp.path()).unwrap().uid();
        let unpacker = Arc::new(CountingUnpacker::default());
        let acquirer =
            ArtifactAcquirer::with_traits(config.clone(), Arc::new(StaticPuller), unpacker.clone());
        let runner = FakeCommandRunner::new(vec![]);
        let request = || ArtifactAcquireRequest::ExternalImage {
            image: "ghcr.io/acme/web:latest".to_string(),
        };

        acquirer
            .acquire_rootfs_bundle_from_image_config(&runner, request(), RegistryAuth::Anonymous)
            .await
            .expect("first acquire materializes rootfs");
        std::fs::remove_file(
            config
                .artifact_dir
                .join("sha256-ownership-sidecar")
                .join("rootfs.ownership.json"),
        )
        .expect("remove sidecar");
        acquirer
            .acquire_rootfs_bundle_from_image_config(&runner, request(), RegistryAuth::Anonymous)
            .await
            .expect("second acquire repairs missing sidecar");

        assert_eq!(
            *unpacker.calls.lock().unwrap(),
            2,
            "missing ownership sidecar means the rootfs must be republished"
        );
    }

    #[test]
    fn rootfs_bundle_from_oci_config_resolves_named_user() {
        let tmp = tempfile::TempDir::new().unwrap();
        let rootfs = tmp.path();
        std::fs::create_dir_all(rootfs.join("etc")).unwrap();
        std::fs::write(
            rootfs.join("etc/passwd"),
            "root:x:0:0:root:/root:/bin/sh\nnginx:x:101:101:nginx:/nonexistent:/bin/false\n",
        )
        .unwrap();
        std::fs::write(rootfs.join("etc/group"), "root:x:0:\nnginx:x:101:\n").unwrap();
        let cfg = crate::oci::config::OciImageConfig {
            config: Some(crate::oci::config::OciImageProcessConfig {
                entrypoint: Some(vec!["/app".to_string()]),
                cmd: None,
                env_vars: None,
                working_dir: None,
                user: Some("nginx".to_string()),
            }),
            rootfs: None,
        };

        let process = rootfs_bundle_from_oci_config(&cfg, rootfs, 65_536).expect("process");

        assert_eq!(process.user.uid, 101);
        assert_eq!(process.user.gid, 101);
    }

    #[test]
    fn rootfs_bundle_from_oci_config_rejects_unknown_user() {
        let tmp = tempfile::TempDir::new().unwrap();
        let rootfs = tmp.path();
        std::fs::create_dir_all(rootfs.join("etc")).unwrap();
        std::fs::write(rootfs.join("etc/passwd"), "root:x:0:0:root:/root:/bin/sh\n").unwrap();
        let cfg = crate::oci::config::OciImageConfig {
            config: Some(crate::oci::config::OciImageProcessConfig {
                entrypoint: Some(vec!["/app".to_string()]),
                cmd: None,
                env_vars: None,
                working_dir: None,
                user: Some("nginx".to_string()),
            }),
            rootfs: None,
        };

        let error = rootfs_bundle_from_oci_config(&cfg, rootfs, 65_536).expect_err("unknown user");

        assert!(matches!(
            error,
            ArtifactAcquireError::InvalidImageUser { .. }
        ));
    }

    #[tokio::test]
    async fn external_image_acquire_publishes_rootfs_with_traverse_mode() {
        struct StaticPuller;
        #[async_trait]
        impl OciImagePuller for StaticPuller {
            async fn pull(
                &self,
                _image: &str,
                _auth: RegistryAuth,
            ) -> Result<PulledImage, OciError> {
                Ok(PulledImage {
                    digest: "sha256:mode".to_string(),
                    config: crate::oci::config::OciImageConfig {
                        config: Some(crate::oci::config::OciImageProcessConfig {
                            entrypoint: Some(vec!["/app".to_string()]),
                            cmd: None,
                            env_vars: None,
                            working_dir: None,
                            user: None,
                        }),
                        rootfs: None,
                    },
                    layers: Vec::new(),
                    _staging: None,
                    _cache_reservations: Vec::new(),
                })
            }

            async fn read_layout(&self, _d: &Path) -> Result<PulledImage, OciError> {
                unreachable!("StaticPuller::read_layout not expected")
            }
        }

        struct PrivateRootfsUnpacker;
        impl OciRootfsUnpacker for PrivateRootfsUnpacker {
            fn unpack(&self, _layers: &[LayerBlob], rootfs_dir: &Path) -> Result<(), OciError> {
                std::fs::create_dir_all(rootfs_dir).map_err(OciError::Io)?;
                std::fs::set_permissions(rootfs_dir, std::fs::Permissions::from_mode(0o700))
                    .map_err(OciError::Io)
            }
        }

        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = AppConfig::for_test("test-token");
        config.artifact_dir = tmp.path().to_path_buf();
        config.userns_base = std::fs::metadata(tmp.path()).unwrap().uid();
        let acquirer = ArtifactAcquirer::with_traits(
            config.clone(),
            Arc::new(StaticPuller),
            Arc::new(PrivateRootfsUnpacker),
        );
        let runner = FakeCommandRunner::new(vec![]);

        acquirer
            .acquire_rootfs_bundle_from_image_config(
                &runner,
                ArtifactAcquireRequest::ExternalImage {
                    image: "ghcr.io/acme/web:latest".to_string(),
                },
                RegistryAuth::Anonymous,
            )
            .await
            .expect("acquire rootfs bundle");

        let mode = std::fs::metadata(config.artifact_dir.join("sha256-mode").join("rootfs"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o755, "published rootfs must be traversable");
    }
}
