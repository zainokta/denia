//! systemd unit rendering. The systemctl wrappers land in Task 11.

use super::paths::InstallContext;
use sha2::Digest;

const TEMPLATE: &str = include_str!("../../templates/denia.service.in");

/// Render the operator-aware systemd unit text for `denia.service`.
pub fn render_unit(ctx: &InstallContext) -> String {
    TEMPLATE
        .replace("{{denia_bin}}", "/usr/local/bin/denia")
        .replace("{{denia_user}}", "denia")
        .replace("{{denia_group}}", "denia")
        .replace("{{denia_home}}", "/var/lib/denia")
        .replace("{{config_file}}", &ctx.config_file.display().to_string())
        .replace("{{age_key_file}}", &ctx.age_key_file.display().to_string())
        .replace("{{token_file}}", &ctx.token_file.display().to_string())
        .replace("{{user_config_dir}}", &ctx.user_config_dir.display().to_string())
}

/// SHA-256 of the rendered unit. Used by `denia doctor` (Task 16) to detect
/// hand-edited drift in `/etc/systemd/system/denia.service`.
pub fn unit_sha256(ctx: &InstallContext) -> [u8; 32] {
    let mut hasher = sha2::Sha256::new();
    hasher.update(render_unit(ctx).as_bytes());
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_contains_required_directives() {
        let ctx = InstallContext::from_user("rakei", "/home/rakei");
        let unit = render_unit(&ctx);
        for needle in [
            "User=denia",
            "Group=denia",
            "WorkingDirectory=/var/lib/denia",
            "Environment=DENIA_CONFIG_FILE=/home/rakei/.config/denia/config.toml",
            "Environment=SOPS_AGE_KEY_FILE=/home/rakei/.config/denia/age.key",
            "EnvironmentFile=/home/rakei/.config/denia/admin.token",
            "ExecStart=/usr/local/bin/denia",
            "BindReadOnlyPaths=/home/rakei/.config/denia",
            "AmbientCapabilities=CAP_NET_BIND_SERVICE CAP_SYS_ADMIN CAP_SETUID CAP_SETGID",
            "Delegate=yes",
            "ProtectHome=true",
            "Conflicts=traefik.service nginx.service caddy.service apache2.service httpd.service",
        ] {
            assert!(unit.contains(needle), "expected `{needle}` in unit:\n{unit}");
        }
    }

    #[test]
    fn unit_hash_is_stable_for_same_inputs() {
        let ctx = InstallContext::from_user("rakei", "/home/rakei");
        assert_eq!(unit_sha256(&ctx), unit_sha256(&ctx));
    }

    #[test]
    fn unit_hash_changes_per_install_context() {
        let a = InstallContext::from_user("rakei", "/home/rakei");
        let b = InstallContext::from_user("ops", "/home/ops");
        assert_ne!(unit_sha256(&a), unit_sha256(&b));
    }
}
