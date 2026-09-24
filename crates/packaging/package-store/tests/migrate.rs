//! Packages installed by the retired layouts move into the content store
//! once, and the old copies go.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use package_store::PackageStore;
use tilo_app_package::{Packed, SigningIdentity};

use crate::support::{bytes, package, scratch, tree};

fn files() -> BTreeMap<&'static str, Vec<u8>> {
    BTreeMap::from([
        ("bin/app", bytes(11, 100_000)),
        ("share/data.bin", bytes(12, 40_000)),
    ])
}

/// Lay `packed` out the way `tilo-install` used to: an unpacked tree with
/// the manifest inside it, its record under `integrity/`, and a row.
fn legacy_install(root: &Path, packed: &Packed, source_files: &BTreeMap<&str, Vec<u8>>) {
    let app_id = &packed.meta.manifest.package.id;
    let payload = root.join("payloads").join(app_id);
    for (rel, content) in source_files {
        let path = payload.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    std::fs::write(payload.join("manifest.toml"), &packed.meta.manifest_bytes).unwrap();
    let record = root.join("integrity").join(app_id);
    std::fs::create_dir_all(&record).unwrap();
    std::fs::write(record.join("signed-root"), packed.meta.signed_root_bytes()).unwrap();
    std::fs::write(record.join("manifest.toml"), &packed.meta.manifest_bytes).unwrap();
    std::fs::write(record.join("collection.qcl"), &packed.meta.collection_bytes).unwrap();
    let row = format!(
        "[[entry]]\nid = {app_id:?}\nversion = \"1.0.0\"\npackage_id_hex = {:?}\n\
         payload_root = {:?}\nmanifest_path = {:?}\npublisher_key_hex = {:?}\n\
         pinned = true\ninstalled_at = 1700000000\ncli_commands = []\nsource = \"/old.tapp\"\n",
        packed.id().to_hex(),
        payload.display().to_string(),
        payload.join("manifest.toml").display().to_string(),
        hex::encode(packed.meta.publisher()),
    );
    let registry = root.join("registry.toml");
    let mut text = std::fs::read_to_string(&registry).unwrap_or_default();
    text.push_str(&row);
    std::fs::write(registry, text).unwrap();
}

#[test]
fn a_legacy_install_moves_into_the_content_store_once() {
    let identity = SigningIdentity::generate();
    let source = files();
    let (_payload, packed) = package(&identity, "so.tilo.legacy", "1.0.0", &source);
    let root = scratch();
    legacy_install(root.path(), &packed, &source);
    // A second legacy row with no record cannot be verified; it is kept.
    std::fs::create_dir_all(root.path().join("payloads/so.tilo.orphan")).unwrap();
    let mut registry = std::fs::read_to_string(root.path().join("registry.toml")).unwrap();
    write!(
        registry,
        "[[entry]]\nid = \"so.tilo.orphan\"\npayload_root = {:?}\nmanifest_path = {:?}\n",
        root.path()
            .join("payloads/so.tilo.orphan")
            .display()
            .to_string(),
        root.path()
            .join("payloads/so.tilo.orphan/manifest.toml")
            .display()
            .to_string(),
    )
    .unwrap();
    std::fs::write(root.path().join("registry.toml"), registry).unwrap();

    let store = PackageStore::at(root.path());
    let report = store.migrate_legacy_layout().unwrap();
    assert_eq!(report.migrated, vec!["so.tilo.legacy".to_string()]);
    assert_eq!(report.kept.len(), 1);
    assert_eq!(report.kept[0].0, "so.tilo.orphan");

    let registry = store.registry().unwrap();
    let row = registry.find("so.tilo.legacy").unwrap();
    assert_eq!(row.payload_root, store.layout().payload(packed.id()));
    assert_eq!(row.manifest_path, store.layout().manifest(packed.id()));
    assert!(row.pinned, "row fields survive the move");
    assert_eq!(row.source, "/old.tapp");
    let expected: BTreeMap<String, Vec<u8>> = source
        .iter()
        .map(|(rel, content)| ((*rel).to_string(), content.clone()))
        .collect();
    assert_eq!(tree(&row.payload_root), expected);
    assert!(
        package_store::integrity::verify(row, &row.payload_root)
            .unwrap()
            .is_intact()
    );
    assert!(!root.path().join("payloads/so.tilo.legacy").exists());
    assert!(!root.path().join("integrity/so.tilo.legacy").exists());
    assert!(
        registry.find("so.tilo.orphan").is_some(),
        "an unverifiable row keeps launching as it did"
    );

    // Idempotent: nothing left to move, the orphan is reported again.
    let again = store.migrate_legacy_layout().unwrap();
    assert!(again.migrated.is_empty());
    assert_eq!(store.registry().unwrap(), registry);
}

#[test]
fn a_tampered_legacy_payload_is_kept_rather_than_imported() {
    let identity = SigningIdentity::generate();
    let source = files();
    let (_payload, packed) = package(&identity, "so.tilo.legacy", "1.0.0", &source);
    let root = scratch();
    legacy_install(root.path(), &packed, &source);
    std::fs::write(
        root.path().join("payloads/so.tilo.legacy/share/data.bin"),
        b"changed",
    )
    .unwrap();
    let store = PackageStore::at(root.path());
    let report = store.migrate_legacy_layout().unwrap();
    assert!(report.migrated.is_empty());
    assert_eq!(report.kept.len(), 1);
    assert!(root.path().join("payloads/so.tilo.legacy").exists());
}

/// Lay `packed` out the way the app host used to: every blob under
/// `store/<hash>`, an unpacked copy under `payloads/<package id>`, and a row
/// keyed by package id.
fn app_host_install(old: &Path, packed: &Packed, source_files: &BTreeMap<&str, Vec<u8>>) {
    let blobs = old.join("store");
    std::fs::create_dir_all(&blobs).unwrap();
    let put = |bytes: &[u8]| {
        std::fs::write(
            blobs.join(tilo_app_package::BlobHash::of(bytes).to_hex()),
            bytes,
        )
        .unwrap();
    };
    put(&packed.meta.signed_root_bytes());
    put(&packed.meta.manifest_bytes);
    put(&packed.meta.collection_bytes);
    let payload = old.join("payloads").join(packed.id().to_hex());
    for (rel, content) in source_files {
        put(content);
        let path = payload.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    std::fs::write(
        old.join("registry.toml"),
        format!(
            "[[entry]]\nid = {:?}\npinning = \"pinned\"\npayload_root = {:?}\n\
             manifest_text = \"\"\ninstalled_at_unix = 1700000000\nlast_used_unix = 1700000500\n",
            packed.id().to_hex(),
            payload.display().to_string()
        ),
    )
    .unwrap();
}

#[test]
fn the_app_host_store_is_imported_and_deleted() {
    let identity = SigningIdentity::generate();
    let source = files();
    let (_payload, packed) = package(&identity, "so.tilo.hosted", "2.0.0", &source);
    let old = scratch();
    app_host_install(old.path(), &packed, &source);
    let root = scratch();
    let store = PackageStore::at(root.path());

    let report = store.import_app_host_store(old.path()).unwrap();
    assert_eq!(report.migrated, vec!["so.tilo.hosted".to_string()]);
    let registry = store.registry().unwrap();
    let row = registry.find("so.tilo.hosted").unwrap();
    assert!(row.pinned);
    assert_eq!(row.last_used, 1_700_000_500);
    assert_eq!(row.version, "2.0.0");
    assert!(
        package_store::integrity::verify(row, &row.payload_root)
            .unwrap()
            .is_intact()
    );
    for gone in ["store", "payloads", "registry.toml"] {
        assert!(!old.path().join(gone).exists(), "{gone} is removed");
    }
    assert!(
        store.import_app_host_store(old.path()).unwrap().is_empty(),
        "a second run finds nothing to import"
    );
}
