//! Older versions through a compact snapshot's version index: the catalogue
//! lists only the newest version, `app install --version` and
//! `app rollback` fetch the signed index for the rest, and a tampered index or
//! a replayed older snapshot is refused.

#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::sync::{Arc, Mutex};

use app_cli::config::{RegistriesFile, RegistryConfig};
use ed25519_dalek::SigningKey;
use registry_types::{
    CompactRegistrySnapshot, PublisherAuthorization, RegistryEntry, RegistryVersion,
    SignedRegistrySnapshot, VersionIndexFile, sign_entry,
};

use crate::registry_to_installed::{
    author_tapp_version, native_target, scoped_env, test_publisher, tilo_install_bin,
};

const PACKAGE_ID: &str = "so.tilo.version-index-e2e";
const OTHER_ID: &str = "so.tilo.version-index-other";

/// Serves fixed bodies by exact request target (path and query), and logs
/// every request target.
#[derive(Clone, Default)]
struct Routes {
    bodies: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    log: Arc<Mutex<Vec<String>>>,
}

impl Routes {
    fn set(&self, target: &str, body: Vec<u8>) {
        self.bodies.lock().unwrap().insert(target.to_owned(), body);
    }

    fn requests(&self) -> Vec<String> {
        std::mem::take(&mut *self.log.lock().unwrap())
    }

    fn start(&self) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let routes = self.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { break };
                routes.serve(stream);
            }
        });
        port
    }

    fn serve(&self, mut stream: TcpStream) {
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request_line = String::new();
        if reader.read_line(&mut request_line).is_err() {
            return;
        }
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) if line.trim().is_empty() => break,
                Ok(_) => {}
                Err(_) => return,
            }
        }
        let target = request_line
            .split_whitespace()
            .nth(1)
            .unwrap_or("/")
            .to_owned();
        self.log.lock().unwrap().push(target.clone());
        let body = self.bodies.lock().unwrap().get(&target).cloned();
        let (status, body) = match body {
            Some(body) => ("200 OK", body),
            None => ("404 Not Found", b"not found".to_vec()),
        };
        let header = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(header.as_bytes());
        let _ = stream.write_all(&body);
        let _ = stream.flush();
    }
}

fn version(number: &str, url: &str, tapp: &[u8]) -> RegistryVersion {
    RegistryVersion {
        version: number.into(),
        released_at_unix: 1,
        artifact: registry_types::VersionArtifact::Tapp(registry_types::TappArtifact {
            tapp_url: url.into(),
            tapp_root_hash_hex: app_registry::tapp::load(tapp).unwrap().package_id.to_hex(),
            tapp_size_bytes: tapp.len() as u64,
            tapp_blake3_hex: blake3::hash(tapp).to_hex().to_string(),
            target: registry_types::RegistryTarget {
                platform: registry_types::TILO_OS_PLATFORM.into(),
                architecture: native_target().0.into(),
                isa: native_target().2.into(),
                runtime: None,
                interfaces: Vec::new(),
            },
        }),
    }
}

fn entry(package_id: &str, versions: Vec<RegistryVersion>, updated_at: u64) -> RegistryEntry {
    let key = SigningKey::from_bytes(&[11u8; 32]);
    let mut entry = RegistryEntry {
        package_id: package_id.into(),
        kind: registry_types::EntryKind::App,
        name: "Sheets".into(),
        description: "A spreadsheet".into(),
        tagline: "Native Tilo OS app".into(),
        images: registry_types::LocalizedImages::english_icon(
            "https://apps.tilo.so/images/test-icon.svg",
            "Test icon",
        ),
        supported_languages: vec!["en".into()],
        categories: vec!["Office".into()],
        source_url: None,
        homepage: None,
        publisher_pubkey_hex: hex::encode(key.verifying_key().to_bytes()),
        versions,
        updated_at_unix: updated_at,
        signature_hex: String::new(),
    };
    sign_entry(&mut entry, &key);
    entry
}

/// Both signed forms of one snapshot and the indexes the compact one names.
struct Published {
    full: SignedRegistrySnapshot,
    compact: CompactRegistrySnapshot,
    indexes: Vec<VersionIndexFile>,
}

