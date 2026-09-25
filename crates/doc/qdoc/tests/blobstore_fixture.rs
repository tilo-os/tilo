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

/// Hold this for as long as a test relies on the process-wide store or transport
/// being the one it installed — or, for a test that only reads, on nobody having
/// swapped it.
pub fn serialised() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
