//! One lock for every test that touches process-wide blob state.
//!
//! A test that needs a store or transport of its own installs it with
//! `qdoc::blobstore::scope`, which only its own thread sees and which ends with the
//! test. What stays process-wide is when each hash was last asked for and first
//! wanted (the transport's rate limit and the pending clock), and all of these test
//! files are modules of one test binary, so tests that ask for the same content
//! would otherwise see each other's clocks.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::{Mutex, MutexGuard, OnceLock};

/// Start with a fresh default store and hold this for as long as a test relies on the process-wide store or transport
/// being the one it installed — or, for a test that only reads, on nobody having
/// swapped it.
pub fn serialised() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let guard = LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Serialization alone does not undo a preceding test's quota or transport.
    qdoc::blobstore::set_store(std::sync::Arc::new(
        qdoc::blobstore::MemoryBlobStore::default(),
    ));
    qdoc::blobstore::clear_transport();
    guard
}

#[test]
fn every_blob_fixture_starts_with_a_fresh_default_quota() {
    use qdoc::blobstore::{self, BlobStoreLimits, MemoryBlobStore};
    use std::sync::Arc;
    {
        let _guard = serialised();
        blobstore::set_store(Arc::new(MemoryBlobStore::new(BlobStoreLimits {
            max_blob_bytes: 2,
            max_total_bytes: 2,
            max_entries: 1,
            max_scan_entries: 1,
        })));
    }
    let _guard = serialised();
    assert!(
        qdoc::Document::new()
            .put_blob(b"larger than the prior quota")
            .is_some()
    );
}
