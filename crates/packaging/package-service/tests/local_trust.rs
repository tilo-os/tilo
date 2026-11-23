use crate::common;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use tilo_app_package::trust::{RevocationList, RevokedKey, TrustFile};
use tilo_test_env::SpawnUnderEnvLock as _;

use crate::fixture;

struct Host(Child);
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn host() -> PathBuf {
    tilo_test_env::package_binary!("tilo-package-service")
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
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let _host = Host(command.spawn_locked().unwrap());
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
    // The person pins it from a terminal (`tilo-install pin`), which is this action.
    common::person_client(&socket)
        .change(
            &root.path().join("data/tilo-apps"),
            package_service::wire::Change::SetPinned {
                app: "so.tilo.unlisted-local".into(),
                pinned: true,
            },
            None,
        )
        .unwrap();
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
