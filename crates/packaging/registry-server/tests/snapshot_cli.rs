#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::similar_names,
    clippy::too_many_lines,
    reason = "integration-test setup and assertions"
)]

use std::os::unix::fs::PermissionsExt as _;
use std::process::{Command, Stdio};

use ed25519_dalek::SigningKey;
use registry_server::{PublisherAllowlist, PublisherRow, ServerIdentity, Storage};
use registry_types::{RegistryEntry, RegistryTarget, RegistryVersion, sign_entry};

fn wait_for_registry_lock_waiter(pid: u32) {
    let pid = pid.to_string();
    // This only bounds a process that never waits.
    let deadline = tilo_test_env::hang_deadline();
    while std::time::Instant::now() < deadline {
        let locks = std::fs::read_to_string("/proc/locks").unwrap();
        if locks.lines().any(|line| {
            line.contains("-> FLOCK") && line.split_whitespace().any(|field| field == pid)
        }) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("registry CLI process never waited on the held registry lock");
}

fn write_secret(path: &std::path::Path, key: &SigningKey) {
    std::fs::write(path, hex::encode(key.to_bytes())).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
}

fn identity(owner: &SigningKey) -> ServerIdentity {
    ServerIdentity {
        owner_pubkey_hex: hex::encode(owner.verifying_key().to_bytes()),
        name: "test".into(),
        description: "test".into(),
        base_url: "https://registry.test".into(),
    }
}

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
                namespaces: vec![registry_types::namespace::TILO_NAMESPACE.to_owned()],
            }],
        })
        .unwrap();
    let mut entry = RegistryEntry {
        package_id: "so.tilo.test".into(),
        kind: registry_types::EntryKind::App,
        name: "Test".into(),
        description: "Test app".into(),
        tagline: "Test on Tilo OS".into(),
        images: registry_types::LocalizedImages::english_icon(
            "https://apps.tilo.so/images/test-icon.svg",
            "Test icon",
        ),
        supported_languages: vec!["en".into()],
        categories: vec!["Test".into()],
        source_url: None,
        homepage: None,
        publisher_pubkey_hex: publisher_hex,
        versions: vec![RegistryVersion {
            version: "1".into(),
            released_at_unix: 1,
            artifact: registry_types::VersionArtifact::Tapp(registry_types::TappArtifact {
                tapp_url: "https://apps.tilo.so/artifacts/test-x86_64.tapp".into(),
                tapp_root_hash_hex: "ab".repeat(32),
                tapp_size_bytes: 1,
                tapp_blake3_hex: "cd".repeat(32),
                target: RegistryTarget {
                    platform: registry_types::TILO_OS_PLATFORM.into(),
                    architecture: "x86_64".into(),
                    isa: "x86_64-v3".into(),
                    runtime: None,
                    interfaces: Vec::new(),
                },
            }),
        }],
        updated_at_unix: 1,
        signature_hex: String::new(),
    };
    sign_entry(&mut entry, &publisher);
    storage.save_entry(&entry).unwrap();

    let key_path = offline.path().join("offline-owner.seed");
    std::fs::write(&key_path, hex::encode(owner.to_bytes())).unwrap();
    std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let output = Command::new(tilo_test_env::package_binary!("tilo-registry-server"))
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
    assert_eq!(snapshot.entries, vec![entry.clone()]);
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

    let mut replacement = entry;
    replacement.name = "Changed".into();
    replacement.updated_at_unix = 2;
    sign_entry(&mut replacement, &publisher);
    storage.save_entry(&replacement).unwrap();
    let second = Command::new(tilo_test_env::package_binary!("tilo-registry-server"))
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
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(storage.load_snapshot().unwrap().unwrap().sequence, 2);

    let rollback = Command::new(tilo_test_env::package_binary!("tilo-registry-server"))
        .arg("--data-dir")
        .arg(temp.path())
        .arg("rollback-snapshot")
        .arg("--to-sequence")
        .arg("1")
        .arg("--owner-key-file")
        .arg(&key_path)
        .arg("--valid-for-seconds")
        .arg("3600")
        .output()
        .unwrap();
    assert!(
        rollback.status.success(),
        "{}",
        String::from_utf8_lossy(&rollback.stderr)
    );
    let rolled_back = storage.load_snapshot().unwrap().unwrap();
    assert_eq!(rolled_back.sequence, 3);
    assert_eq!(rolled_back.entries[0].name.resolve("en"), Some("Test"));
    assert_eq!(
        storage.load_snapshot_sequence(2).unwrap().unwrap().entries[0]
            .name
            .resolve("en"),
        Some("Changed")
    );

    std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let refused = Command::new(tilo_test_env::package_binary!("tilo-registry-server"))
        .arg("--data-dir")
        .arg(temp.path())
        .arg("sign-snapshot")
        .arg("--owner-key-file")
        .arg(&key_path)
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert_eq!(storage.load_snapshot().unwrap().unwrap().sequence, 3);
}

#[test]
fn signing_and_rollback_read_owner_identity_only_after_acquiring_the_registry_lock() {
    let temp = tempfile::tempdir().unwrap();
    let storage = Storage::open(temp.path()).unwrap();
    let owner_a = SigningKey::from_bytes(&[41; 32]);
    let owner_b = SigningKey::from_bytes(&[42; 32]);
    let key_path = temp.path().join("owner-a.seed");
    write_secret(&key_path, &owner_a);
    storage.save_identity(&identity(&owner_a)).unwrap();

    let transaction = storage.lock_snapshot().unwrap();
    let signer = Command::new(tilo_test_env::package_binary!("tilo-registry-server"))
        .arg("--data-dir")
        .arg(temp.path())
        .arg("sign-snapshot")
        .arg("--owner-key-file")
        .arg(&key_path)
        .arg("--valid-for-seconds")
        .arg("3600")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for_registry_lock_waiter(signer.id());
    storage.save_identity(&identity(&owner_b)).unwrap();
    drop(transaction);
    let signed = signer.wait_with_output().unwrap();
    assert!(!signed.status.success());
    assert!(
        String::from_utf8_lossy(&signed.stderr)
            .contains("owner private key does not match its authorized identity")
    );
    assert!(storage.load_snapshot().unwrap().is_none());

    storage.save_identity(&identity(&owner_a)).unwrap();
    for _ in 0..2 {
        let signed = Command::new(tilo_test_env::package_binary!("tilo-registry-server"))
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
            signed.status.success(),
            "{}",
            String::from_utf8_lossy(&signed.stderr)
        );
    }
    assert_eq!(storage.load_snapshot().unwrap().unwrap().sequence, 2);

    let transaction = storage.lock_snapshot().unwrap();
    let rollback = Command::new(tilo_test_env::package_binary!("tilo-registry-server"))
        .arg("--data-dir")
        .arg(temp.path())
        .arg("rollback-snapshot")
        .arg("--to-sequence")
        .arg("1")
        .arg("--owner-key-file")
        .arg(&key_path)
        .arg("--valid-for-seconds")
        .arg("3600")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for_registry_lock_waiter(rollback.id());
    storage.save_identity(&identity(&owner_b)).unwrap();
    drop(transaction);
    let rolled_back = rollback.wait_with_output().unwrap();
    assert!(!rolled_back.status.success());
    assert!(
        String::from_utf8_lossy(&rolled_back.stderr)
            .contains("owner private key does not match its authorized identity")
    );
    assert_eq!(storage.load_snapshot().unwrap().unwrap().sequence, 2);
}