fn publish(
    entries: Vec<RegistryEntry>,
    owner: &SigningKey,
    previous: Option<&SignedRegistrySnapshot>,
) -> Published {
    let now = tilo_clock::unix_now();
    let mut full = SignedRegistrySnapshot {
        protocol: registry_types::PACKAGE_FORMAT_VERSION,
        sequence: previous.map_or(1, |previous| previous.sequence.saturating_add(1)),
        issued_at_unix: now,
        expires_at_unix: now.saturating_add(3600),
        previous_snapshot_hash_hex: previous
            .map(|previous| registry_types::snapshot_hash(previous).unwrap())
            .unwrap_or_default(),
        moderation_head_hex: String::new(),
        publishers: vec![PublisherAuthorization {
            pubkey_hex: entries[0].publisher_pubkey_hex.clone(),
            can_add_new: true,
        }],
        entries,
        owner_signature_hex: String::new(),
        channel_authorization: None,
        bots_signature_hex: String::new(),
    };
    registry_types::sign_snapshot(&mut full, owner).unwrap();
    let (mut compact, indexes) = registry_types::compact_snapshot(&full).unwrap();
    registry_types::sign_compact_snapshot(&mut compact, owner).unwrap();
    Published {
        full,
        compact,
        indexes,
    }
}

fn serve(routes: &Routes, published: &Published) {
    routes.set("/entries", serde_json::to_vec(&published.full).unwrap());
    routes.set(
        "/entries?format=compact",
        serde_json::to_vec(&published.compact).unwrap(),
    );
    for index in &published.indexes {
        routes.set(
            &format!("/indexes/{}.json", index.blake3_hex),
            index.bytes.clone(),
        );
    }
}

