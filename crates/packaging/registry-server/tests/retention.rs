//! Version retention end to end: a publisher drops old versions from its
//! signed entry, the registry signs the shorter entry, and pruning removes
//! the dropped artifacts only once no snapshot or version index lists them.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    reason = "integration-test setup and assertions"
)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use ed25519_dalek::SigningKey;
use registry_server::storage::SNAPSHOT_HISTORY_RETAINED;
use registry_server::{PublisherAllowlist, PublisherRow, ServerIdentity, Storage};
use registry_types::retention::RetentionPolicy;
use registry_types::{RegistryEntry, RegistryTarget, RegistryVersion, sign_entry};

const SHARED: &[u8] = b"#!/bin/true\n";

struct Registry {
    dir: tempfile::TempDir,
    storage: Storage,
    owner_hex: String,
    key_path: PathBuf,
    publisher: SigningKey,
    _keys: tempfile::TempDir,
}

fn registry() -> Registry {
    let dir = tempfile::tempdir().unwrap();
    let keys = tempfile::tempdir().unwrap();
    let storage = Storage::open(dir.path()).unwrap();
    let owner = SigningKey::from_bytes(&[51; 32]);
    let publisher = SigningKey::from_bytes(&[52; 32]);
    let owner_hex = hex::encode(owner.verifying_key().to_bytes());
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
    Registry {
        dir,
        storage,
        owner_hex,
        key_path,
        publisher,
        _keys: keys,
    }
}

/// A staged package for `version` with one payload blob shared by every
/// version and one of its own; the version and that blob's hash.
fn stage(storage: &Storage, scratch: &Path, version: &str) -> (RegistryVersion, String) {
    let identity = tilo_app_package::SigningIdentity::generate();
    let payload = scratch.join(version);
    std::fs::create_dir_all(payload.join("bin")).unwrap();
    std::fs::write(payload.join("bin/app"), SHARED).unwrap();
    let unique = format!("data of {version}");
    std::fs::write(payload.join("bin/data"), &unique).unwrap();
    let manifest = tilo_app_package::Manifest::from_toml_str(&format!(
        "[package]\nid = \"so.tilo.retained\"\nversion = \"{version}\"\nkind = \"app\"\n\
         [publisher]\nidentity = \"ed25519:{}\"\nsignature = \"\"\n\
         [sandbox]\nnamespaces = [\"user\"]\nseccomp = \"tilo-default\"\nlandlock = \"strict\"\n\
         [sandbox.net]\nmode = \"off\"\n\
         [entrypoint]\nbin = \"bin/app\"\ntype = \"gui\"\n",
        hex::encode(identity.public_key_bytes())
    ))
    .unwrap();
    let packed = tilo_app_package::pack(&payload, &manifest, &identity).unwrap();
    let bytes = tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed)).unwrap();
    let digest = blake3::hash(&bytes).to_hex().to_string();
    storage.save_staged_artifact(&digest, &bytes).unwrap();
    let version = RegistryVersion {
        version: version.into(),
        released_at_unix: 1,
        artifact: registry_types::VersionArtifact::Tapp(registry_types::TappArtifact {
            tapp_url: format!("https://apps.tilo.so/artifacts/{digest}.tapp"),
            tapp_root_hash_hex: packed.meta.id.to_hex(),
            tapp_size_bytes: bytes.len() as u64,
            tapp_blake3_hex: digest,
            target: RegistryTarget {
                platform: registry_types::TILO_OS_PLATFORM.into(),
                architecture: "x86_64".into(),
                isa: "x86_64-v3".into(),
                runtime: None,
                interfaces: Vec::new(),
            },
        }),
    };
    let blob = tilo_app_package::BlobHash::of(unique.as_bytes()).to_hex();
    (version, blob)
}

impl Registry {
    fn save(&self, versions: Vec<RegistryVersion>, updated_at_unix: u64) {
        let mut entry = RegistryEntry {
            package_id: "so.tilo.retained".into(),
            kind: registry_types::EntryKind::App,
            name: "Retained".into(),
            description: "An app whose old versions retention drops".into(),
            tagline: "Retained on Tilo OS".into(),
            images: registry_types::LocalizedImages::english_icon(
                "https://apps.tilo.so/images/test-icon.svg",
                "Test icon",
            ),
            supported_languages: vec!["en".into()],
            categories: vec!["Test".into()],
            source_url: None,
            homepage: None,
            publisher_pubkey_hex: hex::encode(self.publisher.verifying_key().to_bytes()),
            versions,
            updated_at_unix,
            signature_hex: String::new(),
        };
        sign_entry(&mut entry, &self.publisher);
        self.storage.save_entry(&entry).unwrap();
    }

