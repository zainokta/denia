//! `denia update`: self-update the binary from a signed GitHub release.
//!
//! Trust chain (fail-closed): a pinned minisign public key verifies the
//! `SHA256SUMS.minisig` signature over the `SHA256SUMS` manifest; the manifest
//! binds itself to the release tag and pins the SHA256 of the architecture-
//! specific binary; the downloaded binary's hash must match before it ever
//! replaces the running one. See
//! ADR-029.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use serde::Deserialize;

use super::common::{platform, privilege, systemd};

/// Pinned production minisign public key (base64 — the second line of the `.pub` file).
/// The matching secret key signs each release's `SHA256SUMS` in CI and never
/// leaves CI.
const MINISIGN_PUBKEY: &str = "RWTjef0vJl3g2lcJz4JSOlDB64pmYBRYNHxmShlHtCbbjcm4aMIj+vkP";

const REPO: &str = "zainokta/denia";
const TARGET_BIN: &str = "/usr/local/bin/denia";
const API_BASE: &str = "https://api.github.com";
const SERVICE: &str = "denia.service";
const RESTART_TIMEOUT: Duration = Duration::from_secs(30);
const HTTP_TIMEOUT: Duration = Duration::from_secs(60);
/// Reject absurdly large downloads before buffering them into memory.
const MAX_DOWNLOAD: u64 = 256 * 1024 * 1024;

#[derive(clap::Args, Debug)]
pub struct UpdateArgs {
    /// Only report whether a newer release exists; download nothing.
    #[arg(long)]
    pub check: bool,
    /// Reinstall even if the release is not newer than the running version.
    #[arg(long)]
    pub force: bool,
    /// Skip the confirmation prompt.
    #[arg(short = 'y', long)]
    pub yes: bool,
    /// Update to a specific release tag (e.g. v0.2.0) instead of the latest.
    #[arg(long, value_name = "TAG")]
    pub tag: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    assets: Vec<Asset>,
}

#[derive(Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

pub fn run(args: UpdateArgs) -> anyhow::Result<()> {
    platform::ensure_supported_glibc()?;

    // `--check` is read-only; everything else writes /usr/local/bin and calls
    // systemctl, so it must run as root (like `rotate-token`).
    if !args.check {
        privilege::require_root()?;
    }

    let current = current_version()?;

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let client = HttpClient::new(API_BASE.to_string())?;
    let release = rt.block_on(client.fetch_release(args.tag.as_deref()))?;
    let remote = parse_tag(&release.tag_name)?;
    let newer = remote > current;

    if args.check {
        if newer {
            println!("update available: {current} -> {remote}");
        } else {
            println!("up to date: {current} (latest {remote})");
        }
        return Ok(());
    }

    if !newer && !args.force {
        println!("already up to date: {current} (latest {remote})");
        return Ok(());
    }

    let bin_name = asset_name(target_arch()?);
    let bin_url = asset_url(&release, &bin_name)?;
    let sums_url = asset_url(&release, "SHA256SUMS")?;
    let sig_url = asset_url(&release, "SHA256SUMS.minisig")?;

    if !args.yes && !confirm(&current.to_string(), &remote.to_string())? {
        println!("aborted.");
        return Ok(());
    }

    let (bin_bytes, sums_bytes, sig_bytes) = rt.block_on(async {
        let bin = client.download(&bin_url).await?;
        let sums = client.download(&sums_url).await?;
        let sig = client.download(&sig_url).await?;
        anyhow::Ok((bin, sums, sig))
    })?;

    verify(
        MINISIGN_PUBKEY,
        &bin_bytes,
        &sums_bytes,
        &sig_bytes,
        &bin_name,
        &release.tag_name,
    )?;
    println!(
        "==> verified {bin_name} ({} bytes) against signed SHA256SUMS",
        bin_bytes.len()
    );

    let target = Path::new(TARGET_BIN);
    let dir = target
        .parent()
        .unwrap_or_else(|| Path::new("/usr/local/bin"));
    let backup = install_binary(dir, target, &bin_bytes).context("installing new binary")?;

    println!("==> restarting {SERVICE}");
    if let Err(e) = restart_and_wait() {
        if let Some(b) = &backup {
            eprintln!("restart failed; rolling back to the previous binary");
            let _ = std::fs::copy(b, target);
            let _ = restart_and_wait();
        }
        return Err(e.context("update failed and was rolled back to the previous binary"));
    }

    println!();
    println!("denia {current} -> {remote}");
    println!("service: active. previous binary kept at {TARGET_BIN}.bak");
    Ok(())
}

