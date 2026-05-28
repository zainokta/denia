//! Renders the operator's `~/.config/denia/config.toml` from an
//! InstallContext. Plain string substitution; the source of the template
//! is `src/templates/config.toml.in` embedded via include_str!.

use super::paths::InstallContext;

const TEMPLATE: &str = include_str!("../../templates/config.toml.in");

pub fn render_config_toml(ctx: &InstallContext) -> String {
    TEMPLATE
        .replace(
            "{{user_config_dir}}",
            &ctx.user_config_dir.display().to_string(),
        )
        .replace("{{age_key_file}}", &ctx.age_key_file.display().to_string())
        .replace("{{token_file}}", &ctx.token_file.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::FileConfig;
    use std::path::PathBuf;

    #[test]
    fn rendered_config_parses_as_file_config() {
        let ctx = InstallContext::from_user("rakei", "/home/rakei");
        let toml_str = render_config_toml(&ctx);
        let parsed: FileConfig =
            toml::from_str(&toml_str).expect("rendered config.toml must parse as FileConfig");
        assert_eq!(
            parsed.age_key_file.as_deref(),
            Some(PathBuf::from("/home/rakei/.config/denia/age.key").as_path())
        );
    }

    #[test]
    fn rendered_config_contains_expected_paths() {
        let ctx = InstallContext::from_user("rakei", "/home/rakei");
        let toml_str = render_config_toml(&ctx);
        assert!(
            toml_str.contains("/home/rakei/.config/denia/age.key"),
            "expected age key path in rendered config:\n{toml_str}"
        );
    }
}
