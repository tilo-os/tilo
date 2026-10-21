//! The daemon's side of the handoff through the real process boundary: a
//! `HandoffCredentialSource` whose refresher runs the
//! `tilo-account-credential` binary against an explicit config root.

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::Path;
use std::sync::Arc;

use accounts::{
    Account, AccountId, AccountStore, CommandRefresher, HandoffCredentialSource, HandoffStore,
    Secret, SecretVault,
};
use cloud_source::CredentialSource as _;

fn helper() -> std::path::PathBuf {
    tilo_test_env::package_binary!("tilo-account-credential")
}

#[test]
fn approved_bearer_retains_provider_expiry() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("tilo");
    let id = AccountId::from("music.spotify.streaming");
    let tokens = tilo_oauth::TokenSet {
        access_token: "fixture-approved-access".into(),
        refresh_token: "fixture-refresh-stays-in-vault".into(),
        expires_in: 3600,
        obtained_at: tilo_clock::unix_now(),
        scope: Some("streaming".into()),
    };
    save(
        &root,
        &id,
        "spotify",
        Some(&account_custody::oauth_secret(&tokens)),
    );
    let (bearer, expiry) = account_custody::Custodian::for_root(Some(root))
        .oauth_access_token_with_expiry(
            &id,
            account_custody::OAuthUse::new(
                account_custody::OAuthIssuer::Spotify,
                account_custody::Capability::MusicStreaming,
            ),
            false,
        )
        .unwrap();
    assert_eq!(bearer.as_str(), "fixture-approved-access");
    assert_eq!(expiry, tokens.expires_at());
}

fn save(root: &Path, id: &AccountId, type_id: &str, secret: Option<&Secret>) {
    if let Some(secret) = secret {
        let mut vault = SecretVault::open_in(root);
        vault.put(id, secret);
        vault.try_save().unwrap();
    }
    let mut store = AccountStore::load_in(root);
    let mut account = Account::new(id.clone(), type_id);
    account.config = serde_json::json!({"username": "ada", "base_url": "https://dav.example"});
    store.upsert(account);
    store.save_in(root).unwrap();
}

fn source(root: &Path, id: &AccountId) -> HandoffCredentialSource {
    HandoffCredentialSource::new(
        id.clone(),
        HandoffStore::open_in(root),
        Arc::new(CommandRefresher::new(Some(root.to_path_buf())).with_program(helper())),
    )
}

#[test]
fn helper_publishes_a_valid_access_token_but_never_the_refresh_token() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("tilo");
    let id = AccountId::from("drive-1");
    let tokens = tilo_oauth::TokenSet {
        access_token: "still-valid-access".into(),
        refresh_token: "long-lived-refresh".into(),
        expires_in: 3600,
        obtained_at: tilo_clock::unix_now(),
        scope: None,
    };
    save(
        &root,
        &id,
        account_custody::providers::GOOGLE_DRIVE_TYPE,
        Some(&account_custody::oauth_secret(&tokens)),
    );
    let credential = source(&root, &id).credential().unwrap();
    match credential {
        cloud_source::Credential::Bearer { token, expires } => {
            assert_eq!(token.as_str(), "still-valid-access");
            assert!(expires.is_some());
        }
        other => panic!("unexpected {other:?}"),
    }
    // The helper's own output carries no secret either.
    let output = std::process::Command::new(helper())
        .args(["refresh", id.as_str(), "--config-root"])
        .arg(&root)
        .output()
        .unwrap();
    assert!(output.status.success());
    let streams = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!streams.contains("long-lived-refresh") && !streams.contains("still-valid-access"));
}

#[test]
fn helper_exit_codes_reach_the_daemon_as_typed_errors() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("tilo");
    // An OAuth account without a grant needs sign-in.
    let unsigned = AccountId::from("drive-unsigned");
    save(
        &root,
        &unsigned,
        account_custody::providers::DROPBOX_TYPE,
        None,
    );
    assert!(matches!(
        source(&root, &unsigned).credential(),
        Err(cloud_source::Error::Unauthorized)
    ));
    // A WebDAV account hands off the server's own Basic credential.
    let dav = AccountId::from("dav-1");
    save(
        &root,
        &dav,
        account_custody::providers::WEBDAV_TYPE,
        Some(&Secret::password("dav-password")),
    );
    match source(&root, &dav).credential().unwrap() {
        cloud_source::Credential::Basic { username, password } => {
            assert_eq!(username, "ada");
            assert_eq!(password.as_str(), "dav-password");
        }
        other => panic!("unexpected {other:?}"),
    }
    let status = std::process::Command::new(helper())
        .args(["refresh", "missing", "--config-root"])
        .arg(&root)
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(accounts::handoff::exit::FAILED));
}

#[test]
fn capability_probe_runs_without_creating_custody_and_rejects_extra_arguments() {
    let directory = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(helper())
        .arg("capabilities")
        .env("HOME", directory.path())
        .env("XDG_CONFIG_HOME", directory.path().join("config"))
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
        serde_json::json!({"schema":1,"protected_grant_protocol":1}),
    );
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    let refused = std::process::Command::new(helper())
        .args(["capabilities", "--config-root", "ignored"])
        .output()
        .unwrap();
    assert_eq!(refused.status.code(), Some(64));
    assert!(refused.stdout.is_empty());
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
}