fn current_version() -> anyhow::Result<semver::Version> {
    semver::Version::parse(env!("CARGO_PKG_VERSION")).context("parsing CARGO_PKG_VERSION")
}

fn parse_tag(tag: &str) -> anyhow::Result<semver::Version> {
    let v = tag.strip_prefix('v').unwrap_or(tag);
    semver::Version::parse(v).with_context(|| format!("parsing release tag {tag:?}"))
}

fn target_arch() -> anyhow::Result<&'static str> {
    match std::env::consts::ARCH {
        "x86_64" => Ok("x86_64"),
        "aarch64" => Ok("aarch64"),
        other => bail!("no prebuilt denia release for arch {other:?}; build from source"),
    }
}

fn asset_name(arch: &str) -> String {
    format!("denia-{arch}-linux-gnu")
}

fn asset_url(release: &Release, name: &str) -> anyhow::Result<String> {
    release
        .assets
        .iter()
        .find(|a| a.name == name)
        .map(|a| a.browser_download_url.clone())
        .ok_or_else(|| anyhow!("release {} has no asset named {name}", release.tag_name))
}

/// Full verification: signature over the manifest, then the binary's hash
/// against the manifest. Any failure aborts before the binary is installed.
fn verify(
    pubkey_b64: &str,
    bin: &[u8],
    sums: &[u8],
    sig: &[u8],
    bin_name: &str,
    expected_tag: &str,
) -> anyhow::Result<()> {
    verify_signature(pubkey_b64, sums, sig)?;
    verify_checksum(sums, bin, bin_name, expected_tag)
}

/// Verify a minisign detached signature over `sums` with the pinned key.
/// `allow_legacy = false` requires the prehashed (`ED`) algorithm.
fn verify_signature(pubkey_b64: &str, sums: &[u8], sig: &[u8]) -> anyhow::Result<()> {
    let pk = minisign_verify::PublicKey::from_base64(pubkey_b64)
        .map_err(|e| anyhow!("pinned minisign public key is invalid: {e}"))?;
    let sig_str = std::str::from_utf8(sig).context("SHA256SUMS.minisig is not utf-8")?;
    let signature = minisign_verify::Signature::decode(sig_str)
        .map_err(|e| anyhow!("decoding SHA256SUMS.minisig: {e}"))?;
    pk.verify(sums, &signature, false)
        .map_err(|e| anyhow!("SHA256SUMS signature verification failed: {e}"))
}

/// Compare the binary's SHA256 against the entry for `bin_name` in a manifest
/// whose authenticity has already been established by `verify_signature`.
fn verify_checksum(
    sums: &[u8],
    bin: &[u8],
    bin_name: &str,
    expected_tag: &str,
) -> anyhow::Result<()> {
    let sums_str = std::str::from_utf8(sums).context("SHA256SUMS is not utf-8")?;
    let actual_tag = release_tag_from_manifest(sums_str)?;
    if actual_tag != expected_tag {
        bail!("release tag mismatch in SHA256SUMS: expected {expected_tag}, got {actual_tag}");
    }
    let expected = sha256_for(sums_str, bin_name)
        .ok_or_else(|| anyhow!("SHA256SUMS has no entry for {bin_name}"))?;
    let actual = sha256_hex(bin);
    if actual != expected {
        bail!("checksum mismatch for {bin_name}: refusing to install");
    }
    Ok(())
}

fn release_tag_from_manifest(manifest: &str) -> anyhow::Result<&str> {
    const PREFIX: &str = "# denia-release:";

    let mut found = None;
    for line in manifest.lines() {
        let line = line.trim();
        let Some(raw) = line.strip_prefix(PREFIX) else {
            continue;
        };
        let tag = raw.trim();
        if tag.is_empty() || tag.split_whitespace().count() != 1 {
            bail!("SHA256SUMS has malformed denia-release header");
        }
        parse_tag(tag).context("SHA256SUMS has invalid denia-release tag")?;
        if found.replace(tag).is_some() {
            bail!("SHA256SUMS has multiple denia-release headers");
        }
    }

    found.ok_or_else(|| anyhow!("SHA256SUMS is missing denia-release header"))
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

/// Find the lowercase hex digest for `name` in a `sha256sum`-format manifest.
/// Handles both text (`<hex>  name`) and binary (`<hex> *name`) lines.
fn sha256_for(manifest: &str, name: &str) -> Option<String> {
    for line in manifest.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut it = line.split_whitespace();
        let hex = it.next()?;
        let file = it.next()?.trim_start_matches('*');
        if file == name {
            return Some(hex.to_ascii_lowercase());
        }
    }
    None
}

