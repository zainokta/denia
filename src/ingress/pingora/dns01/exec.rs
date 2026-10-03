//! `exec` DNS-01 provider: shells out to an operator-supplied script to create
//! and remove the challenge TXT record. Mirrors lego's `exec` provider — the
//! universal escape hatch for any DNS host (Akamai, Route53, PowerDNS, BIND, …).
//!
//! Contract: Denia runs `<script> present <fqdn> <value>` to create the record
//! and `<script> cleanup <fqdn> <value>` to remove it. Exit 0 = success; any
//! non-zero exit (or spawn failure) is a [`Dns01Error::Exec`].
//!
//! ## Secrets discipline
//!
//! Denia passes only the (non-secret) FQDN and TXT value as argv. The script owns
//! its own DNS-provider credentials (its environment) — no credential is held by
//! Denia, logged, or placed in `config.toml`.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;

use super::{Dns01Error, Dns01Provider, TxtHandle};

/// Runs an external script for present/cleanup. Holds only the script path.
pub struct ExecDns01 {
    command: PathBuf,
}

impl ExecDns01 {
    pub fn new(command: PathBuf) -> Self {
        Self { command }
    }

    /// Run `<command> <action> <fqdn> <value>`; map a non-zero exit / spawn error
    /// to [`Dns01Error::Exec`]. Hook output is always discarded because it may
    /// contain provider credentials.
    async fn run(&self, action: &str, fqdn: &str, value: &str) -> Result<(), Dns01Error> {
        let mut child = tokio::process::Command::new(&self.command)
            .arg(action)
            .arg(fqdn)
            .arg(value)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                Dns01Error::Exec(format!("spawn {} failed: {e}", self.command.display()))
            })?;
        let status = match tokio::time::timeout(Duration::from_secs(30), child.wait()).await {
            Ok(result) => result.map_err(|e| Dns01Error::Exec(format!("wait failed: {e}")))?,
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(Dns01Error::ExecTimeout(format!("{action} for {fqdn}")));
            }
        };
        if status.success() {
            return Ok(());
        }
        let code = status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".to_string());
        Err(Dns01Error::Exec(format!(
            "{action} for {fqdn} exited {code} (hook output suppressed)"
        )))
    }
}

#[async_trait]
impl Dns01Provider for ExecDns01 {
    async fn present(&self, fqdn: &str, value: &str) -> Result<TxtHandle, Dns01Error> {
        self.run("present", fqdn, value).await?;
        Ok(TxtHandle {
            fqdn: fqdn.to_string(),
            value: value.to_string(),
            provider_ref: None,
        })
    }

    async fn cleanup(&self, handle: &TxtHandle) -> Result<(), Dns01Error> {
        self.run("cleanup", &handle.fqdn, &handle.value).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// Write an executable shell script to a temp dir; return its path.
    fn write_script(dir: &std::path::Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    #[tokio::test]
    async fn present_and_cleanup_invoke_script_with_argv() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("calls.log");
        // Record argv per invocation so we can assert action + fqdn + value.
        let script = write_script(
            dir.path(),
            "hook.sh",
            &format!("#!/bin/sh\necho \"$1 $2 $3\" >> {}\n", out.display()),
        );

        let exec = ExecDns01::new(script);
        let handle = exec
            .present("_acme-challenge.zainokta.com", "tokval")
            .await
            .expect("present");
        assert!(handle.provider_ref.is_none());
        exec.cleanup(&handle).await.expect("cleanup");

        let logged = std::fs::read_to_string(&out).unwrap();
        assert!(logged.contains("present _acme-challenge.zainokta.com tokval"));
        assert!(logged.contains("cleanup _acme-challenge.zainokta.com tokval"));
    }

    #[tokio::test]
    async fn nonzero_exit_is_exec_error() {
        let dir = tempfile::tempdir().unwrap();
        let script = write_script(
            dir.path(),
            "fail.sh",
            "#!/bin/sh\necho 'provider rejected' >&2\nexit 3\n",
        );
        let exec = ExecDns01::new(script);
        let result = exec.present("_acme-challenge.x.com", "v").await;
        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("should fail"),
        };
        assert!(matches!(err, Dns01Error::Exec(_)));
        assert!(!err.is_retryable());
        let rendered = format!("{err}");
        // Exit code is surfaced; stderr is redacted by default (may hold secrets).
        assert!(rendered.contains("exited 3"));
        assert!(!rendered.contains("provider rejected"));
    }

    #[tokio::test]
    async fn missing_script_is_exec_error() {
        let exec = ExecDns01::new(PathBuf::from("/nonexistent/denia-dns-hook"));
        let result = exec.present("_acme-challenge.x.com", "v").await;
        assert!(matches!(result, Err(Dns01Error::Exec(_))));
    }

    #[tokio::test(start_paused = true)]
    async fn timed_out_hooks_are_killed_and_reaped() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("started");
        let script = write_script(
            dir.path(),
            "slow.sh",
            &format!("#!/bin/sh\necho $$ > {}\nexec sleep 60\n", marker.display()),
        );
        let exec = ExecDns01::new(script);
        for action in ["present", "cleanup"] {
            let exec = ExecDns01::new(exec.command.clone());
            let handle = TxtHandle {
                fqdn: "_acme-challenge.x.com".into(),
                value: "v".into(),
                provider_ref: None,
            };
            let operation = tokio::spawn(async move {
                if action == "present" {
                    exec.present("_acme-challenge.x.com", "v").await.map(|_| ())
                } else {
                    exec.cleanup(&handle).await
                }
            });
            let pid = tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    if let Ok(contents) = std::fs::read_to_string(&marker)
                        && let Ok(pid) = contents.trim().parse::<u32>()
                    {
                        break pid;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("hook started and wrote its pid");
            tokio::time::advance(Duration::from_secs(31)).await;
            let result = operation.await.unwrap();
            assert!(matches!(&result, Err(Dns01Error::ExecTimeout(_))));
            assert!(result.unwrap_err().is_retryable());
            assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
            std::fs::remove_file(&marker).unwrap();
        }
    }
}
