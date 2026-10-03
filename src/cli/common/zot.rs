use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, anyhow, bail};
use sha2::{Digest, Sha256};

pub const ZOT_VERSION: &str = "2.1.20";
pub const ZOT_BIN: &str = "/usr/local/bin/zot";
pub const ZOT_CONFIG_DIR: &str = "/etc/denia";
pub const ZOT_CONFIG_PATH: &str = "/etc/denia/zot.json";
pub const ZOT_UNIT_PATH: &str = "/etc/systemd/system/zot.service";
pub const ZOT_MARKER_PATH: &str = "/etc/denia/zot.managed";
pub const DENIA_ZOT_DROPIN_DIR: &str = "/etc/systemd/system/denia.service.d";
pub const DENIA_ZOT_DROPIN_PATH: &str = "/etc/systemd/system/denia.service.d/20-zot.conf";
pub const ZOT_STORAGE_DIR: &str = "/var/lib/denia/zot";
pub const ZOT_LISTEN_ADDR: &str = "127.0.0.1";
pub const ZOT_LISTEN_PORT: &str = "5000";

const ZOT_AMD64_SHA256: &str = "a32e42d042d1f17b5b1317e55cc1a415a744c873dcd05c25c56b665478258bcb";
const ZOT_ARM64_SHA256: &str = "d6a39475587be18ec3d42e0d2bfa50f5c5064cbbcdc222dbd88e55ecf69dd8e9";
const ACTIVE_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
struct ManagedPaths {
    pub binary: PathBuf,
    pub config: PathBuf,
    pub unit: PathBuf,
    pub marker: PathBuf,
    pub storage: PathBuf,
    pub denia_dropin: PathBuf,
}

impl Default for ManagedPaths {
    fn default() -> Self {
        Self {
            binary: ZOT_BIN.into(),
            config: ZOT_CONFIG_PATH.into(),
            unit: ZOT_UNIT_PATH.into(),
            marker: ZOT_MARKER_PATH.into(),
            storage: ZOT_STORAGE_DIR.into(),
            denia_dropin: DENIA_ZOT_DROPIN_PATH.into(),
        }
    }
}

#[derive(Clone, Debug)]
struct PinnedRelease {
    pub version: &'static str,
    pub url: String,
    pub sha256: &'static str,
}

fn release_for_arch(arch: &str) -> anyhow::Result<PinnedRelease> {
    let (asset_arch, sha256) = match arch {
        "x86_64" | "amd64" => ("amd64", ZOT_AMD64_SHA256),
        "aarch64" | "arm64" => ("arm64", ZOT_ARM64_SHA256),
        other => bail!("unsupported architecture for zot: {other}"),
    };
    Ok(PinnedRelease {
        version: ZOT_VERSION,
        url: format!(
            "https://github.com/project-zot/zot/releases/download/v{ZOT_VERSION}/zot-linux-{asset_arch}"
        ),
        sha256,
    })
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

/// Reconcile the pinned, Denia-managed Zot install. Existing unmanaged paths
/// are adopted only when their bytes already match the expected contents.
pub fn reconcile_managed_install() -> anyhow::Result<()> {
    let release = release_for_arch(std::env::consts::ARCH)?;
    reconcile_with(&ManagedPaths::default(), &release, &RealHost)
}

pub fn check_managed_install() -> anyhow::Result<()> {
    let paths = ManagedPaths::default();
    let release = release_for_arch(std::env::consts::ARCH)?;
    let marker = read_marker(&paths.marker)?
        .ok_or_else(|| anyhow!("Denia Zot ownership marker is missing; run `sudo denia setup`"))?;
    let expected_config = render_config();
    let expected_unit = super::systemd::render_zot_unit();
    validate_existing(
        &paths,
        Some(&marker),
        release.sha256,
        &expected_config,
        &expected_unit,
    )?;
    if file_hash(&paths.binary)?.as_deref() != Some(release.sha256) {
        bail!(
            "Zot binary is not the pinned Denia version {}; run `sudo denia setup`",
            release.version
        );
    }
    if read_optional(&paths.config)?.as_deref() != Some(expected_config.as_bytes()) {
        bail!("Denia Zot config is missing or stale; run `sudo denia setup`");
    }
    if read_optional(&paths.unit)?.as_deref() != Some(expected_unit.as_bytes()) {
        bail!("Denia Zot systemd unit is missing or stale; run `sudo denia setup`");
    }
    if read_optional(&paths.denia_dropin)?.as_deref()
        != Some(super::systemd::render_denia_zot_dropin().as_bytes())
    {
        bail!("Denia Zot startup dependency is missing or stale; run `sudo denia setup`");
    }
    RealHost.check_service_ownership(&paths.unit)?;
    if !super::systemd::is_active("zot.service") {
        bail!("zot.service is not active; run `sudo systemctl start zot.service`");
    }
    Ok(())
}

pub fn remove_managed_service(purge: bool) -> anyhow::Result<()> {
    let release = release_for_arch(std::env::consts::ARCH)?;
    remove_managed_with_release(&ManagedPaths::default(), purge, &RealHost, &release)
}

fn remove_managed_with_release(
    paths: &ManagedPaths,
    purge: bool,
    host: &dyn HostOps,
    release: &PinnedRelease,
) -> anyhow::Result<()> {
    let marker = read_marker(&paths.marker)?;
    let config = render_config();
    let unit = super::systemd::render_zot_unit();
    let owned = if marker.is_some() {
        validate_existing(paths, marker.as_ref(), release.sha256, &config, &unit)?;
        true
    } else {
        file_hash(&paths.binary)?.as_deref() == Some(release.sha256)
            && read_optional(&paths.config)?.as_deref() == Some(config.as_bytes())
            && read_optional(&paths.unit)?.as_deref() == Some(unit.as_bytes())
    };
    let dropin = read_optional(&paths.denia_dropin)?;
    if dropin
        .as_deref()
        .is_some_and(|body| body != super::systemd::render_denia_zot_dropin().as_bytes())
    {
        bail!("refusing to remove modified Denia Zot dependency drop-in");
    }
    if owned {
        host.check_service_ownership(&paths.unit)?;
        if marker.is_none() && !purge {
            let marker = format!(
                "version={}\nbinary={}\nconfig={}\nunit={}\n",
                release.version,
                release.sha256,
                sha256_hex(config.as_bytes()),
                sha256_hex(unit.as_bytes())
            );
            let staged = temp_sibling(&paths.marker)?;
            fs::write(&staged, marker)?;
            replace_from(&staged, &paths.marker, 0o644)?;
        }
        if host.is_active() {
            host.stop()?;
        }
        if host.is_enabled() {
            host.disable()?;
        }
        remove_optional(&paths.unit)?;
        if purge {
            for path in [&paths.binary, &paths.config, &paths.marker] {
                remove_optional(path)?;
            }
        }
    }
    remove_optional(&paths.denia_dropin)?;
    Ok(())
}

fn remove_optional(path: &Path) -> anyhow::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("remove {}", path.display())),
    }
}

