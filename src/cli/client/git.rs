//! Git state checks for `denia push`: current branch, remote URL, and a guard
//! that local `HEAD` matches the upstream remote branch. See ADR-030.

use std::path::Path;
use std::process::Command;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GitError {
    #[error("git command failed: {0}")]
    Command(String),
    #[error("git output was empty")]
    EmptyOutput,
    #[error("local HEAD is not pushed to {remote}/{branch}")]
    UnpushedHead { remote: String, branch: String },
}

pub fn parse_branch_name(stdout: &str) -> Result<String, GitError> {
    let value = stdout.trim();
    if value.is_empty() {
        Err(GitError::EmptyOutput)
    } else {
        Ok(value.to_string())
    }
}

pub fn remote_head_matches(local: &str, remote: &str) -> bool {
    local.trim() == remote.trim()
}

pub fn current_branch(cwd: &Path) -> Result<String, GitError> {
    parse_branch_name(&git(cwd, &["branch", "--show-current"])?)
}

pub fn remote_url(cwd: &Path, remote: &str) -> Result<String, GitError> {
    let out = git(cwd, &["remote", "get-url", remote])?;
    let trimmed = out.trim();
    if trimmed.is_empty() {
        Err(GitError::EmptyOutput)
    } else {
        Ok(trimmed.to_string())
    }
}

pub fn ensure_head_pushed(cwd: &Path, remote: &str, branch: &str) -> Result<(), GitError> {
    let local = git(cwd, &["rev-parse", "HEAD"])?;
    let remote_ref = format!("{remote}/{branch}");
    let upstream = git(cwd, &["rev-parse", &remote_ref])?;
    if remote_head_matches(&local, &upstream) {
        Ok(())
    } else {
        Err(GitError::UnpushedHead {
            remote: remote.to_string(),
            branch: branch.to_string(),
        })
    }
}

fn git(cwd: &Path, args: &[&str]) -> Result<String, GitError> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| GitError::Command(e.to_string()))?;
    if !output.status.success() {
        return Err(GitError::Command(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}
