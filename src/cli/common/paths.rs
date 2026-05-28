//! Resolved per-install paths. Built once at the top of every privileged
//! subcommand from `$SUDO_USER` + `getent passwd`.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallContext {
    pub install_user: String,
    pub install_home: PathBuf,
    pub user_config_dir: PathBuf,
    pub config_file: PathBuf,
    pub token_file: PathBuf,
    pub age_key_file: PathBuf,
}

impl InstallContext {
    /// Build paths from the installing operator's identity. Used in tests
    /// and behind `privilege::detect_install_user`.
    pub fn from_user(user: impl Into<String>, home: impl Into<PathBuf>) -> Self {
        let install_user = user.into();
        let install_home = home.into();
        let user_config_dir = install_home.join(".config/denia");
        Self {
            install_user,
            install_home,
            config_file: user_config_dir.join("config.toml"),
            token_file: user_config_dir.join("admin.token"),
            age_key_file: user_config_dir.join("age.key"),
            user_config_dir,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_derived_from_home() {
        let ctx = InstallContext::from_user("rakei", "/home/rakei");
        assert_eq!(ctx.install_user, "rakei");
        assert_eq!(ctx.install_home, PathBuf::from("/home/rakei"));
        assert_eq!(ctx.user_config_dir, PathBuf::from("/home/rakei/.config/denia"));
        assert_eq!(ctx.config_file, PathBuf::from("/home/rakei/.config/denia/config.toml"));
        assert_eq!(ctx.token_file, PathBuf::from("/home/rakei/.config/denia/admin.token"));
        assert_eq!(ctx.age_key_file, PathBuf::from("/home/rakei/.config/denia/age.key"));
    }
}
