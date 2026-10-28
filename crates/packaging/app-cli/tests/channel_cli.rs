use std::process::Command;

#[test]
fn channel_selection_is_explicit_and_persisted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("registries.toml");
    std::fs::write(&path, format!(
        "[[registry]]\nname = 'team'\nbase_url = 'https://apps.tilo.so'\nowner_pubkey_hex = '{}'\nenabled = true\n",
        "12".repeat(32))).unwrap();
    for channel in ["development", "stable"] {
        let result = Command::new(tilo_test_env::package_binary!("app"))
            .args(["repo", "channel", "team", channel])
            .env("TILO_STORE_CONFIG_DIR", dir.path())
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
        let config: app_cli::RegistriesFile =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            config.registries[0].channel == tilo_app_package::version::ReleaseChannel::Development,
            channel == "development"
        );
        assert_eq!(config.registries[0].owner_pubkey_hex, "12".repeat(32));
    }
    let before = std::fs::read(&path).unwrap();
    let result = Command::new(tilo_test_env::package_binary!("app"))
        .args(["repo", "channel", "team", "typo"])
        .env("TILO_STORE_CONFIG_DIR", dir.path())
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
