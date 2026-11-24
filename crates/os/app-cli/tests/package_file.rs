//! `app install <file>` and `app update <app> --from <file>`: a package file from an unverified
//! publisher asks first, and is refused when there is nobody to ask.
//!
//! The programs run as production runs them: the CLI asks this computer's daemon for the change
//! over its native endpoint ([`crate::daemon_fixture::Daemon`]), so the install is the device's
//! own, not one the test's process made in-process.

#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output, Stdio};

use tilo_test_env::SpawnUnderEnvLock as _;

use crate::daemon_fixture::Daemon;
use crate::registry_fixture::{author_tapp_version, scoped_env, test_publisher};

const ID: &str = "so.tilo.package-file-e2e";

fn app(args: &[&str]) -> Output {
    Command::new(tilo_test_env::package_binary!("app"))
        .args(args)
        // No terminal: the person cannot be asked.
        .stdin(Stdio::null())
        .output_locked()
        .unwrap()
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn write_tapp(dir: &Path, version: &str) -> String {
    let path = dir.join(format!("sheets-{version}.tapp"));
    std::fs::write(&path, author_tapp_version(ID, version, &test_publisher())).unwrap();
    path.display().to_string()
}

#[test]
fn a_package_file_installs_by_the_app_hosts_rule() {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("config");
    let cache = home.path().join("cache");
    let apps = home.path().join("apps");
    let xdg_config = home.path().join("xdg-config");
    let xdg_data = home.path().join("xdg-data");
    let xdg_cache = home.path().join("xdg-cache");
    for dir in [&config, &cache, &apps, &xdg_config, &xdg_data, &xdg_cache] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let mut env = scoped_env(&[
        ("TILO_STORE_CONFIG_DIR", &config),
        ("TILO_STORE_CACHE_DIR", &cache),
        ("TILO_APPS_DATA_DIR", &apps),
        ("XDG_CONFIG_HOME", &xdg_config),
        ("XDG_DATA_HOME", &xdg_data),
        ("XDG_CACHE_HOME", &xdg_cache),
    ]);
    // The store this test makes is the whole answer: an image's own is not read over it.
    env.remove("TILO_APPS_SYSTEM_DIR");
    // The workspace trusts nobody and revokes nobody: a self-signed file is unverified.
    env.set("TILO_WORKSPACE_TRUST", r#"{"attestors":[],"revoked":[]}"#);
    env.set(
        "TILO_INSTALL_BIN",
        tilo_test_env::sibling_binary("install", "tilo-install"),
    );
    let _daemon = Daemon::start(&apps);
    let first = write_tapp(home.path(), "1.4.0");
    let second = write_tapp(home.path(), "1.5.0");
    let publisher = hex::encode(test_publisher().public_key_bytes());

    // Self-signed, nobody to ask: refused, naming the publisher.
    let refused = app(&["install", &first]);
    assert!(!refused.status.success(), "{}", text(&refused));
    let said = text(&refused);
    assert!(
        said.contains("unverified publisher") && said.contains(&publisher),
        "the refusal names the publisher as unverified: {said}"
    );
    assert!(said.contains("--yes"), "and says how to go ahead: {said}");
    assert_eq!(app_cli::installed_version(ID).unwrap(), None);

    let accepted = app(&["install", &first, "--yes"]);
    assert!(accepted.status.success(), "{}", text(&accepted));
    assert_eq!(
        app_cli::installed_version(ID).unwrap().as_deref(),
        Some("1.4.0")
    );

    let removed = app(&["remove", ID]);
    assert!(removed.status.success(), "{}", text(&removed));
    let expected = app(&["install", &first, "--require-publisher", &publisher]);
    assert!(expected.status.success(), "{}", text(&expected));

    // `update --from` takes the same rule, and the file must be this app.
    let refused = app(&["update", "package-file-e2e", "--from", &second]);
    assert!(!refused.status.success(), "{}", text(&refused));
    assert_eq!(
        app_cli::installed_version(ID).unwrap().as_deref(),
        Some("1.4.0")
    );
    let updated = app(&["update", "package-file-e2e", "--from", &second, "--yes"]);
    assert!(updated.status.success(), "{}", text(&updated));
    assert_eq!(
        app_cli::installed_version(ID).unwrap().as_deref(),
        Some("1.5.0")
    );

    let reinstalled = app(&["install", &second, "--allow-self-signed"]);
    assert!(reinstalled.status.success(), "{}", text(&reinstalled));

    // A revoked publisher is refused, --yes or not.
    env.set(
        "TILO_WORKSPACE_TRUST",
        serde_json::json!({
            "attestors": [],
            "revoked": [{"pubkey_hex": publisher, "reason": "test"}],
        })
        .to_string(),
    );
    let revoked = app(&["install", &first, "--yes"]);
    assert!(!revoked.status.success(), "{}", text(&revoked));
    assert!(text(&revoked).contains("revoked"), "{}", text(&revoked));

    // The file-only options stay with a package file: a repository app asked
    // for with `--yes` is refused rather than installed past the signature rule.
    let refused = app(&["install", "sheets", "--yes"]);
    assert!(!refused.status.success());
    assert!(
        text(&refused).contains("--yes is for a package file"),
        "{}",
        text(&refused)
    );
}
