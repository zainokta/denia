//! `denia init`: scaffold a `.denia` manifest in a project directory so
//! `denia create` and `denia push` can resolve a target without flags. The
//! template carries the minimum required fields uncommented and the optional
//! ones commented, for the operator to edit (ADR-039).

use std::path::PathBuf;

use clap::Args;

#[derive(Args, Debug)]
pub struct InitArgs {
    /// Directory to write the `.denia` manifest into (default: `.`).
    #[arg(long, value_name = "DIR", default_value = ".")]
    pub path: PathBuf,

    /// Overwrite an existing `.denia` manifest.
    #[arg(long)]
    pub force: bool,

    /// Seed the project name in the template.
    #[arg(long)]
    pub project: Option<String>,

    /// Seed the service name in the template.
    #[arg(long)]
    pub service: Option<String>,

    /// Seed the listen port in the `[create]` block.
    #[arg(long)]
    pub port: Option<u16>,
}

pub fn run(args: InitArgs) -> anyhow::Result<()> {
    let manifest_path = args.path.join(".denia");
    if manifest_path.exists() && !args.force {
        anyhow::bail!(
            "{} already exists; pass --force to overwrite",
            manifest_path.display()
        );
    }

    let project = args.project.as_deref().unwrap_or("default");
    let service = args.service.as_deref().unwrap_or("my-service");
    let port = args.port.unwrap_or(8080);

    std::fs::write(&manifest_path, render_template(project, service, port))?;
    println!("Wrote {}", manifest_path.display());
    println!("Edit it, then run `denia create` followed by `denia push`.");
    Ok(())
}

/// Build the commented `.denia` template. Kept as a literal string (not
/// `toml::to_string`) so the guidance comments survive.
fn render_template(project: &str, service: &str, port: u16) -> String {
    format!(
        "# Denia project manifest. Edit the values below, then run:\n\
         #   denia create   # create the service (upload source)\n\
         #   denia push     # build the working tree and deploy\n\
         \n\
         project = \"{project}\"\n\
         service = \"{service}\"\n\
         \n\
         # Build inputs (optional; shown with their defaults).\n\
         # dockerfile = \"Dockerfile\"\n\
         # context    = \".\"\n\
         \n\
         # Service-creation defaults consumed by `denia create`.\n\
         [create]\n\
         port = {port}\n\
         # health_path = \"/healthz\"\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::client::manifest::DeniaManifest;

    #[test]
    fn rendered_template_parses_back() {
        let raw = render_template("acme", "web", 3000);
        let m = DeniaManifest::parse(&raw).expect("template must be valid .denia");
        assert_eq!(m.project, "acme");
        assert_eq!(m.service, "web");
        assert_eq!(m.create.as_ref().unwrap().port, 3000);
        // Optional fields stay at their defaults (commented out).
        assert_eq!(m.dockerfile(), "Dockerfile");
        assert_eq!(m.context(), ".");
    }
}