/// The host boundary keeps the real reconciliation flow testable with local
/// files and observable command results.
trait HostOps {
    fn download(&self, url: &str, destination: &Path) -> anyhow::Result<()>;
    fn verify_config(&self, binary: &Path, config: &Path) -> anyhow::Result<()>;
    fn daemon_reload(&self) -> anyhow::Result<()>;
    fn enable_now(&self) -> anyhow::Result<()>;
    fn restart(&self) -> anyhow::Result<()>;
    fn stop(&self) -> anyhow::Result<()>;
    fn start(&self) -> anyhow::Result<()>;
    fn is_active(&self) -> bool;
    fn is_enabled(&self) -> bool;
    fn disable(&self) -> anyhow::Result<()>;
    fn check_service_ownership(&self, expected_unit: &Path) -> anyhow::Result<()>;
    fn wait_active(&self) -> anyhow::Result<()>;
    fn chown_storage(&self, storage: &Path) -> anyhow::Result<()>;
}

struct RealHost;

impl HostOps for RealHost {
    fn download(&self, url: &str, destination: &Path) -> anyhow::Result<()> {
        let dest = destination
            .to_str()
            .ok_or_else(|| anyhow!("download path is not utf-8"))?;
        let status = Command::new("curl")
            .args(["--proto", "=https", "--tlsv1.2", "-fsSL", url, "-o", dest])
            .stdin(Stdio::null())
            .status()
            .context("failed to execute curl while installing zot")?;
        if !status.success() {
            bail!("curl failed while downloading zot {ZOT_VERSION}: {status}");
        }
        Ok(())
    }

    fn verify_config(&self, binary: &Path, config: &Path) -> anyhow::Result<()> {
        let status = Command::new(binary)
            .arg("verify")
            .arg(config)
            .stdin(Stdio::null())
            .status()
            .with_context(|| format!("failed to execute {} verify", binary.display()))?;
        if !status.success() {
            bail!(
                "{} verify {} exited with {status}",
                binary.display(),
                config.display()
            );
        }
        Ok(())
    }

    fn daemon_reload(&self) -> anyhow::Result<()> {
        command("systemctl", &["daemon-reload"])
    }

    fn enable_now(&self) -> anyhow::Result<()> {
        command("systemctl", &["enable", "--now", "zot.service"])
    }

    fn restart(&self) -> anyhow::Result<()> {
        command("systemctl", &["restart", "zot.service"])
    }

    fn stop(&self) -> anyhow::Result<()> {
        command("systemctl", &["stop", "zot.service"])
    }

    fn start(&self) -> anyhow::Result<()> {
        command("systemctl", &["start", "zot.service"])
    }

    fn is_active(&self) -> bool {
        super::systemd::is_active("zot.service")
    }

    fn is_enabled(&self) -> bool {
        super::systemd::is_enabled("zot.service")
    }

    fn disable(&self) -> anyhow::Result<()> {
        command("systemctl", &["disable", "zot.service"])
    }

    fn check_service_ownership(&self, expected_unit: &Path) -> anyhow::Result<()> {
        let fragment = systemctl_show("FragmentPath")?;
        let expected = expected_unit
            .to_str()
            .ok_or_else(|| anyhow!("unit path is not utf-8"))?;
        if !fragment.is_empty() && fragment != expected {
            bail!(
                "zot.service is supplied by {fragment}; refusing to replace an unrelated systemd unit"
            );
        }
        let dropins = systemctl_show("DropInPaths")?;
        if !dropins.is_empty() {
            bail!(
                "zot.service has systemd drop-ins ({dropins}); refusing to change an unrelated service"
            );
        }
        Ok(())
    }