    fn run(&self, args: &[&str]) -> String {
        let output = Command::new(tilo_test_env::package_binary!("tilo-registry-server"))
            .arg("--data-dir")
            .arg(self.dir.path())
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn sign(&self) {
        let key = self.key_path.to_str().unwrap();
        self.run(&["sign-snapshot", "--owner-key-file", key]);
    }

    fn blob_exists(&self, blob: &str) -> bool {
        self.storage.load_payload_blob(blob).unwrap().is_some()
    }

    /// Every version the current snapshot lists rebuilds from storage.
    fn assert_listed_versions_install(&self) {
        let current = self.storage.load_snapshot().unwrap().unwrap();
        registry_types::verify_snapshot(&current, &self.owner_hex, current.issued_at_unix).unwrap();
        for version in current.entries.iter().flat_map(|entry| &entry.versions) {
            let bytes = self
                .storage
                .load_staged_artifact(&version.tapp().unwrap().tapp_blake3_hex)
                .unwrap();
            assert_eq!(
                blake3::hash(&bytes).to_hex().as_str(),
                version.tapp().unwrap().tapp_blake3_hex
            );
        }
    }
}

#[test]
fn retention_prunes_only_versions_nothing_references() {
    let registry = registry();
    let scratch = tempfile::tempdir().unwrap();
    let staged: Vec<(RegistryVersion, String)> = (1..=8)
        .map(|n| stage(&registry.storage, scratch.path(), &format!("1.0.{n}")))
        .collect();
    let all: Vec<RegistryVersion> = staged.iter().map(|(version, _)| version.clone()).collect();
    registry.save(all.clone(), 1);
    registry.sign();
    let shared = tilo_app_package::BlobHash::of(SHARED).to_hex();

    // Everything is listed: pruning deletes nothing.
    let pruned = registry
        .storage
        .lock_snapshot()
        .unwrap()
        .prune_unreferenced()
        .unwrap();
    assert_eq!(pruned.payload_blobs + pruned.thin_packages, 0);

    // The dry run names the versions the policy drops and what goes then,
    // and deletes nothing.
    let policy = RetentionPolicy {
        releases: 5,
        prereleases: 5,
    };
    let output = registry.run(&[
        "prune-storage",
        "--dry-run",
        "--keep-releases",
        "5",
        "--keep-prereleases",
        "5",
    ]);
    let preview: serde_json::Value = serde_json::from_str(&output).unwrap();
    let retention = &preview["preview"]["retention"];
    let dropped: Vec<&str> = retention["dropped_versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|version| version["version"].as_str().unwrap())
        .collect();
    assert_eq!(dropped, ["1.0.1", "1.0.2", "1.0.3"]);
    assert_eq!(retention["prune"]["payload_blobs"], 3);
    assert_eq!(retention["prune"]["thin_packages"], 3);
    assert_eq!(retention["prune"]["staged_artifacts"], 3);
    assert_eq!(preview["preview"]["now"]["payload_blobs"], 0);
    assert!(staged.iter().all(|(_, blob)| registry.blob_exists(blob)));

    // The publisher signs the shorter entry; the registry signs it in.
    let kept = policy.retain(&all);
    assert_eq!(kept.len(), 5);
    registry.save(kept.clone(), 2);
    registry.sign();
    registry.assert_listed_versions_install();

    // The dropped versions are still in the retained history, which an
    // operator may roll back to, and in the version index a client one
    // snapshot behind was offered: nothing is deleted yet.
    let pruned = registry
        .storage
        .lock_snapshot()
        .unwrap()
        .prune_unreferenced()
        .unwrap();
    assert_eq!(pruned.payload_blobs + pruned.thin_packages, 0);
    for (version, blob) in &staged[..3] {
        assert!(registry.blob_exists(blob));
        registry
            .storage
            .load_staged_artifact(&version.tapp().unwrap().tapp_blake3_hex)
            .unwrap();
    }

    // Once the history and served indexes have moved past them, exactly the
    // dropped versions' artifacts go and every listed version still installs.
    for _ in 0..SNAPSHOT_HISTORY_RETAINED {
        registry.sign();
    }
    let pruned = registry
        .storage
        .lock_snapshot()
        .unwrap()
        .prune_unreferenced()
        .unwrap();
    assert_eq!(
        (
            pruned.staged_artifacts,
            pruned.thin_packages,
            pruned.payload_blobs
        ),
        (3, 3, 3)
    );
    assert!(pruned.bytes > 0);
    for (version, blob) in &staged[..3] {
        assert!(!registry.blob_exists(blob));
        assert_eq!(
            registry
                .storage
                .find_staged_artifact(&version.tapp().unwrap().tapp_blake3_hex)
                .unwrap(),
            None
        );
    }
    for (_, blob) in &staged[3..] {
        assert!(registry.blob_exists(blob));
    }
    assert!(registry.blob_exists(&shared));
    registry.assert_listed_versions_install();

    // The compact snapshot's version index lists exactly the kept versions.
    let compact = registry.storage.load_compact_snapshot().unwrap().unwrap();
    let [listed] = compact.entries.as_slice() else {
        panic!("one entry: {:?}", compact.entries);
    };
    let index = registry
        .storage
        .load_version_index(&listed.version_index.blake3_hex)
        .unwrap()
        .unwrap();
    let full = registry_types::verify_version_index(listed, &index).unwrap();
    assert_eq!(full.versions, kept);

    // A second run finds nothing more.
    let pruned = registry
        .storage
        .lock_snapshot()
        .unwrap()
        .prune_unreferenced()
        .unwrap();
    assert_eq!(pruned.payload_blobs + pruned.thin_packages, 0);
}
