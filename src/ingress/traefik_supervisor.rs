use crate::config::AppConfig;
use crate::oci::{OciImagePuller, OciRootfsUnpacker, RegistryAuth, pull_image_to_dir};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub struct TraefikStaticOptions {
    pub http_port: u16,
    pub https_port: u16,
    pub dynamic_dir: PathBuf,
    pub acme_email: String,
    pub acme_storage: PathBuf,
    pub resolver: String,
}

pub fn render_static_config(opts: &TraefikStaticOptions) -> String {
    format!(
        "entryPoints:\n\
\x20 web:\n\
\x20   address: \":{http}\"\n\
\x20 websecure:\n\
\x20   address: \":{https}\"\n\
providers:\n\
\x20 file:\n\
\x20   directory: \"{dir}\"\n\
\x20   watch: true\n\
certificatesResolvers:\n\
\x20 {resolver}:\n\
\x20   acme:\n\
\x20     email: \"{email}\"\n\
\x20     storage: \"{storage}\"\n\
\x20     httpChallenge:\n\
\x20       entryPoint: web\n",
        http = opts.http_port,
        https = opts.https_port,
        dir = opts.dynamic_dir.display(),
        resolver = opts.resolver,
        email = opts.acme_email,
        storage = opts.acme_storage.display(),
    )
}

pub fn backoff_delay(attempt: u32) -> Duration {
    let secs = 1u64.checked_shl(attempt).unwrap_or(u64::MAX).min(30);
    Duration::from_secs(secs)
}

/// A spawn/exit failure that must NOT be retried (port already bound).
pub fn is_fatal_bind_error(err: &std::io::Error) -> bool {
    err.kind() == std::io::ErrorKind::AddrInUse
}

#[derive(Debug, PartialEq, Eq)]
pub enum ChildExit {
    Exited,
    FatalBind,
}

#[async_trait::async_trait]
pub trait TraefikSpawner: Send + Sync {
    async fn run_once(&self) -> ChildExit;
}

#[derive(Debug, PartialEq, Eq)]
pub enum SupervisorOutcome {
    Shutdown,
    FatalBind,
    MaxRestarts,
}

pub struct TraefikSupervisor<S: TraefikSpawner> {
    pub spawner: S,
    pub max_restarts_for_test: Option<u32>,
}

