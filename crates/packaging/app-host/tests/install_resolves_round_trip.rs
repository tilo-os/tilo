//! Round-trip test: install a .tapp via the library surface, then find it
//! in the package store and check the payload landed on disk.

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::common;
use app_host::install::{InstallError, InstallExpectation, install_tapp, install_verified_tapp};
use app_host::{PackageStore, Pinning};

#[test]
fn installs_resolve_list_and_bind_registry_identity() {
    // Phase `install_tapp_then_resolve_recovers_entry`.
    {
        let dir = tempfile::tempdir().unwrap();
        let store = PackageStore::at(dir.path());

        let (tapp_bytes, expected_id) = common::build_tapp(4);
        let installed = install_tapp(&store, &tapp_bytes, Pinning::Tried, None).unwrap();
        assert_eq!(installed.package_id(), Some(expected_id));

        let registry = store.registry().unwrap();
        let entry = registry.find_package(expected_id).expect("resolve");
        assert_eq!(entry.id, "so.tilo.editor");
        assert_eq!(entry.pinning(), Pinning::Tried);
        assert!(entry.payload_root.exists(), "payload dir must be created");
        assert!(entry.payload_root.join("bin/editor").exists());
        assert!(entry.payload_root.join("share/splash.png").exists());
        let manifest = std::fs::read_to_string(&entry.manifest_path).unwrap();
        assert!(manifest.contains("so.tilo.editor"));
        assert!(
            !dir.path().join("store").exists() && !dir.path().join("payloads").exists(),
            "no private blob directory and no second unpacked copy"
        );
    }
    // Phase `list_includes_installed_package`.
    {
        let dir = tempfile::tempdir().unwrap();
        let store = PackageStore::at(dir.path());
        let (bytes, id) = common::build_tapp(1);
        install_tapp(&store, &bytes, Pinning::Pinned, None).unwrap();

        let registry = store.registry().unwrap();
        let listed = registry.entries();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].package_id(), Some(id));
        assert_eq!(listed[0].pinning(), Pinning::Pinned);
    }
    // Phase `verified_install_binds_every_registry_identity_before_writing`.
    {
        let dir = tempfile::tempdir().unwrap();
        let store = PackageStore::at(dir.path());
        let (bytes, id) = common::build_tapp(1);
        let refused = |expected: &InstallExpectation| {
            let result = install_verified_tapp(&store, &bytes, Pinning::Tried, expected, None);
            assert!(
                matches!(
                    result,
                    Err(InstallError::Verify(_)
                        | InstallError::Store(package_store::StoreError::Refused(_)))
                ),
                "{result:?}"
            );
            assert!(
                store.registry().unwrap().is_empty(),
                "a rejected identity must not touch the registry"
            );
            assert!(
                !dir.path().join("pkgs").exists(),
                "nor materialise anything"
            );
        };

        let mut expected = expectation(&bytes, id);
        expected.app_id = "so.tilo.impostor".into();
        refused(&expected);

        let mut expected = expectation(&bytes, id);
        expected.publisher[0] ^= 0x80;
        refused(&expected);

        let mut expected = expectation(&bytes, id);
        expected.size_bytes += 1;
        refused(&expected);

        let mut expected = expectation(&bytes, id);
        expected.package_root = app_host::PackageId::from_bytes([1; 32]);
        refused(&expected);

        let installed = install_verified_tapp(
            &store,
            &bytes,
            Pinning::Tried,
            &expectation(&bytes, id),
            None,
        )
        .unwrap();
        assert_eq!(installed.package_id(), Some(id));
    }
}

fn expectation(bytes: &[u8], id: app_host::PackageId) -> InstallExpectation {
    let meta = app_registry::tapp::load(bytes).unwrap();
    InstallExpectation {
        package_root: id,
        publisher: meta.manifest.publisher.identity_bytes().unwrap(),
        app_id: meta.manifest.package.id,
        version: meta.manifest.package.version,
        size_bytes: bytes.len() as u64,
    }
}