    fn wait_active(&self) -> anyhow::Result<()> {
        super::systemd::wait_active("zot.service", ACTIVE_TIMEOUT)
    }

    fn chown_storage(&self, storage: &Path) -> anyhow::Result<()> {
        let path = storage
            .to_str()
            .ok_or_else(|| anyhow!("storage path is not utf-8"))?;
        command("chown", &["denia:denia", path])
    }
}

fn reconcile_with(
    paths: &ManagedPaths,
    release: &PinnedRelease,
    host: &dyn HostOps,
) -> anyhow::Result<()> {
    // Reject unsupported architectures before creating any files or invoking
    // a downloader; release metadata is resolved by the caller first.
    let desired_config = render_config();
    let desired_unit = super::systemd::render_zot_unit();
    let desired_denia_dropin = super::systemd::render_denia_zot_dropin();
    let desired_binary_hash = normalize_hash(release.sha256)?;
    let existing_marker = read_marker(&paths.marker)?;

    validate_existing(
        paths,
        existing_marker.as_ref(),
        &desired_binary_hash,
        &desired_config,
        &desired_unit,
    )?;
    let installed_denia_dropin = read_optional(&paths.denia_dropin)?;
    if installed_denia_dropin
        .as_deref()
        .is_some_and(|bytes| bytes != desired_denia_dropin.as_bytes())
    {
        bail!(
            "{} exists and is not the expected Denia Zot dependency drop-in; refusing to replace it",
            paths.denia_dropin.display()
        );
    }
    host.check_service_ownership(&paths.unit)?;

    let mut stages = StagedFiles::new(paths)?;
    let binary_is_expected = file_hash(&paths.binary)?.as_deref() == Some(&desired_binary_hash);
    let binary_has_mode = has_mode(&paths.binary, 0o755)?;
    if binary_is_expected && binary_has_mode {
        stages.binary_source = Some(paths.binary.clone());
    } else {
        if binary_is_expected {
            fs::copy(&paths.binary, &stages.binary)?;
        } else {
            host.download(&release.url, &stages.binary)?;
        }
        let actual = file_hash(&stages.binary)?
            .ok_or_else(|| anyhow!("downloaded Zot binary is missing"))?;
        if actual != desired_binary_hash {
            bail!("zot sha256 mismatch: got {actual}, expected {desired_binary_hash}");
        }
        fs::set_permissions(&stages.binary, fs::Permissions::from_mode(0o755))?;
        stages.binary_source = Some(stages.binary.to_path_buf());
    }
    fs::write(&stages.config, &desired_config)?;
    fs::set_permissions(&stages.config, fs::Permissions::from_mode(0o644))?;
    fs::write(&stages.unit, &desired_unit)?;
    fs::set_permissions(&stages.unit, fs::Permissions::from_mode(0o644))?;
    fs::write(&stages.denia_dropin, &desired_denia_dropin)?;
    fs::set_permissions(&stages.denia_dropin, fs::Permissions::from_mode(0o644))?;
    let binary_hash = file_hash(stages.binary_source.as_ref().expect("staged binary source"))?
        .ok_or_else(|| anyhow!("Zot binary disappeared during staging"))?;
    let config_hash = sha256_hex(desired_config.as_bytes());
    let unit_hash = sha256_hex(desired_unit.as_bytes());
    let marker = format!(
        "version={}\nbinary={binary_hash}\nconfig={config_hash}\nunit={unit_hash}\n",
        release.version
    );
    fs::write(&stages.marker, &marker)?;
    fs::set_permissions(&stages.marker, fs::Permissions::from_mode(0o644))?;

    host.verify_config(stages.binary_source.as_ref().unwrap(), &stages.config)
        .context("verifying staged Zot configuration")?;

    let changed_binary =
        file_hash(&paths.binary)?.as_deref() != Some(&binary_hash) || !binary_has_mode;
    let changed_config = read_optional(&paths.config)?.as_deref()
        != Some(desired_config.as_bytes())
        || !has_mode(&paths.config, 0o644)?;
    let changed_unit = read_optional(&paths.unit)?.as_deref() != Some(desired_unit.as_bytes())
        || !has_mode(&paths.unit, 0o644)?;
    let changed_denia_dropin = installed_denia_dropin.is_none();
    let changed_marker = read_optional(&paths.marker)?.as_deref() != Some(marker.as_bytes());
    let changed = changed_binary || changed_config || changed_unit;
    let changed_systemd = changed_unit || changed_denia_dropin;

    fs::create_dir_all(
        paths
            .binary
            .parent()
            .ok_or_else(|| anyhow!("binary has no parent"))?,
    )?;
    fs::create_dir_all(
        paths
            .config
            .parent()
            .ok_or_else(|| anyhow!("config has no parent"))?,
    )?;
    fs::create_dir_all(
        paths
            .unit
            .parent()
            .ok_or_else(|| anyhow!("unit has no parent"))?,
    )?;
    fs::create_dir_all(
        paths
            .denia_dropin
            .parent()
            .ok_or_else(|| anyhow!("Denia drop-in has no parent"))?,
    )?;
    fs::create_dir_all(&paths.storage)?;
    fs::set_permissions(
        paths
            .config
            .parent()
            .ok_or_else(|| anyhow!("config has no parent"))?,
        fs::Permissions::from_mode(0o755),
    )?;
    fs::set_permissions(
        paths
            .denia_dropin
            .parent()
            .ok_or_else(|| anyhow!("Denia drop-in has no parent"))?,
        fs::Permissions::from_mode(0o755),
    )?;
    fs::set_permissions(&paths.storage, fs::Permissions::from_mode(0o750))?;
    host.chown_storage(&paths.storage)?;

    let previous = Snapshot::capture(paths)?;
    let was_active = host.is_active();
    let was_enabled = host.is_enabled();
    let mutation = (|| -> anyhow::Result<()> {
        if changed_binary {
            replace_from(stages.binary_source.as_ref().unwrap(), &paths.binary, 0o755)?;
        }
        if changed_config {
            replace_from(&stages.config, &paths.config, 0o644)?;
        }
        if changed_unit {
            replace_from(&stages.unit, &paths.unit, 0o644)?;
        }
        if changed_marker {
            replace_from(&stages.marker, &paths.marker, 0o644)?;
        }
        if changed_unit {
            host.daemon_reload()?;
        }
        host.enable_now()?;
        if changed {
            host.restart()?;
        }
        host.wait_active()?;
        if changed_denia_dropin {
            replace_from(&stages.denia_dropin, &paths.denia_dropin, 0o644)?;
            host.daemon_reload()?;
        }
        Ok(())
    })();
    if let Err(error) = mutation {
        let stop = if changed || (!was_active && host.is_active()) {
            host.stop()
        } else {
            Ok(())
        };
        // If this reconciliation enabled Zot for the first time, remove that
        // enablement before restoring a snapshot that may remove its unit.
        let enabled = if was_enabled { Ok(()) } else { host.disable() };
        let prepare_files = stop
            .as_ref()
            .map(|_| ())
            .and(enabled.as_ref().map(|_| ()))
            .map_err(|error| {
                anyhow!("unable to restore Zot service state before file rollback: {error}")
            });
        let rollback = prepare_files.and_then(|()| previous.restore(paths));
        let reload = if rollback.is_ok() && changed_systemd {
            host.daemon_reload()
        } else if rollback.is_err() {
            Err(anyhow!("skipped because restoring prior files failed"))
        } else {
            Ok(())
        };
        let restore_state = if rollback.is_err() || reload.is_err() || stop.is_err() {
            Err(anyhow!(
                "skipped because restoring prior files or systemd state failed"
            ))
        } else {
            if was_active {
                host.start().and_then(|()| host.wait_active())
            } else if host.is_active() {
                host.stop()
            } else {
                Ok(())
            }
        };
        return Err(combine_rollback_error(
            error,
            stop,
            enabled,
            rollback,
            reload,
            restore_state,
        ));
    }
    Ok(())
}

