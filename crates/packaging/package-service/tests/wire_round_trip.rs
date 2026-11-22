//! End-to-end round trip: serve the `PackageServiceServer` on a Unix socket on its
//! own thread, connect the Store's client, and drive install → list → pin.

use crate::common;

use package_service::install::{InstallExpectation, install_tapp};
use package_store::PackageStore;

#[test]
fn install_list_pin_over_the_wire() {
    let h = common::Harness::new();
    let client = common::store_client(h.socket());

    let (bytes, expected_id) = common::build_tapp(2);
    let publisher = app_registry::tapp::load(&bytes)
        .unwrap()
        .manifest
        .publisher
        .identity_bytes()
        .unwrap();
    let installed = client
        .install(
            &bytes,
            &InstallExpectation {
                package_root: expected_id,
                publisher,
                app_id: "so.tilo.editor".into(),
                version: "1.4.2".into(),
                size_bytes: bytes.len() as u64,
            },
        )
        .unwrap();
    assert_eq!(installed, expected_id);

    let listed = client.list().unwrap();
    let [entry] = listed.as_slice() else {
        panic!("one listed package, got {listed:?}");
    };
    assert!(!entry.package.pinned);
    assert!(
        entry.package.user_managed(),
        "package-service entries are mutable"
    );
    assert_eq!(entry.package.package_id_hex, expected_id.to_hex());
    assert!(!entry.package.payload_root.as_os_str().is_empty());
    assert_eq!(entry.package.id, "so.tilo.editor");

    client.pin(expected_id, true).unwrap();
    assert!(client.list().unwrap()[0].package.pinned);
    client.pin(expected_id, false).unwrap();
    assert!(!client.list().unwrap()[0].package.pinned);
}

#[test]
fn registry_catalogue_refuses_caller_supplied_endpoints() {
    let h = common::Harness::new();
    let client = common::store_client(h.socket());
    let error = client
        .registry_catalogue("https://caller-controlled.example.test/")
        .expect_err("caller endpoint must be refused");
    assert!(
        error.to_string().contains("not configured and enabled"),
        "{error}"
    );
}

#[test]
fn list_reports_payload_bytes_without_walking() {
    let h = common::Harness::new();
    let store = PackageStore::at(h.store_root());
    let (bytes, _) = common::build_tapp(2);
    install_tapp(&store, &bytes, package_service::Pinning::Tried, None).unwrap();
    let row = store.registry().unwrap().entries()[0].clone();
    let measured = package_store::tree_size(&row.payload_root);
    assert!(measured > 0);
    assert_eq!(
        row.payload_bytes, measured,
        "install records the tree's size"
    );

    let client = common::store_client(h.socket());
    let listed_size = || {
        let listed = client.list().unwrap();
        assert_eq!(listed.len(), 1);
        listed[0].package.payload_bytes
    };
    assert_eq!(listed_size(), measured);

    // With the tree gone, a walk would report 0; the recorded size stands.
    make_writable(&row.payload_root);
    std::fs::remove_dir_all(&row.payload_root).unwrap();
    assert_eq!(listed_size(), measured);
}

/// Materialised payload files and their directories are read-only.
fn make_writable(root: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    for entry in std::fs::read_dir(root).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            make_writable(&path);
        }
    }
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o755)).unwrap();
}
