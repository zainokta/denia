#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_release_metadata_matches_supported_architectures() {
        assert_eq!(
            release_url_for_arch("x86_64").unwrap(),
            "https://github.com/project-zot/zot/releases/download/v2.1.20/zot-linux-amd64"
        );
        assert_eq!(
            expected_sha256_for_arch("x86_64").unwrap(),
            "a32e42d042d1f17b5b1317e55cc1a415a744c873dcd05c25c56b665478258bcb"
        );
        assert_eq!(
            release_url_for_arch("aarch64").unwrap(),
            "https://github.com/project-zot/zot/releases/download/v2.1.20/zot-linux-arm64"
        );
        assert_eq!(
            expected_sha256_for_arch("aarch64").unwrap(),
            "d6a39475587be18ec3d42e0d2bfa50f5c5064cbbcdc222dbd88e55ecf69dd8e9"
        );
    }

    #[test]
    fn config_is_loopback_only_and_uses_denia_storage() {
        let config = render_config();
        assert!(config.contains("\"address\": \"127.0.0.1\""));
        assert!(config.contains("\"port\": \"5000\""));
        assert!(config.contains("\"rootDirectory\": \"/var/lib/denia/zot\""));
        assert!(config.contains("\"gc\": true"));
        assert!(!config.contains("0.0.0.0"));
    }
}