fn confirm(current: &str, remote: &str) -> anyhow::Result<bool> {
    use std::io::{self, BufRead};
    print!("Update denia {current} -> {remote} and restart the service? [y/N] ");
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "Yes"))
}

/// Stage the new binary next to `target` (same filesystem → atomic rename),
/// back up the current binary to `<target>.bak`, then replace `target`.
/// Returns the backup path, if a prior binary existed.
fn install_binary(dir: &Path, target: &Path, bin: &[u8]) -> anyhow::Result<Option<PathBuf>> {
    let mut tmp = tempfile::Builder::new()
        .prefix(".denia.new.")
        .tempfile_in(dir)
        .with_context(|| format!("staging new binary in {}", dir.display()))?;
    tmp.as_file_mut().write_all(bin)?;
    tmp.as_file_mut().sync_all()?;
    std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o755))?;

    let backup = if target.exists() {
        let b = target.with_extension("bak");
        std::fs::copy(target, &b)
            .with_context(|| format!("backing up {} to {}", target.display(), b.display()))?;
        Some(b)
    } else {
        None
    };

    let path = tmp.into_temp_path();
    path.persist(target)
        .map_err(|e| anyhow!("installing new binary at {}: {}", target.display(), e.error))?;
    Ok(backup)
}

fn restart_and_wait() -> anyhow::Result<()> {
    systemd::restart(SERVICE)?;
    systemd::wait_active(SERVICE, RESTART_TIMEOUT)
}

struct HttpClient {
    client: reqwest::Client,
    api_base: String,
}

impl HttpClient {
    fn new(api_base: String) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("denia-update/", env!("CARGO_PKG_VERSION")))
            .timeout(HTTP_TIMEOUT)
            .build()?;
        Ok(Self { client, api_base })
    }

    async fn fetch_release(&self, tag: Option<&str>) -> anyhow::Result<Release> {
        let url = match tag {
            Some(t) => format!("{}/repos/{REPO}/releases/tags/{t}", self.api_base),
            None => format!("{}/repos/{REPO}/releases/latest", self.api_base),
        };
        let resp = self
            .client
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            bail!("GitHub API {url} returned {status}");
        }
        let bytes = resp.bytes().await?;
        serde_json::from_slice(&bytes).context("parsing GitHub release JSON")
    }

    async fn download(&self, url: &str) -> anyhow::Result<Vec<u8>> {
        let mut resp = self
            .client
            .get(url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            bail!("download {url} returned {status}");
        }
        if let Some(len) = resp.content_length()
            && len > MAX_DOWNLOAD
        {
            bail!("download {url} is too large ({len} bytes)");
        }
        let mut body = Vec::with_capacity(
            resp.content_length()
                .and_then(|len| usize::try_from(len).ok())
                .unwrap_or_default(),
        );
        let mut downloaded = 0_u64;
        while let Some(chunk) = resp.chunk().await? {
            push_download_chunk(&mut body, &mut downloaded, &chunk, url)?;
        }
        Ok(body)
    }
}

