//! BuildKit cache step of the operator prune (ADR-040), driven through
//! `buildctl` against the Denia-provisioned `buildkitd`.

use serde::Deserialize;

use crate::command::CommandRunner;

/// `{{json .}}` is the stable machine-readable output of `buildctl du` (a JSON
/// array of records) and `buildctl prune` (one record per line).
const JSON_FORMAT: &str = "{{json .}}";

#[derive(Debug, Deserialize)]
struct UsageRecord {
    #[serde(default)]
    size: i64,
    #[serde(default, rename = "inUse")]
    in_use: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BuildkitUsage {
    pub entries: u64,
    pub bytes: u64,
}

/// What `buildctl prune` would reclaim: every record not currently in use.
pub async fn plan(runner: &dyn CommandRunner, buildctl: &str) -> Result<BuildkitUsage, String> {
    let output = runner
        .run(buildctl, &["du", "--format", JSON_FORMAT])
        .await
        .map_err(|e| format!("buildctl du failed: {e}"))?;
    parse_du(&output.stdout)
}

/// Run `buildctl prune` and sum what it reports as removed.
pub async fn prune(runner: &dyn CommandRunner, buildctl: &str) -> Result<BuildkitUsage, String> {
    let output = runner
        .run(buildctl, &["prune", "--format", JSON_FORMAT])
        .await
        .map_err(|e| format!("buildctl prune failed: {e}"))?;
    parse_prune(&output.stdout)
}

fn parse_du(stdout: &str) -> Result<BuildkitUsage, String> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() || trimmed == "null" {
        return Ok(BuildkitUsage::default());
    }
    let records: Vec<UsageRecord> =
        serde_json::from_str(trimmed).map_err(|e| format!("unexpected buildctl du output: {e}"))?;
    Ok(sum(records.iter().filter(|r| !r.in_use)))
}

fn parse_prune(stdout: &str) -> Result<BuildkitUsage, String> {
    let records = stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(serde_json::from_str::<UsageRecord>)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("unexpected buildctl prune output: {e}"))?;
    Ok(sum(records.iter()))
}

fn sum<'a>(records: impl Iterator<Item = &'a UsageRecord>) -> BuildkitUsage {
    records.fold(BuildkitUsage::default(), |acc, r| BuildkitUsage {
        entries: acc.entries + 1,
        bytes: acc.bytes + r.size.max(0) as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{CommandOutput, FakeCommandRunner};

    fn ok(stdout: &str) -> CommandOutput {
        CommandOutput {
            status: 0,
            stdout: stdout.to_string(),
            stderr: String::new(),
        }
    }

    #[tokio::test]
    async fn plan_counts_only_records_not_in_use() {
        let runner = FakeCommandRunner::new(vec![ok(
            r#"[{"id":"a","size":100,"inUse":false},{"id":"b","size":50,"inUse":true},{"id":"c","size":7}]"#,
        )]);
        let usage = plan(&runner, "buildctl").await.unwrap();
        assert_eq!(
            usage,
            BuildkitUsage {
                entries: 2,
                bytes: 107
            }
        );
        assert_eq!(runner.commands(), vec!["buildctl du --format {{json .}}"]);
    }

    #[tokio::test]
    async fn prune_sums_one_record_per_line() {
        let runner = FakeCommandRunner::new(vec![ok(
            "{\"id\":\"a\",\"size\":100}\n{\"id\":\"b\",\"size\":23}\n",
        )]);
        let usage = prune(&runner, "buildctl").await.unwrap();
        assert_eq!(
            usage,
            BuildkitUsage {
                entries: 2,
                bytes: 123
            }
        );
        assert_eq!(
            runner.commands(),
            vec!["buildctl prune --format {{json .}}"]
        );
    }

    #[tokio::test]
    async fn empty_cache_is_zero() {
        let runner = FakeCommandRunner::new(vec![ok("null\n")]);
        assert_eq!(
            plan(&runner, "buildctl").await.unwrap(),
            BuildkitUsage::default()
        );
    }

    #[tokio::test]
    async fn runner_failure_is_reported_not_fatal() {
        let runner = FakeCommandRunner::new(vec![]);
        let err = plan(&runner, "buildctl").await.unwrap_err();
        assert!(err.starts_with("buildctl du failed"), "{err}");
    }
}
