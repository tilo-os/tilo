//! GC sweep removes LRU-Tried entries under size pressure and preserves
//! Pinned ones. Uses .tapp installs so both the payload tree and
//! the blob store contribute to reported disk usage.

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
use app_host::gc;
use app_host::install::install_tapp;
use app_host::registry::Pinning;
use app_host::store::Store;

#[test]
fn sweep_evicts_tried_and_preserves_pinned() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();

    // Install one Pinned + three Tried, each with ~8 KiB payload.
    let (pinned_bytes, pinned_id) = common::build_tapp(8);
    install_tapp(&store, &pinned_bytes, Pinning::Pinned).unwrap();

    let mut tried_ids = Vec::new();
    for last_used_unix in [1_000, 2_000, 3_000] {
        let (bytes, id) = common::build_tapp(8);
        install_tapp(&store, &bytes, Pinning::Tried).unwrap();
        tried_ids.push(id);
        // Distinct last-used times give the LRU order a defined answer.
        let mut entry = store.resolve(id).unwrap();
        entry.last_used_unix = last_used_unix;
        store.put_entry(entry).unwrap();
    }

    let before = store.total_bytes_for_gc().unwrap();
    let before_blobs = store.disk_usage().unwrap().store_bytes;
    assert!(before > 0);

    // Cap the store at ~10 KiB — well under the total of 4 packages.
    // Two payload trees of 8 KiB + shared blobs is ~16-24 KiB total,
    // so the sweep will need to evict at least two Tried.
    let report = gc::sweep(&store, 10 * 1024).unwrap();

    // Some Tried must have been evicted.
    assert!(
        report.touched(),
        "sweep should have evicted at least one Tried"
    );
    // Pinned must be untouched.
    assert!(
        store.resolve(pinned_id).is_ok(),
        "Pinned package must survive gc"
    );
    // Eviction runs least recently used first: whatever was removed is
    // the oldest Tried packages, in order.
    assert_eq!(
        report.removed,
        tried_ids[..report.removed.len()],
        "gc must evict the least recently used Tried packages first"
    );
    // Non-decreasing tally: freed_bytes > 0 since we actually removed
    // payload trees.
    assert!(report.freed_bytes > 0);
    assert!(
        store.disk_usage().unwrap().store_bytes < before_blobs,
        "unreferenced content-addressed blobs must be reclaimed too"
    );
}

#[test]
fn removing_one_package_preserves_shared_and_pinned_content_graphs() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (pinned_bytes, pinned_id) = common::build_tapp(8);
    let (tried_bytes, tried_id) = common::build_tapp(16);
    install_tapp(&store, &pinned_bytes, Pinning::Pinned).unwrap();
    install_tapp(&store, &tried_bytes, Pinning::Tried).unwrap();

    let before_blobs = store.disk_usage().unwrap().store_bytes;
    store.remove(tried_id).unwrap();

    assert!(store.resolve(pinned_id).is_ok());
    assert!(store.resolve(tried_id).is_err());
    assert!(store.disk_usage().unwrap().store_bytes < before_blobs);
}

#[test]
fn sweep_disabled_when_cap_is_zero() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (bytes, _) = common::build_tapp(2);
    install_tapp(&store, &bytes, Pinning::Tried).unwrap();
    let r = gc::sweep(&store, 0).unwrap();
    assert!(!r.touched());
}
