//! Tube's records: the library that syncs between everyone who shares it, and the person's watch
//! history, each with the verbs its app and its agents change them by.
//!
//! The app draws these records and changes them by their verbs: what one person's app submits
//! and what another device applies are the same action, with the same typed arguments, on
//! either side.
//!
//! Everything here is plain serde-able data — no toolkit types, no colours, no IO.

mod history;
// Public so the app's tests reach the parameter structs the resource macro generates beside the
// type (`library::ReplyToComment`): the typed action arguments both sides share.
pub mod library;

pub use history::{MAX_HISTORY_ENTRIES, RESUME_TOLERANCE_MS, WatchHistory};
pub use library::{Library, Reply};