fn app(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_app"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn older_versions_install_and_roll_back_through_a_verified_version_index() {
    let publisher = test_publisher();
    let owner = SigningKey::from_bytes(&[12; 32]);
    let routes = Routes::default();
    let port = routes.start();
    let base_url = format!("http://127.0.0.1:{port}");

    let mut tapps = Vec::new();
    for number in ["1.0.0", "1.1.0", "2.0.0"] {
        let tapp = author_tapp_version(PACKAGE_ID, number, &publisher);
        let path = format!("/{number}.tapp");
        routes.set(&path, tapp.clone());
        tapps.push(version(number, &format!("{base_url}{path}"), &tapp));
    }
    let other_tapp = author_tapp_version(OTHER_ID, "3.0.0", &publisher);
    routes.set("/other.tapp", other_tapp.clone());
    let other = entry(
        OTHER_ID,
        vec![version(
            "3.0.0",
            &format!("{base_url}/other.tapp"),
            &other_tapp,
        )],
        1,
    );
    // Versions are listed newest first.
    let old = publish(
        vec![
            entry(PACKAGE_ID, vec![tapps[1].clone(), tapps[0].clone()], 2),
            other.clone(),
        ],
        &owner,
        None,
    );
    let current = publish(
        vec![
            entry(
                PACKAGE_ID,
                vec![tapps[2].clone(), tapps[1].clone(), tapps[0].clone()],
                3,
            ),
            other,
        ],
        &owner,
        Some(&old.full),
    );
    assert_eq!(current.compact.entries[0].entry.versions.len(), 1);
    serve(&routes, &current);

    let config_dir = tempfile::tempdir().unwrap();
    let cache_dir = tempfile::tempdir().unwrap();
    let apps_dir = tempfile::tempdir().unwrap();
    std::fs::write(
        config_dir.path().join("registries.toml"),
        toml::to_string_pretty(&RegistriesFile {
            registries: vec![RegistryConfig {
                name: "local".into(),
                base_url,
                owner_pubkey_hex: hex::encode(owner.verifying_key().to_bytes()),
                enabled: true,
                channel: tilo_app_package::version::ReleaseChannel::Stable,
            }],
        })
        .unwrap(),
    )
    .unwrap();
    let install_bin = tilo_install_bin();
    let _env = scoped_env(&[
        ("TILO_STORE_CONFIG_DIR", config_dir.path()),
        ("TILO_STORE_CACHE_DIR", cache_dir.path()),
        ("TILO_APPS_DATA_DIR", apps_dir.path()),
        ("TILO_INSTALL_BIN", &install_bin),
    ]);

    // The catalogue comes from the compact snapshot alone.
    let listing = app_cli::available();
    assert!(listing.unreachable.is_empty(), "{:?}", listing.unreachable);
    let newest = listing
        .apps
        .iter()
        .find(|app| app.package_id == PACKAGE_ID)
        .unwrap();
    assert_eq!(newest.version, "2.0.0");
    assert_eq!(routes.requests(), ["/entries?format=compact"]);

    // Every older version is reachable through the signed index.
    let offers = app_cli::version_offers(PACKAGE_ID, "local").unwrap();
    let versions: Vec<_> = offers.iter().map(|offer| offer.version.as_str()).collect();
    assert_eq!(versions, ["2.0.0", "1.1.0", "1.0.0"]);
    let index_path = format!(
        "/indexes/{}.json",
        current.compact.entries[0].version_index.blake3_hex
    );
    assert_eq!(
        routes.requests(),
        ["/entries?format=compact".to_owned(), index_path.clone()]
    );

    // Install an older version, update, then roll back to it.
    let installed = app(&["install", PACKAGE_ID, "--version", "1.0.0"]);
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    assert_eq!(
        app_cli::installed_version(PACKAGE_ID).unwrap().as_deref(),
        Some("1.0.0")
    );
    let updated = app(&["update", PACKAGE_ID]);
    assert!(
        updated.status.success(),
        "{}",
        String::from_utf8_lossy(&updated.stderr)
    );
    assert_eq!(
        app_cli::installed_version(PACKAGE_ID).unwrap().as_deref(),
        Some("2.0.0")
    );
    let rolled = app(&["rollback", PACKAGE_ID]);
    assert!(
        rolled.status.success(),
        "{}",
        String::from_utf8_lossy(&rolled.stderr)
    );
    assert_eq!(
        app_cli::installed_version(PACKAGE_ID).unwrap().as_deref(),
        Some("1.1.0"),
        "a rollback without --to goes to the previous release"
    );
    let rolled = app(&["rollback", PACKAGE_ID, "--to", "1.0.0"]);
    assert!(
        rolled.status.success(),
        "{}",
        String::from_utf8_lossy(&rolled.stderr)
    );
    assert_eq!(
        app_cli::installed_version(PACKAGE_ID).unwrap().as_deref(),
        Some("1.0.0")
    );
    let forward = app(&["rollback", PACKAGE_ID, "--to", "2.0.0"]);
    assert!(!forward.status.success(), "a rollback never goes forward");

    // A tampered index is refused: other bytes, another entry's index, and a
    // truncated or reordered copy of the right one.
    let good = current.indexes[0].bytes.clone();
    let mut flipped = good.clone();
    let middle = flipped.len() / 2;
    flipped[middle] ^= 1;
    let mut reordered = current.full.entries[0].clone();
    reordered.versions.reverse();
    let reordered = registry_types::version_index_bytes(&reordered).unwrap();
    for tampered in [
        flipped,
        current.indexes[1].bytes.clone(),
        good[..good.len() - 1].to_vec(),
        reordered,
    ] {
        routes.set(&index_path, tampered);
        let error = app_cli::version_offers(PACKAGE_ID, "local").unwrap_err();
        assert!(error.contains("version index"), "{error}");
        let refused = app(&["rollback", PACKAGE_ID, "--to", "1.0.0"]);
        assert!(!refused.status.success());
    }
    routes.set(&index_path, good);
    app_cli::version_offers(PACKAGE_ID, "local").unwrap();

    // A replayed older snapshot, signed and naming its own valid index, is
    // still below the checkpoint.
    serve(&routes, &old);
    let error = app_cli::version_offers(PACKAGE_ID, "local").unwrap_err();
    assert!(error.contains("rolled back"), "{error}");
    assert!(!app_cli::available().unreachable.is_empty());
    let replayed = app(&["install", OTHER_ID, "--version", "3.0.0"]);
    assert!(!replayed.status.success());

    // A registry without the compact form serves the full snapshot, and the
    // same checkpoint accepts it: both forms share the snapshot's identity.
    serve(&routes, &current);
    routes.set(
        "/entries?format=compact",
        serde_json::to_vec(&current.full).unwrap(),
    );
    routes.requests();
    let offers = app_cli::version_offers(PACKAGE_ID, "local").unwrap();
    assert_eq!(offers.len(), 3);
    assert!(
        !routes
            .requests()
            .iter()
            .any(|request| request.starts_with("/indexes/")),
        "a full snapshot needs no index"
    );
}
