//! The size-pressure sweep removes least recently used unpinned packages
//! and preserves pinned ones; removal reclaims only unshared blobs.

use crate::common;
use app_host::install::install_tapp;
use app_host::{PackageStore, Pinning};

fn stored_blob_files(root: &std::path::Path) -> usize {
    std::fs::read_dir(root.join("content/data")).map_or(0, |entries| entries.flatten().count())
}

#[test]
fn gc_sweeps_tried_packages_but_never_pinned_or_shared_content() {
    // Phase `sweep_evicts_tried_and_preserves_pinned`.
    {
        let dir = tempfile::tempdir().unwrap();
        let store = PackageStore::at(dir.path());

        // One pinned and three tried packages, each with its own ~24 KiB blob.
        let (pinned_bytes, _) = common::build_tapp_for("so.tilo.pinned", 24);
        install_tapp(&store, &pinned_bytes, Pinning::Pinned, None).unwrap();
        let mut tried = Vec::new();
        for (index, last_used) in [1_000u64, 2_000, 3_000].into_iter().enumerate() {
            let app_id = format!("so.tilo.tried{index}");
            let (bytes, _) = common::build_tapp_for(&app_id, 24);
            install_tapp(&store, &bytes, Pinning::Tried, None).unwrap();
            // Distinct last-used times give the LRU order a defined answer.
            store
                .update_row(&app_id, |row| row.last_used = last_used)
                .unwrap();
            tried.push(app_id);
        }
        let before = store.disk_usage().unwrap().total_bytes;
        let before_blobs = stored_blob_files(dir.path());
        assert!(before > 0);

        // Cap at ~60 KiB: well under four packages, so at least two tried go.
        let removed = store.sweep(60 * 1024).unwrap();
        assert!(removed.len() >= 2, "{removed:?}");
        assert_eq!(
            removed,
            tried[..removed.len()],
            "the least recently used packages go first"
        );
        let registry = store.registry().unwrap();
        assert!(registry.find("so.tilo.pinned").is_some(), "pinned survives");
        assert!(store.disk_usage().unwrap().total_bytes <= 60 * 1024);
        assert!(
            stored_blob_files(dir.path()) < before_blobs,
            "blobs of evicted packages are reclaimed"
        );
    }
    // Phase `removing_one_package_preserves_shared_and_pinned_content`.
    {
        let dir = tempfile::tempdir().unwrap();
        let store = PackageStore::at(dir.path());
        let (pinned_bytes, _) = common::build_tapp_for("so.tilo.pinned", 24);
        let (tried_bytes, _) = common::build_tapp_for("so.tilo.tried", 48);
        install_tapp(&store, &pinned_bytes, Pinning::Pinned, None).unwrap();
        install_tapp(&store, &tried_bytes, Pinning::Tried, None).unwrap();

        let before_blobs = stored_blob_files(dir.path());
        let removed = store.remove("so.tilo.tried", false).unwrap();
        assert!(removed.evicted_blobs > 0);
        let registry = store.registry().unwrap();
        let pinned = registry.find("so.tilo.pinned").unwrap();
        assert!(registry.find("so.tilo.tried").is_none());
        assert!(stored_blob_files(dir.path()) < before_blobs);
        assert!(
            package_store::integrity::verify(pinned, &pinned.payload_root)
                .unwrap()
                .is_intact(),
            "the remaining package still has every byte"
        );
    }
    // Phase `sweep_disabled_when_cap_is_zero`.
    {
        let dir = tempfile::tempdir().unwrap();
        let store = PackageStore::at(dir.path());
        let (bytes, _) = common::build_tapp(2);
        install_tapp(&store, &bytes, Pinning::Tried, None).unwrap();
        assert!(store.sweep(0).unwrap().is_empty());
    }
}
