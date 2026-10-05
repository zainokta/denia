//! Filesystem side of the operator prune (ADR-041): find reclaimable entries
//! under the Denia data directories and remove or truncate them.
//!
//! Planning and applying are split so `GET /v1/system/prune` can report the
//! exact candidates without touching disk, and `POST` can delete them.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use uuid::Uuid;

/// Directories under `<artifact_dir>` that are not rootfs bundles.
const BUILDS_DIR: &str = "builds";
const GIT_CHECKOUTS_DIR: &str = "git-checkouts";
/// Files whose presence marks a directory as a rootfs bundle; their mtimes
/// (plus the bundle directory's own) decide whether it is inside the grace
/// window. `write_bundle` rewrites `layers.json` on every acquisition.
const BUNDLE_MARKERS: [&str; 4] = [
    "rootfs",
    "process.json",
    "layers.json",
    "rootfs.ownership.json",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Remove,
    /// Truncate to zero bytes. Used for log files a live workload may still
    /// hold open: unlinking them would not free space until it exits.
    Truncate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub path: PathBuf,
    pub bytes: u64,
    pub action: Action,
}

/// Outcome of [`apply`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Applied {
    pub entries: u64,
    pub bytes: u64,
    pub errors: Vec<String>,
}

/// Rootfs bundles under `artifact_dir` whose directory name is not in
/// `keep_names` and that were not touched within `grace` of `now`.
pub fn plan_bundles(
    artifact_dir: &Path,
    keep_names: &BTreeSet<String>,
    grace: Duration,
    now: SystemTime,
) -> io::Result<Vec<Candidate>> {
    let mut out = Vec::new();
    for bundle in bundle_dirs(artifact_dir)? {
        let name = file_name(&bundle);
        if keep_names.contains(&name) || touched_within(&bundle, grace, now) {
            continue;
        }
        out.push(Candidate {
            bytes: disk_usage(&bundle),
            path: bundle,
            action: Action::Remove,
        });
    }
    Ok(out)
}

/// Temporary build, checkout, staged-rootfs, and upload directories left
/// behind by an interrupted run.
///
/// Build and checkout dirs carry no owner marker, so while any deployment is
/// in flight (`in_flight`) they are all kept. Uploads are kept until they are
/// older than `upload_ttl`, matching the background upload sweep.
pub fn plan_leftovers(
    artifact_dir: &Path,
    uploads_dir: &Path,
    in_flight: bool,
    upload_ttl: Duration,
    now: SystemTime,
) -> io::Result<Vec<Candidate>> {
    let mut out = Vec::new();
    if !in_flight {
        for sub in [BUILDS_DIR, GIT_CHECKOUTS_DIR] {
            for entry in child_entries(&artifact_dir.join(sub))? {
                out.push(remove_candidate(entry));
            }
        }
        for bundle in bundle_dirs(artifact_dir)? {
            for entry in child_entries(&bundle)? {
                let name = file_name(&entry);
                if name.ends_with(".tmp") {
                    out.push(remove_candidate(entry));
                }
            }
        }
    }
    for entry in child_entries(uploads_dir)? {
        if !touched_within(&entry, upload_ttl, now) {
            out.push(remove_candidate(entry));
        }
    }
    Ok(out)
}

/// Log files under `log_dir`.
///
/// - `{service_id}.log` of a service in `live_services`: truncated.
/// - `{service_id}.log` of any other service: removed.
/// - `deployments/{deployment_id}.log` not in `active_deployments`: removed.
pub fn plan_logs(
    log_dir: &Path,
    live_services: &BTreeSet<Uuid>,
    active_deployments: &BTreeSet<Uuid>,
) -> io::Result<Vec<Candidate>> {
    let mut out = Vec::new();
    for entry in child_entries(log_dir)? {
        let Some(id) = log_id(&entry) else {
            continue;
        };
        let bytes = disk_usage(&entry);
        if live_services.contains(&id) {
            if bytes > 0 {
                out.push(Candidate {
                    path: entry,
                    bytes,
                    action: Action::Truncate,
                });
            }
        } else {
            out.push(Candidate {
                path: entry,
                bytes,
                action: Action::Remove,
            });
        }
    }
    for entry in child_entries(&log_dir.join("deployments"))? {
        let Some(id) = log_id(&entry) else {
            continue;
        };
        if !active_deployments.contains(&id) {
            out.push(remove_candidate(entry));
        }
    }
    Ok(out)
}

