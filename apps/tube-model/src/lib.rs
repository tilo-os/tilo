//! Tube's records: the library that syncs between everyone who shares it, and the person's
//! watch history, each with the verbs its app and its agents change them by.

mod history;
// Public so the app's tests reach the parameter structs the resource macro generates beside
// the type (`library::ReplyToComment`): the typed action arguments both sides share.
pub mod library;

pub use history::{MAX_HISTORY_ENTRIES, RESUME_TOLERANCE_MS, WatchHistory};
pub use library::{Library, Reply};