fn validate_existing(
    paths: &ManagedPaths,
    marker: Option<&Marker>,
    expected_binary_hash: &str,
    expected_config: &str,
    expected_unit: &str,
) -> anyhow::Result<()> {
    let binary_hash = file_hash(&paths.binary)?;
    let config = read_optional(&paths.config)?;
    let unit = read_optional(&paths.unit)?;
    match marker {
        Some(marker) => {
            semver::Version::parse(&marker.version)
                .context("invalid version in Denia Zot ownership marker")?;
            if let Some(actual) = binary_hash.as_deref()
                && actual != marker.binary
            {
                bail!(
                    "managed Zot binary changed outside Denia; refusing to overwrite {}",
                    paths.binary.display()
                );
            }
            if let Some(actual) = config.as_deref()
                && sha256_hex(actual) != marker.config
            {
                bail!(
                    "managed Zot config changed outside Denia; refusing to overwrite {}",
                    paths.config.display()
                );
            }
            if let Some(actual) = unit.as_deref()
                && sha256_hex(actual) != marker.unit
            {
                bail!(
                    "managed Zot unit changed outside Denia; refusing to overwrite {}",
                    paths.unit.display()
                );
            }
        }
        None => {
            let any_existing = binary_hash.is_some() || config.is_some() || unit.is_some();
            if any_existing
                && (binary_hash.as_deref() != Some(expected_binary_hash)
                    || config.as_deref() != Some(expected_config.as_bytes())
                    || unit.as_deref() != Some(expected_unit.as_bytes()))
            {
                bail!(
                    "existing Zot files lack a valid Denia ownership marker and do not all match the pinned managed install; refusing to overwrite them"
                );
            }
        }
    }
    Ok(())
}

#[derive(Debug)]
struct Marker {
    version: String,
    binary: String,
    config: String,
    unit: String,
}

fn read_marker(path: &Path) -> anyhow::Result<Option<Marker>> {
    let Some(bytes) = read_optional(path)? else {
        return Ok(None);
    };
    let raw = std::str::from_utf8(&bytes).context("Denia Zot ownership marker is not utf-8")?;
    let mut fields = std::collections::BTreeMap::new();
    for line in raw.lines() {
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| anyhow!("malformed Denia Zot ownership marker"))?;
        if fields.insert(key, value).is_some() {
            bail!("duplicate field in Denia Zot ownership marker");
        }
    }
    let version = fields
        .get("version")
        .ok_or_else(|| anyhow!("missing version in Denia Zot ownership marker"))?
        .to_string();
    let digest = |name: &str| -> anyhow::Result<String> {
        let value = fields
            .get(name)
            .ok_or_else(|| anyhow!("missing {name} in Denia Zot ownership marker"))?;
        normalize_hash(value)
    };
    if fields.len() != 4 {
        bail!("unexpected fields in Denia Zot ownership marker");
    }
    Ok(Some(Marker {
        version,
        binary: digest("binary")?,
        config: digest("config")?,
        unit: digest("unit")?,
    }))
}

