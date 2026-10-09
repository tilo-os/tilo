//! A release into an entry at its version limit, end to end: the publisher
//! client uploads and submits over HTTP, the collector and the networkless
//! promoter run as the deployed binary, and the registry's storage keeps
//! exactly the versions the entry still lists.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::indexing_slicing,
    clippy::unreachable,
    reason = "integration-test setup and assertions"
)]

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::Command;

use ed25519_dalek::SigningKey;
use registry_client::RegistryClient;
use registry_server::storage::SNAPSHOT_HISTORY_RETAINED;
use registry_server::{
    AppState, PublisherAllowlist, PublisherRow, ServerIdentity, Storage, bind_and_run,
};
use registry_types::entry::MAX_ENTRY_VERSIONS;
use registry_types::retention::{DEFAULT_RETAINED_PER_CHANNEL, RetentionPolicy};

const PACKAGE_ID: &str = "so.tilo.agent";
/// Identical in every release, so the registry holds it once.
fn shared_library() -> Vec<u8> {
    vec![7; 64 * 1024]
}

fn run(data_dir: &Path, arguments: &[&str]) -> serde_json::Value {
    let output = Command::new(env!("CARGO_BIN_EXE_tilo-registry-server"))
        .arg("--data-dir")
        .arg(data_dir)
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

/// Collect the waiting request and promote it, as the collector and
/// publisher `CronJobs` do.
fn promote(data_dir: &Path, publisher_hex: &str, channel_key: &Path) -> serde_json::Value {
    let collected = run(
        data_dir,
        &["collect-first-party", "--publisher-pubkey", publisher_hex],
    );
    assert_eq!(collected["status"], "staged", "{collected}");
    let published = run(
        data_dir,
        &[
            "publish-staged-first-party",
            "--channel-key-file",
            channel_key.to_str().unwrap(),
            "--valid-for-seconds",
            "3600",
        ],
    );
    assert_eq!(published["status"], "published", "{published}");
    published
}

fn release(dir: &Path, key: &SigningKey, minor: usize) -> Vec<u8> {
    let identity = tilo_app_package::SigningIdentity::from_signing_key(key.clone());
    let payload = dir.join(format!("release-{minor}"));
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    std::fs::create_dir_all(payload.join("lib")).unwrap();
    std::fs::write(payload.join("bin/agent"), format!("agent build {minor}")).unwrap();
    std::fs::write(payload.join("lib/shared.so"), shared_library()).unwrap();
    let manifest = tilo_app_package::Manifest::from_toml_str(&format!(
        "[package]\nid = \"{PACKAGE_ID}\"\nversion = \"1.{minor}.0\"\nkind = \"app\"\n\
         [package.isa]\nx86_64 = \"v3\"\n\
         [publisher]\nidentity = \"ed25519:{}\"\nsignature = \"\"\n\
         [sandbox]\nnamespaces = [\"user\"]\nseccomp = \"tilo-default\"\nlandlock = \"strict\"\n\
         [sandbox.net]\nmode = \"off\"\n\
         [entrypoint]\nbin = \"bin/agent\"\ntype = \"gui\"\n",
        hex::encode(identity.public_key_bytes())
    ))
    .unwrap();
    let packed = tilo_app_package::pack(&payload, &manifest, &identity).unwrap();
    tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed)).unwrap()
}

fn root_of(bytes: &[u8]) -> String {
    tilo_app_package::tapp::read(bytes, &mut tilo_app_package::DiscardBlobs)
        .unwrap()
        .meta
        .id
        .to_hex()
}

fn names(dir: &Path) -> BTreeSet<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect()
}

fn status(url: &str) -> reqwest::StatusCode {
    tilo_net::http::blocking_client()
        .build()
        .unwrap()
        .get(url)
        .send()
        .unwrap()
        .status()
}

