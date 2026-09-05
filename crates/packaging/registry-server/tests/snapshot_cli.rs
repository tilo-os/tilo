#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "integration-test setup and assertions"
)]

use std::os::unix::fs::PermissionsExt as _;
use std::process::Command;

use ed25519_dalek::SigningKey;
use registry_server::{PublisherAllowlist, PublisherRow, ServerIdentity, Storage};
use registry_types::{RegistryEntry, RegistryVersion, sign_entry};

#[test]
fn offline_command_publishes_a_verifiable_snapshot_without_server_private_key() {
    let temp = tempfile::tempdir().unwrap();
    let offline = tempfile::tempdir().unwrap();
    let storage = Storage::open(temp.path()).unwrap();
    let owner = SigningKey::from_bytes(&[31; 32]);
    let publisher = SigningKey::from_bytes(&[32; 32]);
    let owner_hex = hex::encode(owner.verifying_key().to_bytes());
    let publisher_hex = hex::encode(publisher.verifying_key().to_bytes());
    storage
        .save_identity(&ServerIdentity {
            owner_pubkey_hex: owner_hex.clone(),
            name: "test".into(),
            description: "test".into(),
            base_url: "https://registry.test".into(),
        })
        .unwrap();
    storage
        .save_allowlist(&PublisherAllowlist {
            publishers: vec![PublisherRow {
                pubkey_hex: publisher_hex.clone(),
                can_add_new: true,
                note: "not public".into(),
            }],
        })
        .unwrap();
    let mut entry = RegistryEntry {
        package_id: "dev.qos.test".into(),
        name: "Test".into(),
        description: "Test app".into(),
        categories: vec!["Test".into()],
        icon_url: None,
        source_url: None,
        homepage: None,
        publisher_pubkey_hex: publisher_hex,
        versions: vec![RegistryVersion {
            version: "1".into(),
            qpkg_url: "https://registry.test/test.qpkg".into(),
            qpkg_root_hash_hex: "ab".repeat(32),
            qpkg_size_bytes: 1,
            released_at_unix: 1,
        }],
        updated_at_unix: 1,
        signature_hex: String::new(),
    };
    sign_entry(&mut entry, &publisher);
    storage.save_entry(&entry).unwrap();

    let key_path = offline.path().join("offline-owner.seed");
    std::fs::write(&key_path, hex::encode(owner.to_bytes())).unwrap();
    std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_qos-registry-server"))
        .arg("--data-dir")
        .arg(temp.path())
        .arg("sign-snapshot")
        .arg("--owner-key-file")
        .arg(&key_path)
        .arg("--valid-for-seconds")
        .arg("3600")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let snapshot = storage.load_snapshot().unwrap().unwrap();
    registry_types::verify_snapshot(&snapshot, &owner_hex, snapshot.issued_at_unix).unwrap();
    assert_eq!(snapshot.entries, vec![entry]);
    assert_eq!(snapshot.publishers.len(), 1);
    assert_eq!(
        snapshot.publishers[0].pubkey_hex,
        snapshot.entries[0].publisher_pubkey_hex
    );
    assert!(
        std::fs::read_dir(temp.path()).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains("seed")),
        "the serving data directory must contain no owner private-key file"
    );

    std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let refused = Command::new(env!("CARGO_BIN_EXE_qos-registry-server"))
        .arg("--data-dir")
        .arg(temp.path())
        .arg("sign-snapshot")
        .arg("--owner-key-file")
        .arg(&key_path)
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert_eq!(storage.load_snapshot().unwrap().unwrap().sequence, 1);
}