struct StagedFiles {
    binary: tempfile::TempPath,
    config: tempfile::TempPath,
    unit: tempfile::TempPath,
    marker: tempfile::TempPath,
    denia_dropin: tempfile::TempPath,
    binary_source: Option<PathBuf>,
}

impl StagedFiles {
    fn new(paths: &ManagedPaths) -> anyhow::Result<Self> {
        let binary = temp_sibling(&paths.binary)?;
        let config = temp_sibling(&paths.config)?;
        let unit = temp_sibling(&paths.unit)?;
        let marker = temp_sibling(&paths.marker)?;
        let denia_dropin = temp_sibling(&paths.denia_dropin)?;
        Ok(Self {
            binary,
            config,
            unit,
            marker,
            denia_dropin,
            binary_source: None,
        })
    }
}

#[derive(Default)]
struct Snapshot(Vec<(PathBuf, Option<Vec<u8>>, Option<u32>)>);

impl Snapshot {
    fn capture(paths: &ManagedPaths) -> anyhow::Result<Self> {
        let mut snapshot = Self::default();
        for path in [
            &paths.binary,
            &paths.config,
            &paths.unit,
            &paths.marker,
            &paths.denia_dropin,
        ] {
            let bytes = read_optional(path)?;
            let mode = if bytes.is_some() {
                Some(fs::metadata(path)?.permissions().mode() & 0o777)
            } else {
                None
            };
            snapshot.0.push((path.clone(), bytes, mode));
        }
        Ok(snapshot)
    }

    fn restore(self, _paths: &ManagedPaths) -> anyhow::Result<()> {
        let mut errors = Vec::new();
        for (path, bytes, mode) in self.0 {
            let result = match bytes {
                Some(bytes) => atomic_write(&path, &bytes, mode.unwrap_or(0o644)),
                None => match fs::remove_file(&path) {
                    Ok(()) => Ok(()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(error) => Err(error.into()),
                },
            };
            if let Err(error) = result {
                errors.push(format!("{}: {error}", path.display()));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            bail!("restoring previous Zot files failed: {}", errors.join("; "))
        }
    }
}

fn combine_rollback_error(
    cause: anyhow::Error,
    stop: anyhow::Result<()>,
    enabled: anyhow::Result<()>,
    files: anyhow::Result<()>,
    reload: anyhow::Result<()>,
    service_state: anyhow::Result<()>,
) -> anyhow::Error {
    match (stop, enabled, files, reload, service_state) {
        (Ok(()), Ok(()), Ok(()), Ok(()), Ok(())) => cause
            .context("Zot reconciliation failed; previous files and service state were restored"),
        (stop, enabled, files, reload, service_state) => anyhow!(
            "Zot reconciliation failed ({cause}); rollback results: stop={}, enable_state={}, files={}, systemd_reload={}, service_state={}",
            result_text(stop),
            result_text(enabled),
            result_text(files),
            result_text(reload),
            result_text(service_state)
        ),
    }
}

fn result_text(result: anyhow::Result<()>) -> String {
    match result {
        Ok(()) => "ok".into(),
        Err(error) => format!("failed ({error})"),
    }
}

fn replace_from(source: &Path, destination: &Path, mode: u32) -> anyhow::Result<()> {
    let bytes =
        fs::read(source).with_context(|| format!("read staged file {}", source.display()))?;
    atomic_write(destination, &bytes, mode)
}

fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent", path.display()))?;
    fs::create_dir_all(parent)?;
    let mut tmp = tempfile::Builder::new()
        .prefix(".denia-zot.")
        .tempfile_in(parent)?;
    std::io::Write::write_all(tmp.as_file_mut(), bytes)?;
    tmp.as_file_mut().sync_all()?;
    fs::set_permissions(tmp.path(), fs::Permissions::from_mode(mode))?;
    let temp = tmp.into_temp_path();
    temp.persist(path)
        .map_err(|error| anyhow!("replace {}: {}", path.display(), error.error))?;
    Ok(())
}

fn temp_sibling(path: &Path) -> anyhow::Result<tempfile::TempPath> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent", path.display()))?;
    fs::create_dir_all(parent)?;
    Ok(tempfile::Builder::new()
        .prefix(".denia-zot-stage.")
        .tempfile_in(parent)?
        .into_temp_path())
}

fn read_optional(path: &Path) -> anyhow::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn file_hash(path: &Path) -> anyhow::Result<Option<String>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("{} is a symlink; refusing to manage it", path.display());
        }
        Ok(_) => Ok(Some(sha256_hex(&fs::read(path)?))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn has_mode(path: &Path, mode: u32) -> anyhow::Result<bool> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.permissions().mode() & 0o777 == mode),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(bytes);
    hex::encode(hash.finalize())
}

fn normalize_hash(hash: &str) -> anyhow::Result<String> {
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("invalid SHA-256 digest in Zot metadata");
    }
    Ok(hash.to_ascii_lowercase())
}