fn push_download_chunk(
    body: &mut Vec<u8>,
    downloaded: &mut u64,
    chunk: &[u8],
    url: &str,
) -> anyhow::Result<()> {
    let chunk_len = u64::try_from(chunk.len()).context("download chunk length overflow")?;
    let next = downloaded
        .checked_add(chunk_len)
        .ok_or_else(|| anyhow!("download {url} is too large"))?;
    if next > MAX_DOWNLOAD {
        bail!("download {url} is too large ({next} bytes)");
    }
    body.extend_from_slice(chunk);
    *downloaded = next;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tag_strips_v_prefix() {
        assert_eq!(parse_tag("v0.2.0").unwrap(), parse_tag("0.2.0").unwrap());
        assert!(parse_tag("v0.2.0").unwrap() > parse_tag("0.1.0").unwrap());
        assert!(parse_tag("v0.1.0").unwrap() == parse_tag("0.1.0").unwrap());
        // pre-release sorts below its release
        assert!(parse_tag("0.2.0-rc.1").unwrap() < parse_tag("0.2.0").unwrap());
        assert!(parse_tag("not-a-version").is_err());
    }

    #[test]
    fn asset_name_per_arch() {
        assert_eq!(asset_name("x86_64"), "denia-x86_64-linux-gnu");
        assert_eq!(asset_name("aarch64"), "denia-aarch64-linux-gnu");
    }

    #[test]
    fn asset_url_found_and_missing() {
        let release = Release {
            tag_name: "v0.2.0".into(),
            assets: vec![Asset {
                name: "denia-x86_64-linux-gnu".into(),
                browser_download_url: "https://example/dl".into(),
            }],
        };
        assert_eq!(
            asset_url(&release, "denia-x86_64-linux-gnu").unwrap(),
            "https://example/dl"
        );
        assert!(asset_url(&release, "denia-aarch64-linux-gnu").is_err());
    }

    #[test]
    fn sha256_for_text_and_binary_lines() {
        let manifest = "\
# denia-release: v0.2.0
abc123  denia-x86_64-linux-gnu
DEF456 *denia-aarch64-linux-gnu
# a comment
";
        assert_eq!(
            sha256_for(manifest, "denia-x86_64-linux-gnu").as_deref(),
            Some("abc123")
        );
        // binary-mode marker stripped, hex lowercased
        assert_eq!(
            sha256_for(manifest, "denia-aarch64-linux-gnu").as_deref(),
            Some("def456")
        );
        assert!(sha256_for(manifest, "missing").is_none());
    }

    #[test]
    fn release_tag_from_manifest_accepts_one_signed_header() {
        let manifest = "\
# denia-release: v0.2.0
abc123  denia-x86_64-linux-gnu
";
        assert_eq!(release_tag_from_manifest(manifest).unwrap(), "v0.2.0");
    }

    #[test]
    fn release_tag_from_manifest_rejects_missing_malformed_or_conflicting_headers() {
        assert!(release_tag_from_manifest("abc123  denia-x86_64-linux-gnu\n").is_err());
        assert!(release_tag_from_manifest("# denia-release:\nabc123  denia\n").is_err());
        assert!(
            release_tag_from_manifest("# denia-release: v0.2.0 beta\nabc123  denia\n").is_err()
        );

        let conflicting = "\
# denia-release: v0.2.0
# denia-release: v0.3.0
abc123  denia-x86_64-linux-gnu
";
        assert!(release_tag_from_manifest(conflicting).is_err());
    }

    #[test]
    fn verify_checksum_requires_matching_release_tag_and_digest() {
        let bin = b"the new denia binary bytes";
        let digest = sha256_hex(bin);
        let manifest = format!("# denia-release: v0.2.0\n{digest}  denia-x86_64-linux-gnu\n");
        verify_checksum(manifest.as_bytes(), bin, "denia-x86_64-linux-gnu", "v0.2.0").unwrap();

        // replayed manifest from a different release tag -> error before install
        let err = verify_checksum(manifest.as_bytes(), bin, "denia-x86_64-linux-gnu", "v0.3.0")
            .unwrap_err();
        assert!(err.to_string().contains("release tag mismatch"));

        // legacy or stripped manifest without a signed tag binding -> fail closed
        let legacy_manifest = format!("{digest}  denia-x86_64-linux-gnu\n");
        assert!(
            verify_checksum(
                legacy_manifest.as_bytes(),
                bin,
                "denia-x86_64-linux-gnu",
                "v0.2.0",
            )
            .is_err()
        );

        // tampered binary -> mismatch
        let err = verify_checksum(
            manifest.as_bytes(),
            b"tampered",
            "denia-x86_64-linux-gnu",
            "v0.2.0",
        )
        .unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"));

        // missing entry -> error
        assert!(verify_checksum(manifest.as_bytes(), bin, "other", "v0.2.0").is_err());
    }

    // Known-good prehashed minisign vector (message = b"test"), lifted from the
    // minisign-verify crate's own test suite. In production the signed message
    // is the SHA256SUMS bytes; here it proves we call verify() correctly
    // (prehashed, allow_legacy = false) and fail closed on tampering.
    const VEC_PUBKEY: &str = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
    const VEC_SIG: &str = "untrusted comment: signature from minisign secret key
RUQf6LRCGA9i559r3g7V1qNyJDApGip8MfqcadIgT9CuhV3EMhHoN1mGTkUidF/z7SrlQgXdy8ofjb7bNJJylDOocrCo8KLzZwo=
trusted comment: timestamp:1556193335\tfile:test
y/rUw2y8/hOUYjZU71eHp/Wo1KZ40fGy2VJEDl34XMJM+TX48Ss/17u3IvIfbVR1FkZZSNCisQbuQY+bHwhEBg==";

    #[test]
    fn verify_signature_accepts_vector_and_rejects_tamper() {
        verify_signature(VEC_PUBKEY, b"test", VEC_SIG.as_bytes()).unwrap();
        // tampered message
        assert!(verify_signature(VEC_PUBKEY, b"Test", VEC_SIG.as_bytes()).is_err());
        // malformed signature
        assert!(verify_signature(VEC_PUBKEY, b"test", b"not a minisig").is_err());
        // the Denia release key must not verify signatures from another key.
        assert!(verify_signature(MINISIGN_PUBKEY, b"test", VEC_SIG.as_bytes()).is_err());
    }

    #[test]
    fn push_download_chunk_enforces_cap_without_content_length() {
        let mut body = Vec::new();
        let mut downloaded = MAX_DOWNLOAD - 2;

        push_download_chunk(&mut body, &mut downloaded, b"ok", "https://example/asset").unwrap();
        assert_eq!(downloaded, MAX_DOWNLOAD);
        assert_eq!(body, b"ok");

        let err = push_download_chunk(&mut body, &mut downloaded, b"x", "https://example/asset")
            .unwrap_err();
        assert!(err.to_string().contains("too large"));
        assert_eq!(downloaded, MAX_DOWNLOAD);
        assert_eq!(body, b"ok");
    }

    #[test]
    fn install_binary_swaps_and_backs_up() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("denia");
        std::fs::write(&target, b"OLD").unwrap();

        let backup = install_binary(dir.path(), &target, b"NEW").unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"NEW");
        let backup = backup.expect("backup created when prior binary exists");
        assert_eq!(std::fs::read(&backup).unwrap(), b"OLD");
        let mode = std::fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
    }

    #[test]
    fn install_binary_no_backup_when_target_absent() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("denia");
        let backup = install_binary(dir.path(), &target, b"NEW").unwrap();
        assert!(backup.is_none());
        assert_eq!(std::fs::read(&target).unwrap(), b"NEW");
    }

    #[tokio::test]
    async fn fetch_release_latest_parses_tag_and_assets() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        let _m = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/repos/zainokta/denia/releases/latest");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(
                        r#"{"tag_name":"v0.2.0","assets":[
                        {"name":"denia-x86_64-linux-gnu","browser_download_url":"https://dl/x86"},
                        {"name":"SHA256SUMS","browser_download_url":"https://dl/sums"}
                    ]}"#,
                    );
            })
            .await;
        let client = HttpClient::new(server.base_url()).unwrap();
        let rel = client.fetch_release(None).await.unwrap();
        assert_eq!(rel.tag_name, "v0.2.0");
        assert_eq!(
            asset_url(&rel, "denia-x86_64-linux-gnu").unwrap(),
            "https://dl/x86"
        );
    }

    #[tokio::test]
    async fn fetch_release_uses_tag_endpoint_and_download_returns_bytes() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        let _rel = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/repos/zainokta/denia/releases/tags/v0.3.0");
                then.status(200)
                    .body(r#"{"tag_name":"v0.3.0","assets":[]}"#);
            })
            .await;
        let _dl = server
            .mock_async(|when, then| {
                when.method(GET).path("/blob");
                then.status(200).body("BINBYTES");
            })
            .await;
        let client = HttpClient::new(server.base_url()).unwrap();
        let rel = client.fetch_release(Some("v0.3.0")).await.unwrap();
        assert_eq!(rel.tag_name, "v0.3.0");
        let bytes = client
            .download(&format!("{}/blob", server.base_url()))
            .await
            .unwrap();
        assert_eq!(bytes, b"BINBYTES");
    }

    #[tokio::test]
    async fn fetch_release_surfaces_http_error() {
        use httpmock::prelude::*;
        let server = MockServer::start_async().await;
        let _m = server
            .mock_async(|when, then| {
                when.method(GET)
                    .path("/repos/zainokta/denia/releases/latest");
                then.status(404);
            })
            .await;
        let client = HttpClient::new(server.base_url()).unwrap();
        assert!(client.fetch_release(None).await.is_err());
    }
}