#[tokio::test]
async fn a_release_into_a_full_entry_replaces_its_oldest_version_in_storage() {
    let temp = tempfile::tempdir().unwrap();
    let data_dir = temp.path().join("registry");
    let storage = Storage::open(&data_dir).unwrap();
    let owner = SigningKey::from_bytes(&[61; 32]);
    let channel = SigningKey::from_bytes(&[62; 32]);
    let publisher = SigningKey::from_bytes(&[63; 32]);
    let publisher_hex = hex::encode(publisher.verifying_key().to_bytes());
    let identity = ServerIdentity {
        owner_pubkey_hex: hex::encode(owner.verifying_key().to_bytes()),
        name: "test".into(),
        description: "test".into(),
        base_url: "https://apps.tilo.so".into(),
    };
    storage.save_identity(&identity).unwrap();
    storage
        .save_allowlist(&PublisherAllowlist {
            publishers: vec![PublisherRow {
                pubkey_hex: publisher_hex.clone(),
                can_add_new: true,
                note: "first party".into(),
                namespaces: vec![registry_types::namespace::TILO_NAMESPACE.to_owned()],
            }],
        })
        .unwrap();
    let owner_key = temp.path().join("owner.seed");
    let channel_key = temp.path().join("channel.seed");
    for (path, key) in [(&owner_key, &owner), (&channel_key, &channel)] {
        std::fs::write(path, hex::encode(key.to_bytes())).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    run(
        &data_dir,
        &[
            "authorize-channel",
            "--owner-key-file",
            owner_key.to_str().unwrap(),
            "--channel-pubkey",
            &hex::encode(channel.verifying_key().to_bytes()),
            "--publisher-pubkey",
            &publisher_hex,
            "--generation",
            "1",
        ],
    );
    let addr = bind_and_run(
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
        AppState::new(storage.clone(), identity),
    )
    .await
    .unwrap();

    tokio::task::spawn_blocking(move || {
        let base = format!("http://{addr}");
        let client = RegistryClient::new(&base).unwrap();
        let releases: Vec<Vec<u8>> = (1..=MAX_ENTRY_VERSIONS + 2)
            .map(|minor| release(temp.path(), &publisher, minor))
            .collect();
        let (full, [next, trimmed]) = releases.split_at(MAX_ENTRY_VERSIONS) else {
            unreachable!("two releases past the limit");
        };
        let keep_all = RetentionPolicy {
            releases: MAX_ENTRY_VERSIONS,
            prereleases: MAX_ENTRY_VERSIONS,
        };

        // The entry fills up to its limit.
        registry_client::publish(&client, &publisher, full, None, None, &keep_all).unwrap();
        promote(&data_dir, &publisher_hex, &channel_key);
        let entry = client.entry(PACKAGE_ID).unwrap().unwrap();
        assert_eq!(entry.versions.len(), MAX_ENTRY_VERSIONS);

        // Release 129: the client drops the oldest version and the server
        // accepts that request, collects it and promotes it.
        let submitted = registry_client::publish(
            &client,
            &publisher,
            std::slice::from_ref(next),
            None,
            None,
            &keep_all,
        )
        .unwrap();
        assert_eq!(
            submitted.first().and_then(|build| build.action),
            Some(registry_types::EntryAction::Update)
        );
        let promoted = promote(&data_dir, &publisher_hex, &channel_key);
        let entry = client.entry(PACKAGE_ID).unwrap().unwrap();
        let versions: BTreeSet<&str> = entry
            .versions
            .iter()
            .map(|version| version.version.as_str())
            .collect();
        assert_eq!(versions.len(), MAX_ENTRY_VERSIONS);
        assert!(versions.contains(format!("1.{}.0", MAX_ENTRY_VERSIONS + 1).as_str()));
        assert!(!versions.contains("1.1.0"));

        // The retained history and the previous version index still list the
        // dropped version, so its bytes stay until both have moved on.
        assert_eq!(promoted["pruned"]["payload_blobs"], 0, "{promoted}");
        let oldest = releases.first().unwrap();
        let oldest_digest = blake3::hash(oldest).to_hex().to_string();
        let oldest_binary = tilo_app_package::BlobHash::of(b"agent build 1").to_hex();
        assert!(
            storage
                .find_staged_artifact(&oldest_digest)
                .unwrap()
                .is_some()
        );
        assert_eq!(
            status(&format!("{base}/blobs/{oldest_binary}")),
            reqwest::StatusCode::OK
        );
        for _ in 0..SNAPSHOT_HISTORY_RETAINED {
            let output = Command::new(env!("CARGO_BIN_EXE_tilo-registry-server"))
                .arg("--data-dir")
                .arg(&data_dir)
                .args(["sign-delegated-snapshot", "--channel-key-file"])
                .arg(&channel_key)
                .args(["--valid-for-seconds", "3600"])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        // Then the dropped version leaves storage: its staged package, its
        // thin form and the blob only it used.
        let pruned = run(&data_dir, &["prune-storage"]);
        assert_eq!(pruned["pruned"]["staged_artifacts"], 1, "{pruned}");
        assert_eq!(pruned["pruned"]["thin_packages"], 1, "{pruned}");
        assert_eq!(pruned["pruned"]["payload_blobs"], 1, "{pruned}");
        assert_eq!(storage.find_staged_artifact(&oldest_digest).unwrap(), None);
        assert_eq!(
            status(&format!("{base}/thin/{}.tapp", root_of(oldest))),
            reqwest::StatusCode::NOT_FOUND
        );
        assert_eq!(
            status(&format!("{base}/blobs/{oldest_binary}")),
            reqwest::StatusCode::NOT_FOUND
        );

        // Every listed version stays collectable byte for byte and servable
        // thin, while storage keeps each package once: its root, its thin
        // form, and blobs shared across releases.
        for version in &entry.versions {
            let bytes = storage
                .load_staged_artifact(&version.tapp_blake3_hex)
                .unwrap();
            assert_eq!(
                blake3::hash(&bytes).to_hex().as_str(),
                version.tapp_blake3_hex
            );
            assert_eq!(
                status(&format!("{base}/thin/{}.tapp", version.tapp_root_hash_hex)),
                reqwest::StatusCode::OK
            );
        }
        let shared = tilo_app_package::BlobHash::of(&shared_library()).to_hex();
        assert_eq!(
            status(&format!("{base}/blobs/{shared}")),
            reqwest::StatusCode::OK
        );
        let staged = names(&data_dir.join("staged-artifacts"));
        assert_eq!(
            staged,
            entry
                .versions
                .iter()
                .map(|version| format!("{}.root", version.tapp_blake3_hex))
                .collect()
        );
        let stored = names(&data_dir.join("payload-blobs"));
        let (twins, blobs): (BTreeSet<String>, BTreeSet<String>) =
            stored.into_iter().partition(|name| name.ends_with(".zst"));
        assert_eq!(
            blobs.len(),
            MAX_ENTRY_VERSIONS + 1,
            "one binary per listed release plus the shared library"
        );
        assert_eq!(
            twins,
            blobs.iter().map(|blob| format!("{blob}.zst")).collect(),
            "every stored blob has its compressed twin"
        );
        let staged_bytes: u64 = std::fs::read_dir(data_dir.join("staged-artifacts"))
            .unwrap()
            .map(|entry| entry.unwrap().metadata().unwrap().len())
            .sum();
        assert!(staged_bytes < next.len() as u64, "{staged_bytes} bytes");

        // Under the default retention the next release keeps only the newest
        // versions, and the server accepts and promotes the shorter entry.
        registry_client::publish(
            &client,
            &publisher,
            std::slice::from_ref(trimmed),
            None,
            None,
            &RetentionPolicy::default(),
        )
        .unwrap();
        promote(&data_dir, &publisher_hex, &channel_key);
        let entry = client.entry(PACKAGE_ID).unwrap().unwrap();
        assert_eq!(entry.versions.len(), DEFAULT_RETAINED_PER_CHANNEL);
        let newest = format!("1.{}.0", MAX_ENTRY_VERSIONS + 2);
        assert!(
            entry
                .versions
                .iter()
                .any(|version| version.version == newest)
        );
    })
    .await
    .unwrap();
}
