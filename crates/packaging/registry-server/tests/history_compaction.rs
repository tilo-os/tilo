//! Snapshot history compaction end to end: a real server over HTTP on real
//! storage, snapshots signed by the real CLI, and the native client verifying
//! what it serves.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    reason = "integration-test setup and assertions"
)]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use app_cli::client::RegistryError;
use app_cli::config::RegistryConfig;
use ed25519_dalek::SigningKey;
use registry_server::storage::SNAPSHOT_HISTORY_RETAINED;
use registry_server::{AppState, PublisherAllowlist, PublisherRow, ServerIdentity, Storage};
use registry_types::{
    RegistryEntry, RegistryTarget, RegistryVersion, SignedRegistrySnapshot, sign_entry,
};

struct Registry {
    dir: tempfile::TempDir,
    storage: Storage,
    owner: SigningKey,
    owner_hex: String,
    key_path: PathBuf,
    _keys: tempfile::TempDir,
}

fn registry(entries: usize) -> Registry {
    let dir = tempfile::tempdir().unwrap();
    let keys = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).unwrap();
    let owner = SigningKey::from_bytes(&[41; 32]);
    let publisher = SigningKey::from_bytes(&[42; 32]);
    let owner_hex = hex::encode(owner.verifying_key().to_bytes());
    let publisher_hex = hex::encode(publisher.verifying_key().to_bytes());
    storage
        .save_identity(&ServerIdentity {
            owner_pubkey_hex: owner_hex.clone(),
            name: "test".into(),
            description: "test".into(),
            base_url: "http://127.0.0.1".into(),
        })
        .unwrap();
    storage
        .save_allowlist(&PublisherAllowlist {
            publishers: vec![PublisherRow {
                pubkey_hex: publisher_hex.clone(),
                can_add_new: true,
                note: String::new(),
                namespaces: vec![registry_types::namespace::TILO_NAMESPACE.to_owned()],
            }],
        })
        .unwrap();
    for index in 0..entries {
        let mut entry = RegistryEntry {
            package_id: format!("so.tilo.test{index}"),
            kind: registry_types::EntryKind::App,
            name: "Test".into(),
            description: "A test app with a description long enough to weigh something".into(),
            tagline: "Test on Tilo OS".into(),
            images: registry_types::LocalizedImages::english_icon(
                "https://apps.tilo.so/images/test-icon.svg",
                "Test icon",
            ),
            supported_languages: vec!["en".into()],
            categories: vec!["Test".into()],
            source_url: None,
            homepage: None,
            publisher_pubkey_hex: publisher_hex.clone(),
            versions: vec![RegistryVersion {
                version: "1".into(),
                released_at_unix: 1,
                artifact: registry_types::VersionArtifact::Tapp(registry_types::TappArtifact {
                    tapp_url: format!("https://apps.tilo.so/artifacts/test{index}.tapp"),
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
    }
    let key_path = keys.path().join("owner.seed");
    std::fs::write(&key_path, hex::encode(owner.to_bytes())).unwrap();
    std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    Registry {
        dir,
        storage,
        owner,
        owner_hex,
        key_path,
        _keys: keys,
    }
}

impl Registry {
    fn run(&self, args: &[&str]) -> std::process::Output {
        Command::new(tilo_test_env::package_binary!("tilo-registry-server"))
            .arg("--data-dir")
            .arg(self.dir.path())
            .args(args)
            .arg("--owner-key-file")
            .arg(&self.key_path)
            .args(["--valid-for-seconds", "86400"])
            .output()
            .unwrap()
    }

    fn sign(&self) -> SignedRegistrySnapshot {
        let output = self.run(&["sign-snapshot"]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        self.storage.load_snapshot().unwrap().unwrap()
    }

    fn history(&self) -> Vec<(u64, u64)> {
        let mut members: Vec<(u64, u64)> = std::fs::read_dir(self.dir.path().join("snapshots"))
            .unwrap()
            .filter_map(|item| {
                let item = item.unwrap();
                let name = item.file_name().into_string().unwrap();
                let sequence = name.split_once('-')?.0.parse().ok()?;
                Some((sequence, item.metadata().unwrap().len()))
            })
            .collect();
        members.sort_unstable();
        members
    }

    fn history_bytes(&self) -> u64 {
        std::fs::read_dir(self.dir.path().join("snapshots"))
            .unwrap()
            .map(|item| item.unwrap().metadata().unwrap().len())
            .sum()
    }

    /// What an attacker controlling the server's disk can serve.
    fn serve_raw(&self, snapshot: &SignedRegistrySnapshot) {
        std::fs::write(
            self.dir.path().join("published-snapshot.json"),
            serde_json::to_vec(snapshot).unwrap(),
        )
        .unwrap();
    }
}

/// Run the HTTP server on its own runtime for the rest of the test process.
fn serve(registry: &Registry) -> SocketAddr {
    let storage = registry.storage.clone();
    let identity = storage.load_identity().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async move {
                let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
                let bound = registry_server::bind_and_run(addr, AppState::new(storage, identity))
                    .await
                    .unwrap();
                sender.send(bound).unwrap();
                std::future::pending::<()>().await;
            });
    });
    receiver.recv().unwrap()
}

fn native_sync(config: &RegistryConfig, config_dir: &Path) -> Result<u64, RegistryError> {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_STORE_CONFIG_DIR", config_dir);
    app_cli::client::fetch_snapshot(config).map(|snapshot| snapshot.sequence())
}

#[test]
fn a_client_synced_before_compaction_verifies_the_latest_and_still_rejects_rollbacks_and_forks() {
    let registry = registry(2);
    let addr = serve(&registry);
    let config = RegistryConfig {
        name: "test".into(),
        base_url: format!("http://{addr}"),
        owner_pubkey_hex: registry.owner_hex.clone(),
        enabled: true,
        channel: tilo_app_package::version::ReleaseChannel::default(),
    };
    let native_dir = tempfile::tempdir().unwrap();

    // The client last syncs at sequence 3, then goes offline.
    let mut signed = Vec::new();
    for _ in 0..3 {
        signed.push(registry.sign());
    }
    assert_eq!(native_sync(&config, native_dir.path()).unwrap(), 3);

    let last = u64::try_from(SNAPSHOT_HISTORY_RETAINED).unwrap() * 2 + 5;
    while signed.len() < usize::try_from(last).unwrap() {
        signed.push(registry.sign());
    }
    let history = registry.history();
    assert_eq!(history.len(), SNAPSHOT_HISTORY_RETAINED);
    let oldest_retained = last - u64::try_from(SNAPSHOT_HISTORY_RETAINED).unwrap() + 1;
    assert_eq!(history[0].0, oldest_retained);
    assert_eq!(history.last().unwrap().0, last);
    let compaction = registry.storage.history_compaction().unwrap().unwrap();
    assert_eq!(compaction.through_sequence, oldest_retained - 1);
    // The record names the member the oldest retained snapshot signed as its
    // predecessor: the signed chain commits to the pruned prefix.
    let through = &signed[usize::try_from(oldest_retained - 2).unwrap()];
    assert_eq!(
        compaction.through_snapshot_hash_hex,
        registry_types::snapshot_hash(through).unwrap()
    );
    assert_eq!(
        registry
            .storage
            .load_snapshot_sequence(oldest_retained)
            .unwrap()
            .unwrap()
            .previous_snapshot_hash_hex,
        compaction.through_snapshot_hash_hex
    );
    assert!(
        registry
            .storage
            .load_snapshot_sequence(3)
            .unwrap()
            .is_none()
    );

    // A rollback to an older signed snapshot is refused before the client
    // ever sees the latest one.
    let latest = registry.storage.load_snapshot().unwrap().unwrap();
    registry.serve_raw(&signed[1]);
    assert!(matches!(
        native_sync(&config, native_dir.path()),
        Err(RegistryError::Rollback)
    ));

    // So is a fork: another validly signed snapshot at the checkpoint.
    let mut fork = signed[2].clone();
    fork.entries.pop();
    registry_types::sign_snapshot(&mut fork, &registry.owner).unwrap();
    registry_types::verify_snapshot(&fork, &registry.owner_hex, tilo_clock::unix_now()).unwrap();
    registry.serve_raw(&fork);
    assert!(matches!(
        native_sync(&config, native_dir.path()),
        Err(RegistryError::Rollback)
    ));

    // The latest state verifies for a client last synced before compaction.
    registry.serve_raw(&latest);
    assert_eq!(native_sync(&config, native_dir.path()).unwrap(), last);

    // Replaying a snapshot from before the compaction point, or forking the
    // newest checkpoint, is still refused afterwards.
    registry.serve_raw(&signed[2]);
    assert!(matches!(
        native_sync(&config, native_dir.path()),
        Err(RegistryError::Rollback)
    ));
    let mut fork = latest.clone();
    fork.entries.pop();
    registry_types::sign_snapshot(&mut fork, &registry.owner).unwrap();
    registry.serve_raw(&fork);
    assert!(matches!(
        native_sync(&config, native_dir.path()),
        Err(RegistryError::Rollback)
    ));
    registry.serve_raw(&latest);

    // The server cannot be led into reusing a pruned sequence either: a lost
    // pointer does not restart the channel at sequence 1.
    std::fs::remove_file(registry.dir.path().join("published-snapshot.json")).unwrap();
    let restart = registry.run(&["sign-snapshot"]);
    assert!(!restart.status.success());
    assert!(String::from_utf8_lossy(&restart.stderr).contains("compacted through sequence"));
    registry.serve_raw(&latest);

    // Operators roll back to a retained snapshot as before, and are told when
    // the one they ask for was compacted.
    let pruned = registry.run(&["rollback-snapshot", "--to-sequence", "3"]);
    assert!(!pruned.status.success());
    assert!(String::from_utf8_lossy(&pruned.stderr).contains("was compacted"));
    let target = oldest_retained.to_string();
    let rolled = registry.run(&["rollback-snapshot", "--to-sequence", &target]);
    assert!(
        rolled.status.success(),
        "{}",
        String::from_utf8_lossy(&rolled.stderr)
    );
    assert_eq!(native_sync(&config, native_dir.path()).unwrap(), last + 1);
    assert_eq!(registry.history().len(), SNAPSHOT_HISTORY_RETAINED);
}

#[test]
fn snapshot_history_size_is_bounded() {
    let registry = registry(64);
    let mut largest = 0_u64;
    let mut steady = None;
    for published in 1..=SNAPSHOT_HISTORY_RETAINED * 3 {
        registry.sign();
        let history = registry.history();
        largest = largest.max(history.iter().map(|(_, len)| *len).max().unwrap());
        assert_eq!(history.len(), published.min(SNAPSHOT_HISTORY_RETAINED));
        let bytes = registry.history_bytes();
        let retained = u64::try_from(SNAPSHOT_HISTORY_RETAINED).unwrap();
        assert!(
            bytes <= retained * largest + 512,
            "{bytes} bytes of history after {published} publishes"
        );
        if published > SNAPSHOT_HISTORY_RETAINED {
            // Same catalogue each time, so the history stops growing.
            let first = *steady.get_or_insert(bytes);
            assert!(bytes <= first + retained * 4, "history grew to {bytes}");
        }
    }
}
