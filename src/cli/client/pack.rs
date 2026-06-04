//! Working-tree packer: walk a context directory, honour `.gitignore` and
//! `.dockerignore`, and stream the result into a `tar.zst` archive.

use std::path::{Path, PathBuf};

pub struct PackLimits {
    pub max_files: u64,
    pub max_bytes: u64,
}

/// Pack the working tree under `context_root` into a `tar.zst` at `out`.
///
/// File selection rules:
/// - `.gitignore` is honoured only when `context_root` is inside a git repo
///   (`WalkBuilder` default `require_git(true)`).
/// - `.dockerignore` is always honoured (added as a custom ignore filename).
/// - Hidden files / dotfiles are included unless an ignore rule excludes them.
/// - `dockerfile_rel` (relative to `context_root`) is always included even if
///   an ignore rule would otherwise exclude it.
/// - Files are added in sorted (deterministic) order.
/// - `limits` caps total file count and total uncompressed byte size.
pub fn pack_context(
    context_root: &Path,
    dockerfile_rel: &str,
    out: &Path,
    limits: &PackLimits,
) -> anyhow::Result<()> {
    let mut walk = ignore::WalkBuilder::new(context_root);
    walk.hidden(false) // include dotfiles like .env unless explicitly ignored
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .add_custom_ignore_filename(".dockerignore");

    let mut paths: Vec<PathBuf> = Vec::new();
    for dent in walk.build() {
        let dent = dent?;
        if dent.file_type().is_some_and(|t| t.is_file()) {
            paths.push(dent.path().to_path_buf());
        }
    }

    // Always include the Dockerfile even if an ignore rule excluded it.
    let dockerfile_abs = context_root.join(dockerfile_rel);
    if dockerfile_abs.is_file() && !paths.iter().any(|p| p == &dockerfile_abs) {
        paths.push(dockerfile_abs);
    }

    paths.sort(); // deterministic ordering

    if paths.len() as u64 > limits.max_files {
        anyhow::bail!(
            "context has {} files (limit {}); tighten .dockerignore",
            paths.len(),
            limits.max_files
        );
    }

    let mut total = 0u64;
    let file = std::fs::File::create(out)?;
    let enc = zstd::stream::write::Encoder::new(file, 0)?.auto_finish();
    let mut tar = tar::Builder::new(enc);

    for p in &paths {
        let rel = p.strip_prefix(context_root).unwrap_or(p);
        total += std::fs::metadata(p)?.len();
        if total > limits.max_bytes {
            anyhow::bail!(
                "context exceeds {} bytes; tighten .dockerignore",
                limits.max_bytes
            );
        }
        tar.append_path_with_name(p, rel)?;
    }

    tar.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    use crate::api::uploads::{ExtractLimits, extract_tar_zst};

    fn write(dir: &Path, name: &str, contents: &str) {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }

    fn pack_to_tmp(src: &TempDir, dockerfile_rel: &str, limits: &PackLimits) -> TempDir {
        let out_dir = TempDir::new().unwrap();
        let out = out_dir.path().join("ctx.tar.zst");
        pack_context(src.path(), dockerfile_rel, &out, limits).unwrap();
        out_dir
    }

    fn extract(out_dir: &TempDir) -> TempDir {
        let dest = TempDir::new().unwrap();
        let bytes = fs::read(out_dir.path().join("ctx.tar.zst")).unwrap();
        extract_tar_zst(
            &bytes,
            dest.path(),
            &ExtractLimits {
                max_uncompressed: 1 << 30,
                max_entries: 1000,
            },
        )
        .unwrap();
        dest
    }

    #[test]
    fn respects_dockerignore() {
        let src = TempDir::new().unwrap();
        write(src.path(), "a.txt", "hello");
        write(src.path(), "skip.log", "should be ignored");
        write(src.path(), ".dockerignore", "*.log\n");
        write(src.path(), "Dockerfile", "FROM scratch\n");

        let out_dir = pack_to_tmp(
            &src,
            "Dockerfile",
            &PackLimits {
                max_files: 100,
                max_bytes: 1 << 30,
            },
        );
        let dest = extract(&out_dir);

        assert!(dest.path().join("a.txt").exists(), "a.txt must be present");
        assert!(
            dest.path().join("Dockerfile").exists(),
            "Dockerfile must be present"
        );
        assert!(
            !dest.path().join("skip.log").exists(),
            "skip.log must be absent"
        );
    }

    #[test]
    fn always_includes_dockerfile_even_if_ignored() {
        let src = TempDir::new().unwrap();
        write(src.path(), "app.txt", "data");
        write(src.path(), ".dockerignore", "Dockerfile\n");
        write(src.path(), "Dockerfile", "FROM scratch\n");

        let out_dir = pack_to_tmp(
            &src,
            "Dockerfile",
            &PackLimits {
                max_files: 100,
                max_bytes: 1 << 30,
            },
        );
        let dest = extract(&out_dir);

        assert!(
            dest.path().join("Dockerfile").exists(),
            "Dockerfile must be present even when in .dockerignore"
        );
    }

    #[test]
    fn enforces_file_cap() {
        let src = TempDir::new().unwrap();
        write(src.path(), "a.txt", "1");
        write(src.path(), "b.txt", "2");

        let out_tmp = TempDir::new().unwrap();
        let out = out_tmp.path().join("ctx.tar.zst");
        let result = pack_context(
            src.path(),
            "Dockerfile",
            &out,
            &PackLimits {
                max_files: 1,
                max_bytes: 1 << 30,
            },
        );
        assert!(
            result.is_err(),
            "should fail when file count exceeds max_files"
        );
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("limit"), "error should mention 'limit': {msg}");
    }
}
