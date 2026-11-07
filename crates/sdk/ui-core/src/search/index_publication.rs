//! An app's search entries as the system's persistent search index takes
//! them.
//!
//! The vocabulary and the incremental [`Publisher`] live in the toolkit-free
//! resource layer ([`tilo_resource::search_index`]): a toolkit app's window
//! and a tilolang app's instance publish to the same one index, so both read
//! the same types. They are re-exported here under their toolkit path,
//! which every app provider and the framework's root already use.
//!
//! A running app publishes what its search finds (each entry's title,
//! secondary line and short fields, keyed by the stable resource id a hit
//! opens) to the session over its certified broker route, and the session
//! keeps it after the app closes, so the app's content stays findable
//! whether or not it runs. Only short metadata crosses: prose bodies stay in
//! the app, and a secret-bearing entry ([`IndexEntry::secret`]) carries what
//! the app chose to show of it (a name, a username, a site), never a secret.
//!
//! The publication is incremental: a [`Publisher`] remembers what its
//! connection last sent and sends an [`IndexDelta`] of what changed, the
//! whole set again (`reset`) only on a new route.

pub use tilo_resource::search_index::{
    IndexAction, IndexDelta, IndexEntry, IndexKind, IndexPerson, IndexState, IndexWeight,
    MAX_DELTA_BYTES, MAX_ENTRIES, MAX_FIELDS, MAX_ID_BYTES, MAX_TEXT_BYTES, Publisher,
};
