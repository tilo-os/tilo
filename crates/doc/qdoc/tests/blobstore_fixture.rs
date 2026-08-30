//! One lock for every test that touches the process-wide blob store.
//!
//! The store and the transport are properties of the machine, not of a document
//! (see `qdoc::blobstore`), and all of these test files are modules of one test
//! binary, so they share one process and run on threads of it. A test that
//! installs its own store would otherwise pull the store out from under a test
//! that had just written to it, on another thread, in another file.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test that unwraps is stating an invariant"
)]

use std::sync::{Mutex, MutexGuard, OnceLock};

/// Hold this for as long as a test relies on the process-wide store or transport
/// being the one it installed — or, for a test that only reads, on nobody having
/// swapped it.
pub fn serialised() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
