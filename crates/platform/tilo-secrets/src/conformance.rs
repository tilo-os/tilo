//! The contract every [`SecretStore`] keeps, as checks a backend's tests (and
//! a host's own backend's tests) run. They write, overwrite and delete one
//! key, `tilo-conformance`, of the store they are given.
#![allow(
    clippy::panic,
    clippy::expect_used,
    clippy::missing_panics_doc,
    reason = "a conformance check fails by panicking with the violated rule"
)]

use crate::{PutOptions, SecretError, SecretStore};

const KEY: &str = "tilo-conformance";

/// Round trip, overwrite, delete, missing reads, idempotent delete, and
/// malformed keys refused — through the async API.
pub async fn check_store(store: &SecretStore) {
    store
        .delete(KEY)
        .await
        .expect("deleting a missing key succeeds");
    assert_eq!(store.get(KEY).await.expect("a missing key reads"), None);
    store
        .put(KEY, b"\x00first\xff", PutOptions::default())
        .await
        .expect("put");
    assert_eq!(
        store
            .get(KEY)
            .await
            .expect("get")
            .as_ref()
            .map(crate::Secret::expose),
        Some(&b"\x00first\xff"[..]),
        "binary secrets round-trip exactly"
    );
    store
        .put(KEY, b"second", PutOptions::default())
        .await
        .expect("overwrite");
    assert_eq!(
        store
            .get(KEY)
            .await
            .expect("get")
            .as_ref()
            .map(crate::Secret::expose),
        Some(&b"second"[..]),
        "put replaces"
    );
    store.delete(KEY).await.expect("delete");
    assert_eq!(store.get(KEY).await.expect("get"), None, "delete removes");
    store.delete(KEY).await.expect("delete is idempotent");
    for bad in ["", "a\nb"] {
        assert_eq!(
            store.get(bad).await,
            Err(SecretError::InvalidKey),
            "{bad:?}"
        );
    }
}

/// [`check_store`] through the `*_blocking` API.
pub fn check_store_blocking(store: &SecretStore) {
    store
        .delete_blocking(KEY)
        .expect("deleting a missing key succeeds");
    assert_eq!(store.get_blocking(KEY).expect("a missing key reads"), None);
    store
        .put_blocking(KEY, b"\x00first\xff", PutOptions::default())
        .expect("put");
    assert_eq!(
        store
            .get_blocking(KEY)
            .expect("get")
            .as_ref()
            .map(crate::Secret::expose),
        Some(&b"\x00first\xff"[..])
    );
    store
        .put_blocking(KEY, b"second", PutOptions::default())
        .expect("overwrite");
    assert_eq!(
        store
            .get_blocking(KEY)
            .expect("get")
            .as_ref()
            .map(crate::Secret::expose),
        Some(&b"second"[..])
    );
    store.delete_blocking(KEY).expect("delete");
    assert_eq!(store.get_blocking(KEY).expect("get"), None);
    assert_eq!(store.get_blocking(""), Err(SecretError::InvalidKey));
}
