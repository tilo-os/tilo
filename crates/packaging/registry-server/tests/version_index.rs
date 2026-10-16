//! Compact snapshots and version indexes end to end: snapshots signed by the
//! real CLI, served over HTTP from real storage, read by clients that ask for
//! the compact form and by released clients that do not.

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
use registry_server::{AppState, PublisherAllowlist, PublisherRow, ServerIdentity, Storage};
use registry_types::{
    CompactRegistrySnapshot, RegistryEntry, RegistryTarget, RegistryVersion,
    SignedRegistrySnapshot, sign_entry,
};

const NOTES: &str = "so.tilo.notes";

fn version(number: &str) -> RegistryVersion {
    RegistryVersion {
        version: number.into(),
        released_at_unix: 1,
        artifact: registry_types::VersionArtifact::Tapp(registry_types::TappArtifact {
            tapp_url: format!("https://apps.tilo.so/artifacts/notes-{number}.tapp"),
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
    }
}

fn entry(package_id: &str, versions: &[&str], updated_at: u64) -> RegistryEntry {
    let publisher = SigningKey::from_bytes(&[52; 32]);
    let mut entry = RegistryEntry {
        package_id: package_id.into(),
        kind: registry_types::EntryKind::App,
        name: "Notes".into(),
        description: "A test app".into(),
        tagline: "Test on Tilo OS".into(),
        images: registry_types::LocalizedImages::english_icon(
            "https://apps.tilo.so/images/test-icon.svg",
            "Test icon",
        ),
        supported_languages: vec!["en".into()],
        categories: vec!["Test".into()],
        source_url: None,
        homepage: None,
        publisher_pubkey_hex: hex::encode(publisher.verifying_key().to_bytes()),
        versions: versions.iter().map(|number| version(number)).collect(),
        updated_at_unix: updated_at,
        signature_hex: String::new(),
    };
    sign_entry(&mut entry, &publisher);
    entry
}

struct Registry {
    dir: tempfile::TempDir,
    storage: Storage,
    owner_hex: String,
    key_path: PathBuf,
    _keys: tempfile::TempDir,
}

impl Registry {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let keys = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        let owner = SigningKey::from_bytes(&[51; 32]);
        let owner_hex = hex::encode(owner.verifying_key().to_bytes());
        let publisher = SigningKey::from_bytes(&[52; 32]);
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
                    pubkey_hex: hex::encode(publisher.verifying_key().to_bytes()),
                    can_add_new: true,
                    note: String::new(),
                    namespaces: vec![registry_types::namespace::TILO_NAMESPACE.to_owned()],
                }],
            })
            .unwrap();
        let key_path = keys.path().join("owner.seed");
        std::fs::write(&key_path, hex::encode(owner.to_bytes())).unwrap();
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        Self {
            dir,
            storage,
            owner_hex,
            key_path,
            _keys: keys,
        }
    }

    fn sign(&self) -> (SignedRegistrySnapshot, CompactRegistrySnapshot) {
        let output = Command::new(env!("CARGO_BIN_EXE_tilo-registry-server"))
            .arg("--data-dir")
            .arg(self.dir.path())
            .args(["sign-snapshot", "--owner-key-file"])
            .arg(&self.key_path)
            .args(["--valid-for-seconds", "86400"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        (
            self.storage.load_snapshot().unwrap().unwrap(),
            self.storage.load_compact_snapshot().unwrap().unwrap(),
        )
    }

    /// What an attacker controlling the server's disk can serve.
    fn serve_raw(&self, full: &SignedRegistrySnapshot, compact: &CompactRegistrySnapshot) {
        std::fs::write(
            self.dir.path().join("published-snapshot.json"),
            serde_json::to_vec(full).unwrap(),
        )
        .unwrap();
        std::fs::write(
            self.dir.path().join("published-compact-snapshot.json"),
            serde_json::to_vec(compact).unwrap(),
        )
        .unwrap();
    }
}

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

fn get(url: &str, if_none_match: Option<&str>) -> reqwest::blocking::Response {
    let mut request = tilo_net::http::blocking_client().build().unwrap().get(url);
    if let Some(etag) = if_none_match {
        request = request.header("if-none-match", etag);
    }
    request.send().unwrap()
}

fn etag(response: &reqwest::blocking::Response) -> String {
    response
        .headers()
        .get("etag")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned()
}

fn native<T>(
    config: &RegistryConfig,
    config_dir: &Path,
    call: impl FnOnce(&RegistryConfig) -> T,
) -> T {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_STORE_CONFIG_DIR", config_dir);
    call(config)
}

#[test]
fn released_clients_keep_the_full_snapshot_while_new_clients_read_compact_and_indexes() {
    let registry = Registry::new();
    registry
        .storage
        .save_entry(&entry(NOTES, &["1.0.0"], 1))
        .unwrap();
    registry
        .storage
        .save_entry(&entry("so.tilo.sheets", &["1"], 1))
        .unwrap();
    let (old_full, old_compact) = registry.sign();
    let versions: Vec<String> = (0..40).rev().map(|minor| format!("1.{minor}.0")).collect();
    let versions: Vec<&str> = versions.iter().map(String::as_str).collect();
    registry
        .storage
        .save_entry(&entry(NOTES, &versions, 2))
        .unwrap();
    let (full, compact) = registry.sign();
    assert_eq!(compact.sequence, 2);
    let addr = serve(&registry);
    let base_url = format!("http://{addr}");

    // A released client asks for /entries and gets exactly what it always
    // got: the full signed snapshot, every version included.
    let response = get(&format!("{base_url}/entries"), None);
    assert!(response.status().is_success());
    let full_etag = etag(&response);
    let full_bytes = response.bytes().unwrap();
    let served: SignedRegistrySnapshot = serde_json::from_slice(&full_bytes).unwrap();
    assert_eq!(served, full);
    assert_eq!(served.protocol, registry_types::SNAPSHOT_PROTOCOL);
    registry_types::verify_snapshot(&served, &registry.owner_hex, tilo_clock::unix_now()).unwrap();
    assert_eq!(served.entries[0].versions.len(), 40);
    assert_eq!(
        full_etag,
        format!("\"{}\"", registry_types::snapshot_hash(&full).unwrap())
    );
    let entry_response = get(&format!("{base_url}/entries/{NOTES}"), None);
    let served: SignedRegistrySnapshot = entry_response.json().unwrap();
    assert_eq!(served, full);

    // A new client asks for the compact form.
    let response = get(&format!("{base_url}/entries?format=compact"), None);
    let compact_etag = etag(&response);
    let compact_bytes = response.bytes().unwrap();
    let served: CompactRegistrySnapshot = serde_json::from_slice(&compact_bytes).unwrap();
    assert_eq!(served, compact);
    assert_eq!(served.protocol, registry_types::COMPACT_SNAPSHOT_PROTOCOL);
    assert_eq!(served.entries[0].entry.versions, vec![version("1.39.0")]);
    assert!(
        compact_bytes.len() * 4 < full_bytes.len(),
        "compact {} bytes, full {}",
        compact_bytes.len(),
        full_bytes.len()
    );
    assert_ne!(compact_etag, full_etag);
    let revalidated = get(
        &format!("{base_url}/entries?format=compact"),
        Some(&compact_etag),
    );
    assert_eq!(revalidated.status(), reqwest::StatusCode::NOT_MODIFIED);

    // The index is static, immutable and is the complete signed entry.
    let index = &compact.entries[0].version_index;
    let response = get(
        &format!("{base_url}/indexes/{}.json", index.blake3_hex),
        None,
    );
    assert!(response.status().is_success());
    assert!(
        response.headers()["cache-control"]
            .to_str()
            .unwrap()
            .contains("immutable")
    );
    let index_bytes = response.bytes().unwrap();
    assert_eq!(
        registry_types::verify_version_index(&compact.entries[0], &index_bytes).unwrap(),
        full.entries[0]
    );

    // The native client reads the compact form, and the index for every
    // version.
    let config = RegistryConfig {
        name: "test".into(),
        base_url: base_url.clone(),
        owner_pubkey_hex: registry.owner_hex.clone(),
        enabled: true,
        channel: tilo_app_package::version::ReleaseChannel::default(),
    };
    let native_dir = tempfile::tempdir().unwrap();
    let all = native(&config, native_dir.path(), |config| {
        app_cli::client::fetch_all_versions(config, NOTES)
    })
    .unwrap();
    assert_eq!(all.entry, full.entries[0]);
    let listed = native(&config, native_dir.path(), app_cli::client::fetch_entries).unwrap();
    assert_eq!(listed[0].entry.versions.len(), 1);
    assert_eq!(listed[0].version_index.as_ref(), Some(index));

    // A tampered index on disk is never served.
    let index_path = registry
        .dir
        .path()
        .join("version-indexes")
        .join(format!("{}.json", index.blake3_hex));
    let mut tampered = index_bytes.to_vec();
    tampered.truncate(tampered.len() - 1);
    std::fs::write(&index_path, &tampered).unwrap();
    let response = get(
        &format!("{base_url}/indexes/{}.json", index.blake3_hex),
        None,
    );
    assert!(!response.status().is_success());
    std::fs::write(&index_path, &index_bytes).unwrap();

    // Replaying the older snapshot, whose compact form names the older
    // index, is refused by the checkpoint.
    registry.serve_raw(&old_full, &old_compact);
    let replayed = get(&format!("{base_url}/entries?format=compact"), None)
        .json::<CompactRegistrySnapshot>()
        .unwrap();
    assert_eq!(replayed, old_compact);
    assert!(matches!(
        native(&config, native_dir.path(), |config| {
            app_cli::client::fetch_all_versions(config, NOTES)
        }),
        Err(RegistryError::Rollback)
    ));

    // Without a compact form of the current snapshot, compact requests get
    // the full one: a stale compact pointer is not served.
    registry.serve_raw(&full, &old_compact);
    let fallback: SignedRegistrySnapshot = get(&format!("{base_url}/entries?format=compact"), None)
        .json()
        .unwrap();
    assert_eq!(fallback, full);
    std::fs::remove_file(registry.dir.path().join("published-compact-snapshot.json")).unwrap();
    let fallback: SignedRegistrySnapshot = get(&format!("{base_url}/entries?format=compact"), None)
        .json()
        .unwrap();
    assert_eq!(fallback, full);
    let all = native(&config, native_dir.path(), |config| {
        app_cli::client::fetch_all_versions(config, NOTES)
    })
    .unwrap();
    assert_eq!(all.entry, full.entries[0]);

    // The next signing publishes the compact form again and keeps only the
    // indexes it and its predecessor name.
    let (_, latest) = registry.sign();
    assert_eq!(latest.sequence, 3);
    let kept = std::fs::read_dir(registry.dir.path().join("version-indexes"))
        .unwrap()
        .count();
    assert_eq!(kept, 2, "the current indexes, unchanged since sequence 2");
}
