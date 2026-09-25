//! pin / unpin flip a package's `Pinning` state and persist across
//! store re-opens.

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
use app_host::install::install_tapp;
use app_host::registry::Pinning;
use app_host::store::Store;

#[test]
fn pin_and_unpin_flip_state_and_persist() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (bytes, id) = common::build_tapp(1);
    install_tapp(&store, &bytes, Pinning::Tried).unwrap();

    // Initial: Tried.
    assert_eq!(store.resolve(id).unwrap().pinning, Pinning::Tried);

    // Pin.
    store.pin(id).unwrap();
    assert_eq!(store.resolve(id).unwrap().pinning, Pinning::Pinned);

    // Unpin.
    store.unpin(id).unwrap();
    assert_eq!(store.resolve(id).unwrap().pinning, Pinning::Tried);

    // Persistence: re-opening the store must preserve the last flip.
    store.pin(id).unwrap();
    drop(store);

    let reopened = Store::open(dir.path()).unwrap();
    assert_eq!(reopened.resolve(id).unwrap().pinning, Pinning::Pinned);
}

#[test]
fn remove_refuses_pinned() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (bytes, id) = common::build_tapp(1);
    install_tapp(&store, &bytes, Pinning::Pinned).unwrap();

    let err = store.remove(id).unwrap_err();
    assert!(
        matches!(err, app_host::store::StoreError::PinnedRemove(_)),
        "remove of a Pinned package must fail; got {err:?}"
    );
    // Still resolvable.
    assert!(store.resolve(id).is_ok());
}