/// Remove or truncate every candidate. Refuses any path not strictly under
/// one of `allowed_roots`, and never follows symlinks. Entries that vanished
/// since planning are skipped silently.
pub fn apply(candidates: &[Candidate], allowed_roots: &[PathBuf]) -> Applied {
    let mut applied = Applied::default();
    for candidate in candidates {
        let path = &candidate.path;
        if !is_strictly_under(path, allowed_roots) {
            applied.errors.push(format!(
                "refusing path outside data dirs: {}",
                path.display()
            ));
            continue;
        }
        let meta = match fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                applied.errors.push(format!("{}: {error}", path.display()));
                continue;
            }
        };
        // Re-measure: the entry may have changed since the plan.
        let bytes = disk_usage(path);
        let result = match candidate.action {
            Action::Truncate if meta.is_file() => fs::OpenOptions::new()
                .write(true)
                .open(path)
                .and_then(|f| f.set_len(0)),
            Action::Truncate => continue,
            Action::Remove if meta.is_dir() => remove_dir(path),
            Action::Remove => fs::remove_file(path),
        };
        match result {
            Ok(()) => {
                applied.entries += 1;
                applied.bytes += bytes;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => applied.errors.push(format!("{}: {error}", path.display())),
        }
    }
    applied
}

/// Allocated bytes (`st_blocks * 512`, what `du` reports) under `path`,
/// without following symlinks. Unreadable entries count as zero.
pub fn disk_usage(path: &Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(meta) = fs::symlink_metadata(&current) else {
            continue;
        };
        total += meta.blocks() * 512;
        if meta.is_dir()
            && let Ok(entries) = fs::read_dir(&current)
        {
            stack.extend(entries.flatten().map(|e| e.path()));
        }
    }
    total
}

/// Rootfs bundles are written with ownership shifted to the workload's
/// user-namespace range (ADR-038). If plain removal is denied, reclaim
/// ownership (when the daemon holds `CAP_CHOWN`) and retry once.
fn remove_dir(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Err(error)
            if error.kind() == io::ErrorKind::PermissionDenied
                && crate::syscall::caps::has_effective_cap_chown() =>
        {
            let uid = rustix::process::getuid().as_raw();
            let gid = rustix::process::getgid().as_raw();
            crate::syscall::chown::recursive_lchown(path, uid, gid)
                .map_err(|e| io::Error::other(e.to_string()))?;
            fs::remove_dir_all(path)
        }
        other => other,
    }
}

fn bundle_dirs(artifact_dir: &Path) -> io::Result<Vec<PathBuf>> {
    Ok(child_entries(artifact_dir)?
        .into_iter()
        .filter(|path| {
            let name = file_name(path);
            name != BUILDS_DIR
                && name != GIT_CHECKOUTS_DIR
                && is_real_dir(path)
                && BUNDLE_MARKERS.iter().any(|m| path.join(m).exists())
        })
        .collect())
}

/// Direct children of `dir`, excluding symlinks. A missing `dir` is empty.
fn child_entries(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    Ok(entries
        .flatten()
        .filter(|e| e.file_type().map(|t| !t.is_symlink()).unwrap_or(false))
        .map(|e| e.path())
        .collect())
}

fn remove_candidate(path: PathBuf) -> Candidate {
    Candidate {
        bytes: disk_usage(&path),
        path,
        action: Action::Remove,
    }
}

