use super::*;
use std::fs;

#[test]
fn discovery_projects_only_allowlisted_metadata_without_changing_sources() {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("settings");
    fs::create_dir_all(config.join("rclone")).unwrap();
    fs::create_dir_all(config.join("chromium/Default")).unwrap();
    let path = config.join("rclone/rclone.conf");
    let text = "[Files]\ntype=webdav\nurl=https://files.example.test/dav/\nuser=person\npass=PRIVATE_CONFIG_PASSWORD\ntoken=PRIVATE_CONFIG_TOKEN\n";
    fs::write(&path, text).unwrap();
    let inventory = discover(&SourceRoots {
        home: home.path().into(),
        config: config.clone(),
    });
    assert!(inventory.unreadable_sources.is_empty());
    let files = inventory
        .candidates
        .iter()
        .find(|candidate| candidate.origin == Origin::Rclone)
        .unwrap();
    assert_eq!(files.provider, Provider::WebDav);
    assert_eq!(files.identity.as_deref(), Some("person"));
    assert!(files.can_import_credentials);
    assert!(files.needs_sign_in);
    assert!(inventory.candidates.iter().any(|candidate| {
        candidate.origin == Origin::Browser && candidate.profile == config.join("chromium")
    }));
    let receipt = serde_json::to_string(&inventory).unwrap();
    assert!(!receipt.contains("PRIVATE_CONFIG_PASSWORD"));
    assert!(!receipt.contains("PRIVATE_CONFIG_TOKEN"));
    assert_eq!(fs::read_to_string(path).unwrap(), text);
}

#[cfg(unix)]
#[test]
fn redirected_configs_fail_closed_without_replacing_the_granted_config_root() {
    let home = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let config = home.path().join("settings");
    fs::create_dir_all(&config).unwrap();
    fs::create_dir_all(home.path().join(".config/chromium")).unwrap();
    fs::write(outside.path().join("rclone.conf"), "[Files]\ntype=webdav\n").unwrap();
    std::os::unix::fs::symlink(outside.path(), config.join("rclone")).unwrap();
    let inventory = discover(&SourceRoots {
        home: home.path().into(),
        config,
    });
    assert!(inventory.unreadable_sources.contains(&"rclone"));
    assert!(inventory.candidates.is_empty());
    // The independent Home-only entry point must not silently substitute .config.
    assert!(discover_home_profiles(home.path()).candidates.is_empty());
    assert_eq!(
        fs::read_to_string(outside.path().join("rclone.conf")).unwrap(),
        "[Files]\ntype=webdav\n"
    );
}

#[test]
fn metadata_dtos_preserve_frontend_wire_spellings() {
    assert_eq!(
        serde_json::to_string(&Provider::WebDav).unwrap(),
        "\"web-dav\""
    );
    assert_eq!(serde_json::to_string(&Origin::Gnome).unwrap(), "\"gnome\"");
    assert_eq!(
        serde_json::to_string(&Provider::Chromium).unwrap(),
        "\"chromium\""
    );
}
