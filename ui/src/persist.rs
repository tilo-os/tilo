//! Making an app's state survive a restart.
//!
//! Historically the toolkit deliberately stayed away from the disk —
//! [`State::save`]/[`State::load`] convert to and from bytes and nothing
//! more — and each app was left to invent its own storage. Most never got
//! round to it: 29 of the shipped apps started from an empty `AutoCommit`
//! every launch, several of them while carrying a module comment that said
//! their state survived restarts.
//!
//! This module closes that gap in three lines of app code. Instead of
//!
//! ```ignore
//! let state = ui::State::new();
//! ui::App::new(state, my_app::app_root()).run();
//! ```
//!
//! an app writes
//!
//! ```ignore
//! let (state, autosave) = ui::persist::open();
//! ui::App::new(state, my_app::app_root()).with_autosave(autosave).run();
//! ```
//!
//! and gets: the previous session's document loaded before the first
//! `sync`, every subsequent change written back atomically and debounced,
//! and a flush on exit. Where the bytes live, and why an app has no say in
//! it, is documented on [`app_state`].
//!
//! Apps that also carry state an older release wrote somewhere else call
//! [`open_migrating`] once with that legacy path; the old document is
//! merged in rather than replacing anything, so the migration is safe to
//! run repeatedly and cannot orphan a user's data.

use std::path::Path;
use std::time::Duration;

pub use app_state::{Autosave, DEFAULT_DEBOUNCE, DocStore, StoreError};

use crate::state::State;

/// Load this package's persisted document and start its background writer.
///
/// Never fails the caller: if the state directory is unusable (read-only
/// home, missing `HOME`, a corrupt document) the error is logged and the app
/// comes up on an empty document, exactly as it did before persistence
/// existed. An app that would rather know can call [`try_open`].
pub fn open() -> (State, Autosave) {
    try_open().unwrap_or_else(|e| {
        tracing::error!(error = %e, "app state unavailable; running without persistence");
        in_memory_fallback()
    })
}

/// [`open`], plus a one-time import of a document an older release of this
/// app left at `legacy`.
///
/// The legacy document is *merged*, so state written since the upgrade is
/// preserved and running the migration twice is a no-op. The old file is
/// left on disk; deleting it is a decision for whoever knows whether
/// anything else still reads it.
pub fn open_migrating(legacy: &Path) -> (State, Autosave) {
    let attempt = || -> Result<(State, Autosave), StoreError> {
        let mut store = DocStore::open()?;
        match store.migrate_from(legacy) {
            Ok(true) => tracing::info!(
                from = %legacy.display(),
                to = %store.document_path().display(),
                "imported app state from its previous location"
            ),
            Ok(false) => {}
            Err(e) => tracing::error!(error = %e, "legacy app state could not be imported"),
        }
        finish(store)
    };
    attempt().unwrap_or_else(|e| {
        tracing::error!(error = %e, "app state unavailable; running without persistence");
        in_memory_fallback()
    })
}

/// [`open`] without the swallow-and-log fallback.
pub fn try_open() -> Result<(State, Autosave), StoreError> {
    finish(DocStore::open()?)
}

fn finish(mut store: DocStore) -> Result<(State, Autosave), StoreError> {
    let mut doc = store.load_or_new();
    let mut autosave = Autosave::spawn(store, DEFAULT_DEBOUNCE);
    autosave.seed(&mut doc);
    Ok((State::from_doc(doc), autosave))
}

/// The degraded path: an empty document and a writer that is honest about
/// being switched off. [`Autosave::is_enabled`] returns `false` and
/// [`Autosave::last_error`] carries the reason, so nothing downstream can
/// mistake this for a working store.
fn in_memory_fallback() -> (State, Autosave) {
    (State::new(), Autosave::disabled("state directory unavailable"))
}

/// Convenience for tests and tools: same as [`open`] but for an explicit
/// package id rather than `QOS_PACKAGE_ID`.
pub fn open_for(package_id: &str) -> Result<(State, Autosave), StoreError> {
    finish(DocStore::open_for(package_id)?)
}

/// The debounce window [`open`] uses. Re-exported so an app that wants a
/// different one can build its own [`Autosave`] from a [`DocStore`].
pub const DEBOUNCE: Duration = DEFAULT_DEBOUNCE;
