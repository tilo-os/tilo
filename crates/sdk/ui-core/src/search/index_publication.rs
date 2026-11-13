//! An app's search entries as the system's persistent search index takes them. The
//! vocabulary and the incremental [`Publisher`] live in the toolkit-free resource layer
//! ([`tilo_resource::search_index`]): a toolkit app's window and a tilolang app's
//! instance publish to the same one index, so both read the same types. Only short
//! metadata crosses: prose bodies stay in the app, and a secret-bearing entry
//! ([`IndexEntry::secret`]) carries what the app chose to show of it (a name, a username,
//! a site), never a secret.

pub use tilo_resource::search_index::{
    IndexAction, IndexDelta, IndexEntry, IndexKind, IndexPerson, IndexState, IndexWeight,
    MAX_DELTA_BYTES, MAX_ENTRIES, MAX_FIELDS, MAX_ID_BYTES, MAX_TEXT_BYTES, Publisher,
};
