use crate::common;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use tilo_app_package::trust::{RevocationList, RevokedKey, TrustFile};

#[path = "../src/local_package/fixture.rs"]
mod fixture;

struct Host(Child);
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn host() -> PathBuf {
    tilo_test_env::package_binary!("tilo-app-host")
}

fn installer() -> PathBuf {
    let path = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tilo-install");
    assert!(
        path.is_file(),
        "Build the actual tilo-install binary before this integration test"
    );
    path
}

fn isolated_command(binary: &Path, root: &Path) -> Command {
    let mut command = Command::new(binary);
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"));
    command
}

#[test]
fn local_package_real_host_installs_full_cached_thin_and_refuses_stale_review_and_revocation() {
    let root = tempfile::tempdir().unwrap();
    let (full, thin, trust, publisher) = fixture::fixture();
    let config = root.path().join("config/tilo");
    std::fs::create_dir_all(&config).unwrap();
    trust.save(&config.join("trusted-attestors.toml")).unwrap();
    let socket = root.path().join("host.sock");
    let mut command = isolated_command(&host(), root.path());
    command
        .arg("--socket")
        .arg(&socket)
        .arg("--max-store-bytes")
        .arg("0")
        .env("TILO_INSTALL_BIN", installer())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let _host = Host(command.spawn().unwrap());
    let client = common::store_client(&socket);
    let checked = client.review_local(&full).unwrap();
    assert_eq!(checked.manifest.package.id, "so.tilo.unlisted-local");
    assert!(checked.installed_version.is_none());
    // Both are valid trusted encodings of the same signed root. Review
    // of one cannot authorize replacing handed bytes with the other.
    assert!(
        client
            .install_local(&thin, &checked)
            .expect_err("The canonical host must refuse this install")
            .contains("changed since review")
    );
    let id = client.install_local(&full, &checked).unwrap();
    assert_eq!(id.to_hex(), checked.root);
    let thin_review = client.review_local(&thin).unwrap();
    assert_eq!(thin_review.installed_version.as_deref(), Some("1.0.0"));
    let mut pin = isolated_command(&installer(), root.path());
    assert!(
        pin.args(["pin", "so.tilo.unlisted-local"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(client.install_local(&thin, &thin_review).is_ok());
    assert!(client.review_local(&thin).unwrap().pinned);
    let (replacement, _, replacement_trust, _) = fixture::fixture();
    replacement_trust
        .save(&config.join("trusted-attestors.toml"))
        .unwrap();
    let replacement_review = client.review_local(&replacement).unwrap();
    let refusal = client
        .install_local(&replacement, &replacement_review)
        .expect_err("The canonical host must refuse this install");
    assert!(refusal.contains("changed publisher"), "{refusal}");
    trust.save(&config.join("trusted-attestors.toml")).unwrap();
    assert_eq!(client.review_local(&full).unwrap().root, checked.root);
    RevocationList {
        revoked: vec![RevokedKey {
            pubkey_hex: publisher,
            reason: "fixture revocation after review".into(),
        }],
    }
    .save(&config.join("revoked-keys.toml"))
    .unwrap();
    assert!(client.review_local(&full).is_err());
    assert!(client.install_local(&full, &checked).is_err());
    assert!(root.path().join("data/tilo-apps").is_dir());
}
