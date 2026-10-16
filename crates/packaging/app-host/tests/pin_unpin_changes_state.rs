//! pin / unpin flip a package's pin and persist in the package store.

#![allow(
    clippy::unwrap_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use crate::common;
use app_host::install::install_tapp;
use app_host::{PackageStore, Pinning};

fn pinning(store: &PackageStore, app_id: &str) -> Pinning {
    store.registry().unwrap().find(app_id).unwrap().pinning()
}

#[test]
fn pins_persist_and_block_removal() {
    // Phase `pin_and_unpin_flip_state_and_persist`.
    {
        let dir = tempfile::tempdir().unwrap();
        let store = PackageStore::at(dir.path());
        let (bytes, _id) = common::build_tapp(1);
        install_tapp(&store, &bytes, Pinning::Tried, None).unwrap();

        assert_eq!(pinning(&store, "so.tilo.editor"), Pinning::Tried);
        store.set_pinned("so.tilo.editor", true).unwrap();
        assert_eq!(pinning(&store, "so.tilo.editor"), Pinning::Pinned);
        store.set_pinned("so.tilo.editor", false).unwrap();
        assert_eq!(pinning(&store, "so.tilo.editor"), Pinning::Tried);

        // Persistence: another handle on the same root sees the last flip.
        store.set_pinned("so.tilo.editor", true).unwrap();
        drop(store);
        let reopened = PackageStore::at(dir.path());
        assert_eq!(pinning(&reopened, "so.tilo.editor"), Pinning::Pinned);
    }
    // Phase `remove_refuses_pinned`.
    {
        let dir = tempfile::tempdir().unwrap();
        let store = PackageStore::at(dir.path());
        let (bytes, _id) = common::build_tapp(1);
        install_tapp(&store, &bytes, Pinning::Pinned, None).unwrap();

        let err = store.remove("so.tilo.editor", false).unwrap_err();
        assert!(
            matches!(err, package_store::StoreError::Pinned(_)),
            "remove of a Pinned package must fail; got {err:?}"
        );
        assert!(store.registry().unwrap().find("so.tilo.editor").is_some());
    }
}
