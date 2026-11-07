//! Tube's records: the library that syncs between everyone who shares it, and the person's watch
//! history, each with the verbs its app and its agents change them by.
//!
//! The app draws these records, and the session's document host runs their verbs in the module
//! built from this crate (`docs/architecture/apps.md` §1): what one person's app submits and
//! what a host commits are the same action, with the same typed arguments, on either side.
//!
//! Everything here is plain serde-able data — no toolkit types, no colours, no IO.

mod history;
#[cfg(feature = "module")]
mod module;
// Public so the app's tests reach the parameter structs the resource macro generates beside the
// type (`library::ReplyToComment`): the typed action arguments both sides share.
pub mod library;

pub use history::{MAX_HISTORY_ENTRIES, RESUME_TOLERANCE_MS, WatchHistory};
pub use library::{Library, Reply};
