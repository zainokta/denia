use std::process::Command;

use anyhow::{Context, anyhow, bail};

pub const MIN_GLIBC_MAJOR: u64 = 2;
pub const MIN_GLIBC_MINOR: u64 = 39;

pub fn ensure_supported_glibc() -> anyhow::Result<()> {
    let version = host_glibc_version()?;
    if !is_supported_glibc_version(version) {
        bail!(
            "Denia release binaries require glibc >= {} so self-updates can run; detected glibc {}. Upgrade the host OS before using `denia update`.",
            minimum_glibc_label(),
            format_glibc_version(version)
        );
    }
    Ok(())
}

pub fn host_glibc_version() -> anyhow::Result<(u64, u64)> {
    let out = Command::new("ldd")
        .arg("--version")
        .output()
        .context("running `ldd --version` to detect glibc")?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    parse_glibc_version(&text).ok_or_else(|| {
        anyhow!(
            "Denia requires a glibc host with glibc >= {}; `ldd --version` did not report glibc",
            minimum_glibc_label()
        )
    })
}

pub fn is_supported_glibc_version((major, minor): (u64, u64)) -> bool {
    major > MIN_GLIBC_MAJOR || (major == MIN_GLIBC_MAJOR && minor >= MIN_GLIBC_MINOR)
}

pub fn minimum_glibc_label() -> String {
    format!("{MIN_GLIBC_MAJOR}.{MIN_GLIBC_MINOR}")
}

pub fn format_glibc_version((major, minor): (u64, u64)) -> String {
    format!("{major}.{minor}")
}

fn parse_glibc_version(text: &str) -> Option<(u64, u64)> {
    text.lines()
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains("glibc") || lower.contains("gnu libc")
        })
        .find_map(parse_version_from_line)
}

fn parse_version_from_line(line: &str) -> Option<(u64, u64)> {
    for token in line.split(|c: char| !(c.is_ascii_digit() || c == '.')) {
        let Some((major, minor)) = token.split_once('.') else {
            continue;
        };
        if major.is_empty() || minor.is_empty() {
            continue;
        }
        let minor = minor
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>();
        if minor.is_empty() {
            continue;
        }
        if let (Ok(major), Ok(minor)) = (major.parse(), minor.parse()) {
            return Some((major, minor));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_glibc_versions_from_common_ldd_output() {
        assert_eq!(
            parse_glibc_version("ldd (Ubuntu GLIBC 2.39-0ubuntu8.4) 2.39\n"),
            Some((2, 39))
        );
        assert_eq!(
            parse_glibc_version("ldd (Debian GLIBC 2.36-9+deb12u10) 2.36\n"),
            Some((2, 36))
        );
        assert_eq!(
            parse_glibc_version("ldd (GNU libc) 2.40\nCopyright ...\n"),
            Some((2, 40))
        );
    }

    #[test]
    fn rejects_non_glibc_and_too_old_glibc() {
        assert_eq!(
            parse_glibc_version("musl libc (x86_64)\nVersion 1.2.5\n"),
            None
        );
        assert!(!is_supported_glibc_version((2, 36)));
        assert!(is_supported_glibc_version((2, 39)));
        assert!(is_supported_glibc_version((2, 40)));
        assert!(is_supported_glibc_version((3, 0)));
    }
}