fn command(bin: &str, args: &[&str]) -> anyhow::Result<()> {
    let status = Command::new(bin).args(args).stdin(Stdio::null()).status()?;
    if !status.success() {
        bail!("{bin} {args:?} exited with {status}");
    }
    Ok(())
}

fn systemctl_show(property: &str) -> anyhow::Result<String> {
    let output = Command::new("systemctl")
        .args(["show", "-p", property, "--value", "zot.service"])
        .stdin(Stdio::null())
        .output()
        .context("failed to inspect zot.service ownership")?;
    if !output.status.success() {
        bail!(
            "systemctl show {property} zot.service exited with {}",
            output.status
        );
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct FakeHost {
        bytes: Vec<u8>,
        fail: Option<&'static str>,
        events: std::sync::Mutex<Vec<String>>,
        active: std::cell::Cell<bool>,
        enabled: std::cell::Cell<bool>,
    }

    impl FakeHost {
        fn new(bytes: &[u8]) -> Self {
            Self {
                bytes: bytes.to_vec(),
                fail: None,
                events: Default::default(),
                active: Default::default(),
                enabled: Default::default(),
            }
        }

        fn events(&self) -> Vec<String> {
            self.events.lock().unwrap().clone()
        }

        fn event(&self, value: &str) -> anyhow::Result<()> {
            self.events.lock().unwrap().push(value.into());
            if self.fail == Some(value) {
                bail!("fake {value} failure");
            }
            Ok(())
        }
    }

    impl HostOps for FakeHost {
        fn download(&self, _url: &str, destination: &Path) -> anyhow::Result<()> {
            self.events.lock().unwrap().push("download".into());
            fs::write(destination, &self.bytes)?;
            Ok(())
        }
        fn verify_config(&self, binary: &Path, config: &Path) -> anyhow::Result<()> {
            self.events.lock().unwrap().push("verify".into());
            assert!(binary.exists());
            assert!(config.exists());
            Ok(())
        }
        fn daemon_reload(&self) -> anyhow::Result<()> {
            self.event("daemon-reload")
        }
        fn enable_now(&self) -> anyhow::Result<()> {
            self.event("enable-now")?;
            self.enabled.set(true);
            self.active.set(true);
            Ok(())
        }
        fn restart(&self) -> anyhow::Result<()> {
            self.active.set(false);
            self.event("restart")?;
            self.active.set(true);
            Ok(())
        }
        fn stop(&self) -> anyhow::Result<()> {
            self.event("stop")?;
            self.active.set(false);
            Ok(())
        }
        fn start(&self) -> anyhow::Result<()> {
            self.event("start")?;
            self.active.set(true);
            Ok(())
        }
        fn is_active(&self) -> bool {
            self.active.get()
        }
        fn is_enabled(&self) -> bool {
            self.enabled.get()
        }
        fn disable(&self) -> anyhow::Result<()> {
            self.event("disable")?;
            self.enabled.set(false);
            Ok(())
        }
        fn check_service_ownership(&self, _expected_unit: &Path) -> anyhow::Result<()> {
            Ok(())
        }
        fn wait_active(&self) -> anyhow::Result<()> {
            self.event("wait-active")
        }
        fn chown_storage(&self, _storage: &Path) -> anyhow::Result<()> {
            self.event("chown")
        }
    }

    fn paths(root: &Path) -> ManagedPaths {
        ManagedPaths {
            binary: root.join("usr/local/bin/zot"),
            config: root.join("etc/denia/zot.json"),
            unit: root.join("etc/systemd/system/zot.service"),
            marker: root.join("etc/denia/zot.managed"),
            storage: root.join("var/lib/denia/zot"),
            denia_dropin: root.join("etc/systemd/system/denia.service.d/20-zot.conf"),
        }
    }

    fn release(bytes: &[u8]) -> PinnedRelease {
        PinnedRelease {
            version: "0.0.1",
            url: "https://example.invalid/zot".into(),
            sha256: Box::leak(sha256_hex(bytes).into_boxed_str()),
        }
    }

    #[test]
    fn pinned_release_metadata_matches_supported_architectures() {
        assert_eq!(
            release_for_arch("x86_64").unwrap().url,
            "https://github.com/project-zot/zot/releases/download/v2.1.20/zot-linux-amd64"
        );
        assert_eq!(release_for_arch("x86_64").unwrap().sha256, ZOT_AMD64_SHA256);
        assert_eq!(
            release_for_arch("aarch64").unwrap().url,
            "https://github.com/project-zot/zot/releases/download/v2.1.20/zot-linux-arm64"
        );
        assert_eq!(
            release_for_arch("aarch64").unwrap().sha256,
            ZOT_ARM64_SHA256
        );
    }

    #[test]
    fn unsupported_architecture_is_rejected() {
        assert!(release_for_arch("riscv64").is_err());
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

    #[test]
    fn staged_binary_can_be_executed_for_config_verification() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let binary = temp_sibling(&temp.path().join("zot")).unwrap();
        let config = temp.path().join("zot.json");
        fs::write(&binary, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(&config, render_config()).unwrap();
        RealHost.verify_config(&binary, &config).unwrap();
    }

    #[test]
    fn missing_zot_is_installed_then_enabled_and_started_before_return() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        let host = FakeHost::new(b"pinned zot bytes");
        reconcile_with(&paths, &release(&host.bytes), &host).unwrap();
        assert_eq!(fs::read(&paths.binary).unwrap(), host.bytes);
        assert!(paths.marker.exists());
        assert_eq!(
            fs::metadata(paths.config.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        assert_eq!(
            fs::metadata(&paths.config).unwrap().permissions().mode() & 0o777,
            0o644
        );
        assert_eq!(
            fs::metadata(&paths.storage).unwrap().permissions().mode() & 0o777,
            0o750
        );
        let events = host.events();
        let verify = events.iter().position(|x| x == "verify").unwrap();
        let enable = events.iter().position(|x| x == "enable-now").unwrap();
        let restart = events.iter().position(|x| x == "restart").unwrap();
        let wait = events.iter().rposition(|x| x == "wait-active").unwrap();
        assert!(verify < enable && enable < restart && restart < wait);
    }

    #[test]
    fn exact_managed_install_is_idempotent_without_redownload_or_restart() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        let bytes = b"pinned zot bytes";
        let host = FakeHost::new(bytes);
        let release = release(bytes);
        reconcile_with(&paths, &release, &host).unwrap();
        host.events.lock().unwrap().clear();
        reconcile_with(&paths, &release, &host).unwrap();
        let events = host.events();
        assert!(!events.iter().any(|x| x == "download"));
        assert!(!events.iter().any(|x| x == "restart"));
        assert!(events.iter().any(|x| x == "enable-now"));
    }

    #[test]
    fn matching_owned_binary_with_wrong_mode_is_repaired_without_download() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        let host = FakeHost::new(b"pinned Zot");
        reconcile_with(&paths, &release(&host.bytes), &host).unwrap();
        fs::set_permissions(&paths.binary, fs::Permissions::from_mode(0o644)).unwrap();
        host.events.lock().unwrap().clear();
        reconcile_with(&paths, &release(&host.bytes), &host).unwrap();
        assert_eq!(
            fs::metadata(&paths.binary).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert!(!host.events().iter().any(|event| event == "download"));
    }

    #[test]
    fn sha256_mismatch_leaves_existing_files_untouched() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        let host = FakeHost::new(b"tampered");
        let expected = release(b"expected");
        assert!(
            reconcile_with(&paths, &expected, &host)
                .unwrap_err()
                .to_string()
                .contains("sha256 mismatch")
        );
        assert!(!paths.binary.exists());
        assert!(!paths.config.exists());
        assert!(!host.events().iter().any(|x| x == "enable-now"));
    }

    #[test]
    fn unmanaged_binary_or_unit_is_never_overwritten() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        fs::create_dir_all(paths.binary.parent().unwrap()).unwrap();
        fs::write(&paths.binary, b"administrator Zot").unwrap();
        let bytes = b"pinned Zot";
        let host = FakeHost::new(bytes);
        assert!(
            reconcile_with(&paths, &release(bytes), &host)
                .unwrap_err()
                .to_string()
                .contains("lack a valid Denia ownership marker")
        );
        assert_eq!(fs::read(&paths.binary).unwrap(), b"administrator Zot");
        assert!(!host.events().iter().any(|x| x == "download"));
    }

    #[test]
    fn exact_binary_without_denias_config_and_unit_is_not_adopted() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        let bytes = b"pinned Zot bytes";
        fs::create_dir_all(paths.binary.parent().unwrap()).unwrap();
        fs::write(&paths.binary, bytes).unwrap();
        let host = FakeHost::new(bytes);
        let error = reconcile_with(&paths, &release(bytes), &host).unwrap_err();
        assert!(error.to_string().contains("do not all match"));
        assert_eq!(fs::read(&paths.binary).unwrap(), bytes);
        assert!(!paths.marker.exists());
        assert!(!host.events().iter().any(|event| event == "enable-now"));
    }

    #[test]
    fn exact_legacy_install_is_adopted_and_marked_without_download() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        let bytes = b"pinned Zot bytes";
        fs::create_dir_all(paths.binary.parent().unwrap()).unwrap();
        fs::create_dir_all(paths.config.parent().unwrap()).unwrap();
        fs::create_dir_all(paths.unit.parent().unwrap()).unwrap();
        fs::write(&paths.binary, bytes).unwrap();
        fs::set_permissions(&paths.binary, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(&paths.config, render_config()).unwrap();
        fs::write(&paths.unit, super::super::systemd::render_zot_unit()).unwrap();
        let host = FakeHost::new(bytes);
        reconcile_with(&paths, &release(bytes), &host).unwrap();
        assert!(paths.marker.exists());
        assert!(!host.events().iter().any(|event| event == "download"));
        assert!(!host.events().iter().any(|event| event == "restart"));
    }

    #[test]
    fn legacy_uninstall_preserves_ownership_for_setup_to_reuse() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        let bytes = b"pinned Zot bytes";
        fs::create_dir_all(paths.binary.parent().unwrap()).unwrap();
        fs::create_dir_all(paths.config.parent().unwrap()).unwrap();
        fs::create_dir_all(paths.unit.parent().unwrap()).unwrap();
        fs::write(&paths.binary, bytes).unwrap();
        fs::set_permissions(&paths.binary, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(&paths.config, render_config()).unwrap();
        fs::write(&paths.unit, super::super::systemd::render_zot_unit()).unwrap();
        let host = FakeHost::new(bytes);
        remove_managed_with_release(&paths, false, &host, &release(bytes)).unwrap();
        assert!(paths.marker.exists());
        reconcile_with(&paths, &release(bytes), &host).unwrap();
        assert!(paths.unit.exists());
    }

    #[test]
    fn first_install_restart_failure_removes_partial_managed_files() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        let host = FakeHost {
            bytes: b"pinned Zot bytes".to_vec(),
            fail: Some("restart"),
            events: Default::default(),
            active: Default::default(),
            enabled: Default::default(),
        };
        assert!(reconcile_with(&paths, &release(&host.bytes), &host).is_err());
        assert!(!paths.binary.exists());
        assert!(!paths.config.exists());
        assert!(!paths.unit.exists());
        assert!(!paths.marker.exists());
        assert!(!paths.denia_dropin.exists());
        let events = host.events();
        assert!(
            events.iter().position(|event| event == "stop").unwrap()
                < events.iter().position(|event| event == "disable").unwrap()
        );
    }

    #[test]
    fn system_failure_restores_prior_binary_config_unit_and_marker() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        let first = FakeHost::new(b"old Zot");
        reconcile_with(&paths, &release(&first.bytes), &first).unwrap();
        let prior_binary = fs::read(&paths.binary).unwrap();
        let prior_config = fs::read(&paths.config).unwrap();
        let prior_unit = fs::read(&paths.unit).unwrap();
        let prior_marker = fs::read(&paths.marker).unwrap();
        let changed = FakeHost {
            bytes: b"new Zot".to_vec(),
            fail: Some("restart"),
            events: Default::default(),
            active: Default::default(),
            enabled: Default::default(),
        };
        assert!(reconcile_with(&paths, &release(&changed.bytes), &changed).is_err());
        assert_eq!(fs::read(&paths.binary).unwrap(), prior_binary);
        assert_eq!(fs::read(&paths.config).unwrap(), prior_config);
        assert_eq!(fs::read(&paths.unit).unwrap(), prior_unit);
        assert_eq!(fs::read(&paths.marker).unwrap(), prior_marker);
    }

    #[test]
    fn later_reconcile_upgrades_from_recorded_managed_binary_hash() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        let first = FakeHost::new(b"old Zot");
        reconcile_with(&paths, &release(&first.bytes), &first).unwrap();
        let second = FakeHost::new(b"new Zot");
        reconcile_with(&paths, &release(&second.bytes), &second).unwrap();
        assert_eq!(fs::read(&paths.binary).unwrap(), b"new Zot");
        assert!(second.events().contains(&"download".into()));
        assert!(second.events().contains(&"restart".into()));
    }

    #[test]
    fn failed_upgrade_restores_running_and_enabled_states_independently() {
        for (active, enabled) in [(true, true), (true, false), (false, true), (false, false)] {
            let temp = tempfile::tempdir().unwrap();
            let paths = paths(temp.path());
            let first = FakeHost::new(b"old Zot");
            reconcile_with(&paths, &release(&first.bytes), &first).unwrap();
            let mut host = FakeHost::new(b"new Zot");
            host.active.set(active);
            host.enabled.set(enabled);
            host.fail = Some("restart");
            assert!(reconcile_with(&paths, &release(&host.bytes), &host).is_err());
            assert_eq!(fs::read(&paths.binary).unwrap(), b"old Zot");
            assert_eq!((host.active.get(), host.enabled.get()), (active, enabled));
        }
    }

    #[test]
    fn dropin_reload_failure_restores_prior_dependency_file() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        let mut host = FakeHost::new(b"Zot");
        reconcile_with(&paths, &release(&host.bytes), &host).unwrap();
        fs::remove_file(&paths.denia_dropin).unwrap();
        host.fail = Some("daemon-reload");
        assert!(reconcile_with(&paths, &release(&host.bytes), &host).is_err());
        assert!(!paths.denia_dropin.exists());
        assert_eq!(fs::read(&paths.binary).unwrap(), b"Zot");
    }

    #[test]
    fn uninstall_leaves_unrelated_zot_files_and_service_untouched() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        fs::create_dir_all(paths.binary.parent().unwrap()).unwrap();
        fs::write(&paths.binary, b"unrelated Zot").unwrap();
        let host = FakeHost::new(b"");
        host.active.set(true);
        remove_managed_with_release(&paths, true, &host, &release(&host.bytes)).unwrap();
        assert_eq!(fs::read(&paths.binary).unwrap(), b"unrelated Zot");
        assert!(host.active.get());
        assert!(host.events().is_empty());
    }

    #[test]
    fn uninstall_stops_owned_service_and_preserves_reinstall_provenance_unless_purged() {
        for purge in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let paths = paths(temp.path());
            let host = FakeHost::new(b"owned Zot");
            reconcile_with(&paths, &release(&host.bytes), &host).unwrap();
            remove_managed_with_release(&paths, purge, &host, &release(&host.bytes)).unwrap();
            assert!(!host.active.get());
            assert!(!host.enabled.get());
            assert!(!paths.unit.exists());
            assert!(!paths.denia_dropin.exists());
            assert_eq!(paths.binary.exists(), !purge);
            assert_eq!(paths.marker.exists(), !purge);
            if !purge {
                reconcile_with(&paths, &release(&host.bytes), &host).unwrap();
                assert!(paths.unit.exists());
            }
        }
    }
}
