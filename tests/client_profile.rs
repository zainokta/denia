use denia::cli::client::profile::{ClientConfig, ClientProfile};

#[test]
fn save_and_load_profiles() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("profiles.toml");
    let cfg = ClientConfig {
        active: "default".into(),
        profiles: vec![ClientProfile {
            name: "default".into(),
            url: "https://denia.example.com".into(),
            token: "secret-token".into(),
        }],
    };
    cfg.save_to(&path).unwrap();
    let loaded = ClientConfig::load_from(&path).unwrap();
    assert_eq!(
        loaded.active_profile().unwrap().url,
        "https://denia.example.com"
    );
}

#[test]
fn missing_active_profile_errors() {
    let cfg = ClientConfig {
        active: "missing".into(),
        profiles: vec![],
    };
    assert!(cfg.active_profile().is_err());
}
