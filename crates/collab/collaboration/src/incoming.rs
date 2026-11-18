//! What reaches a document's owner from its route, as the owner merges it.

use automerge::{AutoCommit, AutomergeError};
use collab_types::DocumentMutation;
use tilo_automerge::ApplyIncoming as _;

/// What reached a document's owner: the whole document, already loaded, or
/// the changes since a revision. A committed read loads a whole document as it
/// arrives, off the owner's thread ([`crate::CommittedDocument::with_incoming`]),
/// so the owner's work is only what the document adds to its own.
pub enum Incoming<'a> {
    /// The whole document.
    Whole(&'a mut AutoCommit),
    /// Changes since a revision, as Automerge storage.
    Changes(&'a [u8]),
}

impl Incoming<'_> {
    /// `mutation`, as it arrived on a route, handed to `apply`: its whole
    /// document loaded here.
    ///
    /// # Errors
    ///
    /// A whole document that does not load.
    pub fn of<T>(
        mutation: &DocumentMutation,
        apply: impl FnOnce(Incoming<'_>) -> T,
    ) -> Result<T, AutomergeError> {
        Ok(match mutation {
            DocumentMutation::Snapshot(bytes) => {
                apply(Incoming::Whole(&mut AutoCommit::load(bytes)?))
            }
            DocumentMutation::Incremental(bytes) => apply(Incoming::Changes(bytes)),
        })
    }

    /// The same, lent again: an owner that cannot take it yet hands it on.
    pub fn reborrow(&mut self) -> Incoming<'_> {
        match self {
            Incoming::Whole(loaded) => Incoming::Whole(loaded),
            Incoming::Changes(bytes) => Incoming::Changes(bytes),
        }
    }

    /// Take it into `document`, merged with what `document` holds: whether
    /// `document` gained anything. Changes apply whole or not at all.
    ///
    /// # Errors
    ///
    /// Changes that do not parse, or what Automerge refuses of either.
    pub fn merge_into(self, document: &mut AutoCommit) -> Result<bool, AutomergeError> {
        match self {
            Incoming::Whole(loaded) => document.take_loaded(loaded),
            Incoming::Changes(bytes) => document
                .load_incoming_whole(bytes)
                .map(|applied| applied > 0),
        }
    }
}
