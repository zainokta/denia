use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

use anyhow::{Context, anyhow, bail};
use sha2::{Digest, Sha256};

pub const ZOT_VERSION: &str = "2.1.20";
pub const ZOT_BIN: &str = "/usr/local/bin/zot";
pub const ZOT_CONFIG_DIR: &str = "/etc/denia";
pub const ZOT_CONFIG_PATH: &str = "/etc/denia/zot.json";
pub const ZOT_STORAGE_DIR: &str = "/var/lib/denia/zot";
pub const ZOT_LISTEN_ADDR: &str = "127.0.0.1";
pub const ZOT_LISTEN_PORT: &str = "5000";

const ZOT_AMD64_SHA256: &str =
    "a32e42d042d1f17b5b1317e55cc1a415a744c873dcd05c25c56b665478258bcb";
const ZOT_ARM64_SHA256: &str =
    "d6a39475587be18ec3d42e0d2bfa50f5c5064cbbcdc222dbd88e55ecf69dd8e9";

pub fn release_url_for_arch(arch: &str) -> anyhow::Result<String> {
    let asset_arch = match arch {
        "x86_64" | "amd64" => "amd64",
        "aarch64" | "arm64" => "arm64",
        other => bail!("unsupported architecture for zot: {other}"),
    };
    Ok(format!(
        "https://github.com/project-zot/zot/releases/download/v{ZOT_VERSION}/zot-linux-{asset_arch}"
    ))
}

pub fn expected_sha256_for_arch(arch: &str) -> anyhow::Result<&'static str> {
    match arch {
        "x86_64" | "amd64" => Ok(ZOT_AMD64_SHA256),
        "aarch64" | "arm64" => Ok(ZOT_ARM64_SHA256),
        other => bail!("unsupported architecture for zot: {other}"),
    }
}

pub fn render_config() -> String {
    format!(
        r#"{{
  "distSpecVersion": "1.0.1",
  "storage": {{
    "rootDirectory": "{ZOT_STORAGE_DIR}",
    "commit": true,
    "dedupe": true,
    "gc": true,
    "gcDelay": "1h",
    "gcInterval": "24h"
  }},
  "http": {{
    "address": "{ZOT_LISTEN_ADDR}",
    "port": "{ZOT_LISTEN_PORT}",
    "compat": ["docker2s2"]
  }},
  "log": {{
    "level": "info"
  }}
}}
"#
    )
}

/// Install or upgrade Zot to the Denia-pinned version.
///
/// Returns `true` when the binary was replaced and `false` when the pinned
/// version was already installed.
pub fn ensure_installed() -> anyhow::Result<bool> {
    if installed_version_matches() {
        return Ok(false);
    }

    let arch = std::env::consts::ARCH;
    let url = release_url_for_arch(arch)?;
    let expected = expected_sha256_for_arch(arch)?;
    let tmp = format!("/usr/local/bin/.zot-{ZOT_VERSION}-{}.download", std::process::id());

    let _ = fs::remove_file(&tmp);
    let status = Command::new("curl")
        .args(["--proto", "=https", "--tlsv1.2", "-fsSL", &url, "-o", &tmp])
        .stdin(Stdio::null())
        .status()
        .context("failed to execute curl while installing zot")?;
    if !status.success() {
        let _ = fs::remove_file(&tmp);
        bail!("curl failed while downloading zot {ZOT_VERSION}: {status}");
    }

    let actual = sha256_file(Path::new(&tmp))?;
    if actual != expected {
        let _ = fs::remove_file(&tmp);
        bail!("zot sha256 mismatch: got {actual}, expected {expected}");
    }

    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755))?;
    fs::rename(&tmp, ZOT_BIN).with_context(|| format!("install {ZOT_BIN}"))?;
    Ok(true)
}

pub fn write_config() -> anyhow::Result<()> {
    fs::create_dir_all(ZOT_CONFIG_DIR)?;
    fs::set_permissions(ZOT_CONFIG_DIR, fs::Permissions::from_mode(0o755))?;

    fs::create_dir_all(ZOT_STORAGE_DIR)?;
    fs::set_permissions(ZOT_STORAGE_DIR, fs::Permissions::from_mode(0o750))?;
    run("chown", &["denia:denia", ZOT_STORAGE_DIR])?;

    let tmp = format!("{ZOT_CONFIG_PATH}.tmp");
    fs::write(&tmp, render_config())?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o644))?;
    fs::rename(&tmp, ZOT_CONFIG_PATH)?;
    Ok(())
}

pub fn verify_config() -> anyhow::Result<()> {
    let status = Command::new(ZOT_BIN)
        .args(["verify", ZOT_CONFIG_PATH])
        .stdin(Stdio::null())
        .status()
        .context("failed to execute zot verify")?;
    if !status.success() {
        return Err(anyhow!("zot verify {ZOT_CONFIG_PATH} exited with {status}"));
    }
    Ok(())
}

fn installed_version_matches() -> bool {
    let output = match Command::new(ZOT_BIN)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => return false,
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    stdout.contains(ZOT_VERSION) || stderr.contains(ZOT_VERSION)
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0_u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn run(bin: &str, args: &[&str]) -> anyhow::Result<()> {
    let status = Command::new(bin).args(args).stdin(Stdio::null()).status()?;
    if !status.success() {
        return Err(anyhow!("{bin} {args:?} exited with {status}"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_release_metadata_matches_supported_architectures() {
        assert_eq!(
            release_url_for_arch("x86_64").unwrap(),
            "https://github.com/project-zot/zot/releases/download/v2.1.20/zot-linux-amd64"
        );
        assert_eq!(
            expected_sha256_for_arch("x86_64").unwrap(),
            "a32e42d042d1f17b5b1317e55cc1a415a744c873dcd05c25c56b665478258bcb"
        );
        assert_eq!(
            release_url_for_arch("aarch64").unwrap(),
            "https://github.com/project-zot/zot/releases/download/v2.1.20/zot-linux-arm64"
        );
        assert_eq!(
            expected_sha256_for_arch("aarch64").unwrap(),
            "d6a39475587be18ec3d42e0d2bfa50f5c5064cbbcdc222dbd88e55ecf69dd8e9"
        );
    }

    #[test]
    fn unsupported_architecture_is_rejected() {
        assert!(release_url_for_arch("riscv64").is_err());
        assert!(expected_sha256_for_arch("riscv64").is_err());
    }

    #[test]
    fn config_is_loopback_only_and_uses_denia_storage() {
        let config = render_config();
        assert!(config.contains("\"address\": \"127.0.0.1\""));
        assert!(config.contains("\"port\": \"5000\""));
        assert!(config.contains("\"rootDirectory\": \"/var/lib/denia/zot\""));
        assert!(config.contains("\"gc\": true"));
        assert!(config.contains("\"compat\": [\"docker2s2\"]"));
        assert!(!config.contains("0.0.0.0"));
    }
}