/// `true` if `path` or any bundle marker inside it was modified after
/// `now - grace`.
fn touched_within(path: &Path, grace: Duration, now: SystemTime) -> bool {
    let cutoff = now.checked_sub(grace).unwrap_or(SystemTime::UNIX_EPOCH);
    std::iter::once(path.to_path_buf())
        .chain(BUNDLE_MARKERS.iter().map(|m| path.join(m)))
        .filter_map(|p| fs::symlink_metadata(p).and_then(|m| m.modified()).ok())
        .any(|modified| modified > cutoff)
}

/// Parse `{uuid}.log` into its UUID; anything else is not a Denia log.
fn log_id(path: &Path) -> Option<Uuid> {
    if !fs::symlink_metadata(path).ok()?.is_file() {
        return None;
    }
    let name = file_name(path);
    Uuid::parse_str(name.strip_suffix(".log")?).ok()
}

fn is_real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|m| m.is_dir())
        .unwrap_or(false)
}

fn is_strictly_under(path: &Path, roots: &[PathBuf]) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    let parent = parent
        .canonicalize()
        .unwrap_or_else(|_| parent.to_path_buf());
    roots.iter().any(|root| {
        let root = root.canonicalize().unwrap_or_else(|_| root.clone());
        parent.starts_with(&root) && path.file_name().is_some()
    })
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: Duration = Duration::from_secs(3600);

    fn later() -> SystemTime {
        SystemTime::now() + 2 * HOUR
    }

    fn bundle(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        fs::create_dir_all(dir.join("rootfs/bin")).unwrap();
        fs::write(dir.join("rootfs/bin/app"), vec![0u8; 8192]).unwrap();
        fs::write(dir.join("layers.json"), "[]").unwrap();
        dir
    }

    fn paths(candidates: &[Candidate]) -> Vec<PathBuf> {
        let mut out: Vec<_> = candidates.iter().map(|c| c.path.clone()).collect();
        out.sort();
        out
    }

    #[test]
    fn bundles_outside_keep_set_and_grace_are_candidates() {
        let tmp = tempfile::tempdir().unwrap();
        let kept = bundle(tmp.path(), "sha256-aaa");
        let old = bundle(tmp.path(), "sha256-bbb");
        fs::create_dir_all(tmp.path().join("builds/x")).unwrap();
        fs::create_dir_all(tmp.path().join("not-a-bundle")).unwrap();

        let keep = BTreeSet::from(["sha256-aaa".to_string()]);
        let plan = plan_bundles(tmp.path(), &keep, HOUR, later()).unwrap();

        assert_eq!(paths(&plan), vec![old]);
        assert!(plan[0].bytes >= 8192);
        assert!(kept.exists());
    }

    #[test]
    fn bundles_touched_within_grace_are_kept() {
        let tmp = tempfile::tempdir().unwrap();
        bundle(tmp.path(), "sha256-fresh");
        let plan = plan_bundles(tmp.path(), &BTreeSet::new(), HOUR, SystemTime::now()).unwrap();
        assert!(plan.is_empty());
    }

    #[test]
    fn leftovers_skip_builds_while_a_deployment_is_in_flight() {
        let tmp = tempfile::tempdir().unwrap();
        let artifacts = tmp.path().join("artifacts");
        let uploads = tmp.path().join("uploads");
        let b = bundle(&artifacts, "sha256-aaa");
        fs::create_dir_all(artifacts.join("builds/one")).unwrap();
        fs::create_dir_all(artifacts.join("git-checkouts/two")).unwrap();
        fs::create_dir_all(b.join("rootfs.0192.tmp")).unwrap();
        fs::create_dir_all(uploads.join("up1")).unwrap();

        let idle = plan_leftovers(&artifacts, &uploads, false, HOUR, later()).unwrap();
        assert_eq!(
            paths(&idle),
            vec![
                artifacts.join("builds/one"),
                artifacts.join("git-checkouts/two"),
                b.join("rootfs.0192.tmp"),
                uploads.join("up1"),
            ]
        );

        let busy = plan_leftovers(&artifacts, &uploads, true, HOUR, later()).unwrap();
        assert_eq!(paths(&busy), vec![uploads.join("up1")]);
    }

    #[test]
    fn fresh_uploads_are_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let uploads = tmp.path().join("uploads");
        fs::create_dir_all(uploads.join("up1")).unwrap();
        let plan = plan_leftovers(tmp.path(), &uploads, false, HOUR, SystemTime::now()).unwrap();
        assert!(plan.is_empty());
    }

    #[test]
    fn logs_truncate_live_services_and_remove_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let live = Uuid::now_v7();
        let gone = Uuid::now_v7();
        let active_dep = Uuid::now_v7();
        let old_dep = Uuid::now_v7();
        fs::create_dir_all(tmp.path().join("deployments")).unwrap();
        fs::write(tmp.path().join(format!("{live}.log")), "x".repeat(5000)).unwrap();
        fs::write(tmp.path().join(format!("{gone}.log")), "y").unwrap();
        fs::write(tmp.path().join("notes.txt"), "keep").unwrap();
        fs::write(
            tmp.path().join(format!("deployments/{active_dep}.log")),
            "a",
        )
        .unwrap();
        fs::write(tmp.path().join(format!("deployments/{old_dep}.log")), "b").unwrap();

        let plan = plan_logs(
            tmp.path(),
            &BTreeSet::from([live]),
            &BTreeSet::from([active_dep]),
        )
        .unwrap();

        let by_path = |p: PathBuf| plan.iter().find(|c| c.path == p).map(|c| c.action);
        assert_eq!(
            by_path(tmp.path().join(format!("{live}.log"))),
            Some(Action::Truncate)
        );
        assert_eq!(
            by_path(tmp.path().join(format!("{gone}.log"))),
            Some(Action::Remove)
        );
        assert_eq!(
            by_path(tmp.path().join(format!("deployments/{old_dep}.log"))),
            Some(Action::Remove)
        );
        assert_eq!(plan.len(), 3);
    }

    #[test]
    fn apply_removes_truncates_and_reports_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = bundle(tmp.path(), "sha256-bbb");
        let log = tmp.path().join("live.log");
        fs::write(&log, "z".repeat(10_000)).unwrap();

        let plan = vec![
            Candidate {
                path: dir.clone(),
                bytes: 0,
                action: Action::Remove,
            },
            Candidate {
                path: log.clone(),
                bytes: 0,
                action: Action::Truncate,
            },
        ];
        let applied = apply(&plan, &[tmp.path().to_path_buf()]);

        assert_eq!(applied.entries, 2);
        assert!(applied.bytes >= 8192);
        assert!(applied.errors.is_empty(), "{:?}", applied.errors);
        assert!(!dir.exists());
        assert_eq!(fs::metadata(&log).unwrap().len(), 0);
    }

    #[test]
    fn apply_refuses_paths_outside_allowed_roots() {
        let allowed = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let victim = other.path().join("victim");
        fs::write(&victim, "data").unwrap();

        let plan = vec![Candidate {
            path: victim.clone(),
            bytes: 4,
            action: Action::Remove,
        }];
        let applied = apply(&plan, &[allowed.path().to_path_buf()]);

        assert_eq!(applied.entries, 0);
        assert_eq!(applied.errors.len(), 1);
        assert!(victim.exists());
    }

    #[test]
    fn apply_does_not_follow_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("precious"), "data").unwrap();
        let link = tmp.path().join("sha256-link");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();

        let plan = vec![Candidate {
            path: link.clone(),
            bytes: 0,
            action: Action::Remove,
        }];
        apply(&plan, &[tmp.path().to_path_buf()]);

        assert!(outside.path().join("precious").exists());
    }
}
