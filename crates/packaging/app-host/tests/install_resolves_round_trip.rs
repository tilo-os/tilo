//! Round-trip test: install a real .qpkg via the library surface,
//! then resolve it and check the payload landed on disk.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::common;
use app_host::install::{InstallExpectation, install_qpkg, install_verified_qpkg};
use app_host::registry::Pinning;
use app_host::store::Store;

#[test]
fn install_qpkg_then_resolve_recovers_entry() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();

    let (qpkg_bytes, expected_id) = common::build_qpkg(4);
    let installed_id = install_qpkg(&store, &qpkg_bytes, Pinning::Tried).unwrap();
    assert_eq!(installed_id, expected_id);

    let entry = store.resolve(installed_id).expect("resolve");
    assert_eq!(entry.id, installed_id);
    assert_eq!(entry.pinning, Pinning::Tried);
    assert!(entry.payload_root.exists(), "payload dir must be created");
    // Payload files must be present.
    assert!(entry.payload_root.join("bin/editor").exists());
    assert!(entry.payload_root.join("share/splash.png").exists());
    // Manifest text is stored verbatim.
    assert!(!entry.manifest_text.is_empty());
    assert!(entry.manifest_text.contains("dev.qos.editor"));
}

#[test]
fn list_includes_installed_package() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (bytes, id) = common::build_qpkg(1);
    install_qpkg(&store, &bytes, Pinning::Pinned).unwrap();

    let listed: Vec<_> = store.list();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, id);
    assert_eq!(listed[0].pinning, Pinning::Pinned);
}

fn expectation(bytes: &[u8], id: app_host::PackageId) -> InstallExpectation {
    let meta = app_registry::qpkg::load(bytes).unwrap();
    InstallExpectation {
        package_root: id,
        publisher: meta.manifest.publisher.identity_bytes().unwrap(),
        app_id: meta.manifest.package.id,
        version: meta.manifest.package.version,
        size_bytes: bytes.len() as u64,
    }
}

#[test]
fn verified_install_binds_every_registry_identity_before_writing() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (bytes, id) = common::build_qpkg(1);
    let mut expected = expectation(&bytes, id);
    expected.app_id = "dev.qos.impostor".into();
    assert!(install_verified_qpkg(&store, &bytes, Pinning::Tried, &expected).is_err());
    assert!(
        store.list().is_empty(),
        "a rejected identity must not touch the registry"
    );

    let mut expected = expectation(&bytes, id);
    expected.publisher[0] ^= 0x80;
    assert!(install_verified_qpkg(&store, &bytes, Pinning::Tried, &expected).is_err());
    assert!(store.list().is_empty());

    let mut expected = expectation(&bytes, id);
    expected.size_bytes += 1;
    assert!(install_verified_qpkg(&store, &bytes, Pinning::Tried, &expected).is_err());
    assert!(store.list().is_empty());
}
