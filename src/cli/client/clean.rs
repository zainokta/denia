//! `denia clean`: show what the node can reclaim, confirm, then prune it.
//! Thin client over `/v1/system/prune`; the daemon does the work. See ADR-040.

use std::io::{BufRead, IsTerminal, Write};

use clap::Args;

use super::http::{ClientApi, PruneReportView};
use super::profile::{ClientConfig, config_path};

#[derive(Args, Debug)]
pub struct CleanArgs {
    /// Delete without asking for confirmation.
    #[arg(long, short = 'y')]
    pub yes: bool,

    /// Only print what would be reclaimed; delete nothing.
    #[arg(long, conflicts_with = "yes")]
    pub dry_run: bool,

    /// Profile to use; defaults to the active profile.
    #[arg(long)]
    pub profile: Option<String>,
}

pub async fn run(args: CleanArgs) -> anyhow::Result<()> {
    let cfg = ClientConfig::load_from(&config_path()?)?;
    let profile = match args.profile.as_deref() {
        Some(name) => cfg
            .profiles
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("profile '{name}' not found"))?,
        None => cfg.active_profile()?,
    };
    let api = ClientApi::new(&profile.url);
    let token = &profile.token;

    let plan = api.prune_plan(token).await?;
    print!("{}", render(&plan));
    if args.dry_run {
        return Ok(());
    }
    if plan.total_bytes == 0 {
        println!("Nothing to clean.");
        return Ok(());
    }
    if !args.yes {
        if !std::io::stdin().is_terminal() {
            anyhow::bail!("refusing to delete without confirmation; re-run with --yes");
        }
        if !confirm(plan.total_bytes)? {
            println!("Aborted; nothing deleted.");
            return Ok(());
        }
    }

    let result = api.prune_execute(token).await?;
    print!("{}", render(&result));
    Ok(())
}

fn confirm(total_bytes: u64) -> anyhow::Result<bool> {
    print!("Delete and free about {}? [y/N] ", human_bytes(total_bytes));
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(line.trim(), "y" | "Y" | "yes" | "Yes"))
}

/// Table of categories, then a total line. Category errors are listed below
/// the table so one failed store is visible but does not hide the rest.
fn render(report: &PruneReportView) -> String {
    let heading = if report.executed {
        "Freed"
    } else {
        "Reclaimable"
    };
    let mut out = format!("{:<18} {:>8} {:>12}\n", "Category", "Entries", heading);
    for c in &report.categories {
        let bytes = if c.error.is_some() && c.bytes == 0 {
            "error".to_string()
        } else {
            human_bytes(c.bytes)
        };
        out.push_str(&format!(
            "{:<18} {:>8} {:>12}\n",
            label(&c.category),
            c.entries,
            bytes
        ));
    }
    out.push_str(&format!(
        "{:<18} {:>8} {:>12}\n",
        "Total",
        "",
        human_bytes(report.total_bytes)
    ));
    for c in report.categories.iter().filter(|c| c.error.is_some()) {
        out.push_str(&format!(
            "  {}: {}\n",
            label(&c.category),
            c.error.as_deref().unwrap_or_default()
        ));
    }
    out
}

fn label(category: &str) -> String {
    match category {
        "rootfs_bundles" => "Rootfs bundles".into(),
        "crash_leftovers" => "Crash leftovers".into(),
        "logs" => "Logs".into(),
        "oci_layer_cache" => "OCI layer cache".into(),
        "buildkit_cache" => "BuildKit cache".into(),
        other => other.replace('_', " "),
    }
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::client::http::PruneCategoryView;

    fn cat(category: &str, entries: u64, bytes: u64, error: Option<&str>) -> PruneCategoryView {
        PruneCategoryView {
            category: category.into(),
            entries,
            bytes,
            error: error.map(Into::into),
        }
    }

    #[test]
    fn human_bytes_picks_binary_units() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1536), "1.5 KiB");
        assert_eq!(human_bytes(6 * 1024 * 1024 * 1024), "6.0 GiB");
    }

    #[test]
    fn render_lists_categories_total_and_errors() {
        let report = PruneReportView {
            executed: false,
            total_bytes: 5 * 1024 * 1024 * 1024,
            categories: vec![
                cat("rootfs_bundles", 9, 5 * 1024 * 1024 * 1024, None),
                cat(
                    "buildkit_cache",
                    0,
                    0,
                    Some("buildctl du failed: connection refused"),
                ),
            ],
        };
        let text = render(&report);
        assert!(text.contains("Reclaimable"));
        assert!(text.contains("Rootfs bundles"));
        assert!(text.contains("5.0 GiB"));
        assert!(
            text.lines()
                .any(|l| l.starts_with("BuildKit cache") && l.ends_with("error"))
        );
        assert!(text.contains("BuildKit cache: buildctl du failed: connection refused"));
        assert!(text.lines().any(|l| l.starts_with("Total")));
    }

    #[test]
    fn render_after_execute_says_freed() {
        let report = PruneReportView {
            executed: true,
            total_bytes: 0,
            categories: vec![],
        };
        assert!(render(&report).contains("Freed"));
    }
}
