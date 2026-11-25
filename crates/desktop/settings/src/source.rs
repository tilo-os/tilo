//! What this process's settings source left to do: writes it holds back, and writes that failed
//! after the caller moved on. The source itself, and the document a host installs, are
//! `settings_model::source`'s.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::DeferredFailure;

pub fn flush_pending_writes() {
    if let Some(source) = crate::source() {
        source.flush_pending();
    }
}

static DEFERRED_FAILED: AtomicBool = AtomicBool::new(false);

/// A source records that a deferred write failed and was reverted (or, an
/// app's view of the session's settings, that the session refused it).
pub fn note_deferred_failure() {
    DEFERRED_FAILED.store(true, Ordering::Release);
}

#[must_use]
pub fn deferred_failure_pending() -> bool {
    DEFERRED_FAILED.load(Ordering::Acquire)
}

/// The failures of deferred writes since the last call, each once
/// ([`SettingsSource::take_deferred_failures`]).
#[must_use]
pub fn take_deferred_failures() -> Vec<DeferredFailure> {
    if !DEFERRED_FAILED.swap(false, Ordering::AcqRel) {
        return Vec::new();
    }
    crate::source().map_or_else(Vec::new, |source| source.take_deferred_failures())
}
