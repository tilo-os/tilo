//! The untrusted settings copy enters the records only through checked host access.

#[cfg(unix)]
use serde_json::{Value, json};
#[cfg(unix)]
use settings_store::SettingsSource;
use settings_store::{SessionPaths, SessionSettings};

#[test]
#[cfg(unix)]
fn host_refresh_does_not_import_a_symlink_target() {
    let root = tempfile::tempdir().unwrap();
    let trusted_root = root.path().canonicalize().unwrap();
    let paths = SessionPaths::beside(trusted_root.join("session/settings.json"));
    let session = SessionSettings::open_checked(paths.clone()).unwrap();
    session
        .update_section(
            "region",
            json!({"units": "metric"}).as_object().unwrap().clone(),
        )
        .unwrap();
    let before = session.load();
    let history = session.change_log();
    let revision = session.revision();
    let records = [
        std::fs::read(&paths.person).ok(),
        std::fs::read(&paths.device).ok(),
    ];
    let private = trusted_root.join("host-only.json");
    let sentinel = br#"{"region":{"units":"private-copy-sentinel"}}"#;
    std::fs::write(&private, sentinel).unwrap();

    // Exercise both entry points an app can replace in the writable directory.
    for entry in [
        paths.copy.clone(),
        paths.copy.with_file_name("settings.json.lock"),
    ] {
        let saved = std::fs::read(&entry).unwrap();
        std::fs::remove_file(&entry).unwrap();
        std::os::unix::fs::symlink(&private, &entry).unwrap();
        session.refresh();
        assert_eq!(session.load(), before);
        assert_eq!(session.revision(), revision);
        assert_eq!(session.change_log(), history);
        assert!(
            session
                .update_section(
                    "region",
                    json!({"units": "imperial"}).as_object().unwrap().clone()
                )
                .is_err()
        );
        assert_eq!(std::fs::read(&paths.person).ok(), records[0]);
        assert_eq!(std::fs::read(&paths.device).ok(), records[1]);
        assert_eq!(std::fs::read(&private).unwrap(), sentinel);
        assert!(
            std::fs::symlink_metadata(&entry)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        std::fs::remove_file(&entry).unwrap();
        std::fs::write(&entry, saved).unwrap();
    }

    // A normal explicit-path writer still shares the lock and reaches the host.
    settings_store::update_section_at(
        &paths.copy,
        "region",
        json!({"units": "imperial"}).as_object().unwrap().clone(),
    )
    .unwrap();
    session.refresh();
    assert_eq!(session.load()["region"]["units"], "imperial");
    assert_eq!(
        Value::Object(settings_store::load_at(&paths.device))["region"]["units"],
        "imperial"
    );
}

#[test]
#[cfg(unix)]
fn trusted_home_alias_is_resolved_but_copy_directory_links_are_not() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let home = root.join("physical-home");
    std::fs::create_dir(&home).unwrap();
    let alias = root.join("home-alias");
    std::os::unix::fs::symlink(&home, &alias).unwrap();
    let paths = SessionPaths::beside(alias.join(".config/tilo/settings.json"))
        .resolve_trusted_root(&alias)
        .unwrap();
    assert_eq!(paths.copy, home.join(".config/tilo/settings.json"));
    let session = SessionSettings::open_checked(paths.clone()).unwrap();
    session
        .update_section(
            "region",
            json!({"units": "metric"}).as_object().unwrap().clone(),
        )
        .unwrap();
    assert_eq!(session.load()["region"]["units"], "metric");
    drop(session);
    std::fs::rename(home.join(".config/tilo"), home.join("private")).unwrap();
    std::os::unix::fs::symlink(home.join("private"), home.join(".config/tilo")).unwrap();
    assert!(SessionSettings::open_checked(paths).is_err());
}

#[test]
#[cfg(not(unix))]
fn checked_host_copy_has_no_unchecked_platform_fallback() {
    let root = tempfile::tempdir().unwrap();
    let trusted_root = root.path().canonicalize().unwrap();
    let paths = SessionPaths::beside(trusted_root.join("settings.json"));
    assert_eq!(
        SessionSettings::open_checked(paths).err().unwrap().kind(),
        std::io::ErrorKind::Unsupported
    );
}