impl<S: TraefikSpawner> TraefikSupervisor<S> {
    pub async fn run(self, mut shutdown: mpsc::Receiver<()>) -> SupervisorOutcome {
        let mut attempt: u32 = 0;
        let mut restarts: u32 = 0;
        loop {
            tokio::select! {
                _ = shutdown.recv() => return SupervisorOutcome::Shutdown,
                exit = self.spawner.run_once() => {
                    match exit {
                        ChildExit::FatalBind => return SupervisorOutcome::FatalBind,
                        ChildExit::Exited => {
                            restarts += 1;
                            if let Some(max) = self.max_restarts_for_test
                                && restarts >= max
                            {
                                return SupervisorOutcome::MaxRestarts;
                            }
                            let delay = backoff_delay(attempt);
                            attempt = attempt.saturating_add(1);
                            tokio::select! {
                                _ = shutdown.recv() => return SupervisorOutcome::Shutdown,
                                _ = tokio::time::sleep(delay) => {}
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Pull/cache the image, write static config, ensure dynamic dir + acme.json.
/// Returns the path to the traefik binary inside the unpacked rootfs.
pub async fn acquire_and_prepare(
    config: &AppConfig,
    puller: &dyn OciImagePuller,
    unpacker: &dyn OciRootfsUnpacker,
) -> std::io::Result<PathBuf> {
    let dir = &config.traefik_dir;
    pull_image_to_dir(
        puller,
        unpacker,
        &config.traefik_image,
        RegistryAuth::Anonymous,
        dir,
        "usr/local/bin/traefik",
    )
    .await
    .map_err(|e| std::io::Error::other(e.to_string()))?;

    let dynamic_dir = dir.join("dynamic");
    std::fs::create_dir_all(&dynamic_dir)?;
    let denia_yml = dynamic_dir.join("denia.yml");
    if !denia_yml.exists() {
        std::fs::write(&denia_yml, "http:\n  routers: {}\n  services: {}\n")?;
    }

    let acme = dir.join("acme.json");
    if !acme.exists() {
        std::fs::write(&acme, b"")?;
        std::fs::set_permissions(&acme, std::fs::Permissions::from_mode(0o600))?;
    }

    let static_opts = TraefikStaticOptions {
        http_port: config.http_port,
        https_port: config.https_port,
        dynamic_dir,
        acme_email: config.acme_email.clone().unwrap_or_default(),
        acme_storage: acme,
        resolver: config.ingress_resolver_name(),
    };
    std::fs::write(dir.join("traefik.yml"), render_static_config(&static_opts))?;

    Ok(dir.join("rootfs/usr/local/bin/traefik"))
}

pub struct HostTraefikSpawner {
    pub binary: PathBuf,
    pub config_file: PathBuf,
    pub cwd: PathBuf,
    pub log_path: PathBuf,
}

#[async_trait::async_trait]
impl TraefikSpawner for HostTraefikSpawner {
    async fn run_once(&self) -> ChildExit {
        use std::process::Stdio;
        let log = match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
        {
            Ok(f) => f,
            Err(_) => return ChildExit::Exited,
        };
        let log_err = match log.try_clone() {
            Ok(f) => f,
            Err(_) => return ChildExit::Exited,
        };
        let mut cmd = tokio::process::Command::new(&self.binary);
        cmd.arg(format!("--configfile={}", self.config_file.display()))
            .current_dir(&self.cwd)
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err));
        match cmd.spawn() {
            Ok(mut child) => {
                let _ = child.wait().await;
                ChildExit::Exited
            }
            Err(e) if is_fatal_bind_error(&e) => ChildExit::FatalBind,
            Err(_) => ChildExit::Exited,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> TraefikStaticOptions {
        TraefikStaticOptions {
            http_port: 80,
            https_port: 443,
            dynamic_dir: PathBuf::from("/var/lib/denia/traefik/dynamic"),
            acme_email: "ops@example.com".into(),
            acme_storage: PathBuf::from("/var/lib/denia/traefik/acme.json"),
            resolver: "le".into(),
        }
    }

    #[test]
    fn renders_entrypoints_and_file_provider() {
        let y = render_static_config(&opts());
        assert!(y.contains("address: \":80\""));
        assert!(y.contains("address: \":443\""));
        assert!(y.contains("directory: \"/var/lib/denia/traefik/dynamic\""));
        assert!(y.contains("watch: true"));
    }

    #[test]
    fn backoff_is_capped_and_monotonic() {
        assert_eq!(super::backoff_delay(0), std::time::Duration::from_secs(1));
        assert!(super::backoff_delay(1) >= std::time::Duration::from_secs(2));
        assert_eq!(super::backoff_delay(20), std::time::Duration::from_secs(30));
    }

    #[test]
    fn addr_in_use_is_fatal() {
        let e = std::io::Error::from(std::io::ErrorKind::AddrInUse);
        assert!(super::is_fatal_bind_error(&e));
        let other = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert!(!super::is_fatal_bind_error(&other));
    }

    struct ExitOnceThenFatal {
        calls: std::sync::Mutex<u32>,
    }
    #[async_trait::async_trait]
    impl super::TraefikSpawner for ExitOnceThenFatal {
        async fn run_once(&self) -> super::ChildExit {
            let mut c = self.calls.lock().unwrap();
            *c += 1;
            if *c == 1 {
                super::ChildExit::Exited
            } else {
                super::ChildExit::FatalBind
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn restarts_then_stops_on_fatal_bind() {
        let (_tx, rx) = tokio::sync::mpsc::channel(1);
        let sup = super::TraefikSupervisor {
            spawner: ExitOnceThenFatal {
                calls: std::sync::Mutex::new(0),
            },
            max_restarts_for_test: Some(5),
        };
        let outcome = sup.run(rx).await;
        assert_eq!(outcome, super::SupervisorOutcome::FatalBind);
    }

    struct AlwaysExit;
    #[async_trait::async_trait]
    impl super::TraefikSpawner for AlwaysExit {
        async fn run_once(&self) -> super::ChildExit {
            super::ChildExit::Exited
        }
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_stops_loop() {
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        let sup = super::TraefikSupervisor {
            spawner: AlwaysExit,
            max_restarts_for_test: None,
        };
        let handle = tokio::spawn(sup.run(rx));
        tx.send(()).await.unwrap();
        let outcome = handle.await.unwrap();
        assert_eq!(outcome, super::SupervisorOutcome::Shutdown);
    }

    #[test]
    fn renders_acme_resolver() {
        let y = render_static_config(&opts());
        assert!(y.contains("le:"));
        assert!(y.contains("email: \"ops@example.com\""));
        assert!(y.contains("storage: \"/var/lib/denia/traefik/acme.json\""));
        assert!(y.contains("entryPoint: web"));
    }
}
