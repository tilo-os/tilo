//! `app install <file>` and `app update <app> --from <file>`: a package
//! that came from no repository follows the app host's rule. A publisher
//! this person trusts installs; a self-signed one is named as unverified
//! and, with no terminal to ask on, refused unless `--yes` is given. A
//! revoked publisher is refused whatever is given.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::Path;
use std::process::{Command, Output, Stdio};

use tilo_app_package::trust::{RevocationList, RevokedKey, TrustFile};

use crate::registry_to_installed::{
    author_tapp_version, scoped_env, test_publisher, tilo_install_bin,
};

const ID: &str = "so.tilo.package-file-e2e";

fn app(args: &[&str]) -> Output {
    Command::new(tilo_test_env::package_binary!("app"))
        .args(args)
        // No terminal: the person cannot be asked.
        .stdin(Stdio::null())
        .output()
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
    for dir in [&config, &cache, &apps, &xdg_config] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let installer = tilo_install_bin();
    let _env = scoped_env(&[
        ("TILO_STORE_CONFIG_DIR", &config),
        ("TILO_STORE_CACHE_DIR", &cache),
        ("TILO_APPS_DATA_DIR", &apps),
        ("XDG_CONFIG_HOME", &xdg_config),
        ("TILO_INSTALL_BIN", &installer),
    ]);
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

    // --yes accepts it.
    let accepted = app(&["install", &first, "--yes"]);
    assert!(accepted.status.success(), "{}", text(&accepted));
    assert_eq!(
        app_cli::installed_version(ID).unwrap().as_deref(),
        Some("1.4.0")
    );

    // Naming the publisher makes it trusted: no question, no --yes.
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

    // The old flag still means --yes.
    let reinstalled = app(&["install", &second, "--allow-self-signed"]);
    assert!(reinstalled.status.success(), "{}", text(&reinstalled));

    // A revoked publisher is refused, --yes or not.
    let tilo = xdg_config.join("tilo");
    std::fs::create_dir_all(&tilo).unwrap();
    RevocationList {
        revoked: vec![RevokedKey {
            pubkey_hex: publisher,
            reason: "test".into(),
        }],
    }
    .save(&tilo.join("revoked-keys.toml"))
    .unwrap();
    let revoked = app(&["install", &first, "--yes"]);
    assert!(!revoked.status.success(), "{}", text(&revoked));
    assert!(text(&revoked).contains("revoked"), "{}", text(&revoked));
}

#[test]
fn file_only_options_refuse_a_repository_app() {
    let refused = app(&["install", "sheets", "--yes"]);
    assert!(!refused.status.success());
    assert!(
        text(&refused).contains("--yes is for a package file"),
        "{}",
        text(&refused)
    );
}
