//! Bounded reads of an admitted document in an owner-hosted room.
//!
//! The committed view's live [`tilo_peer_state::StatePeer`] is never cloned
//! or saved for a reader. A new or changed document restores the room's
//! durable bytes into a [`tilo_peer_state::BoundedStatePeer`] under explicit
//! limits, rechecks the reader's current membership and document Read
//! permission there, and refuses a document that is encrypted, causally
//! incomplete or larger than the caller can hold. A reader that already
//! holds the admitted revision is answered from those heads, without
//! restoring the room again.
//!
//! A canonical checkpoint reaches a receiving owner as several signed frames.
//! Between the frame that moves the room's coverage and the frame carrying the
//! covered snapshot, the durable bytes cannot be restored. That is a state the
//! owner leaves by itself, not a refusal: the read reports
//! [`DocumentRead::CheckpointPending`] and hands out nothing.
//!
//! A reader that holds an earlier revision this owner handed it (by its
//! heads) is answered with only the changes since, from the committed view's
//! own document ([`NativeCommittedView::read_changes`]). That view is what
//! the room durably committed: its `record.peer` is the view's durable form,
//! and both are published together. So the changes lead to the revision a
//! restoring read would hand out. Whatever only a restoring read can decide
//! (a pending checkpoint; an encrypted, incomplete or absent document; a
//! base the document does not hold) is left to
//! [`NativeCommittedView::read_document`].
use crate::NativeCommittedView;
use std::time::SystemTime;
use tilo_peer_state::{
    BoundedPeerError, BoundedPeerInvalid, BoundedStatePeer, PeerDocumentReadError, PeerLimits,
};
use tilo_user_identity::RevocationList;

#[cfg(test)]
thread_local! {
    static ROOM_RESTORES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many times a document read restored the room. Tests use this to prove
/// an unchanged revision does not pay that cost.
#[cfg(test)]
pub(crate) fn room_restores() -> usize {
    ROOM_RESTORES.with(std::cell::Cell::get)
}

#[cfg(test)]
pub(crate) fn reset_room_restores() {
    ROOM_RESTORES.with(|count| count.set(0));
}

/// A document as the room last admitted it.
pub struct CommittedDocument {
    /// Names exactly these heads; equal revisions are equal documents.
    pub revision: [u8; 32],
    /// The document's heads, sorted: what a later
    /// [`NativeCommittedView::read_changes`] starts from.
    pub heads: Vec<[u8; 32]>,
    pub bytes: Vec<u8>,
}

/// The changes since a revision the reader holds.
pub struct DocumentChanges {
    /// The revision the changes lead to.
    pub revision: [u8; 32],
    /// Its heads, sorted.
    pub heads: Vec<[u8; 32]>,
    /// Automerge change bytes. They apply to a document holding every head
    /// of the revision they start from.
    pub bytes: Vec<u8>,
}

/// What a read of the changes since a held revision found.
pub enum ChangesRead {
    /// The reader already holds this revision.
    Unchanged([u8; 32]),
    Changes(DocumentChanges),
    /// Only a restoring [`NativeCommittedView::read_document`] can answer.
    Whole,
}

/// What a bounded read found.
pub enum DocumentRead {
    /// The reader already holds this revision.
    Unchanged([u8; 32]),
    Captured(CommittedDocument),
    /// The room is between the frames of one checkpoint: its coverage names
    /// a snapshot that has not arrived yet. Nothing was read; the reader may
    /// ask again once the rest of the checkpoint is admitted.
    CheckpointPending,
}

fn sorted(heads: &[automerge::ChangeHash]) -> Vec<[u8; 32]> {
    let mut heads = heads.iter().map(|head| head.0).collect::<Vec<_>>();
    heads.sort_unstable();
    heads
}

fn revision_of(heads: &[automerge::ChangeHash]) -> [u8; 32] {
    revision_of_sorted(&sorted(heads))
}

/// The revision naming exactly `heads`, which are sorted.
fn revision_of_sorted(heads: &[[u8; 32]]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new_derive_key("tilo committed document revision v1");
    for head in heads {
        hasher.update(head);
    }
    *hasher.finalize().as_bytes()
}

impl NativeCommittedView {
    /// Whether a `reader` holding `known` holds the admitted revision of
    /// `app`'s `record`, answered from the admitted heads without restoring
    /// the room; `None` when those heads cannot vouch for the record
    /// (encrypted, incomplete or absent), and only [`Self::read_document`]
    /// can say.
    ///
    /// # Errors
    /// The reader may not read the record now.
    pub fn holds_revision(
        &self,
        reader: [u8; 32],
        app: &str,
        record: &str,
        known: [u8; 32],
        now: SystemTime,
    ) -> Result<Option<bool>, String> {
        let Some(heads) = self.peer.plaintext_complete_heads(app, record) else {
            return Ok(None);
        };
        self.peer.check_document_read(reader, app, record, now)?;
        Ok(Some(revision_of(&heads) == known))
    }

    /// Read `app`'s `record` for the device `reader`.
    ///
    /// # Errors
    /// The reader may not read it now, the room does not hold it as a
    /// complete plaintext document, or it is larger than `maximum` bytes.
    pub fn read_document(
        &self,
        reader: [u8; 32],
        app: &str,
        record: &str,
        known_revision: Option<[u8; 32]>,
        maximum: usize,
        now: SystemTime,
    ) -> Result<DocumentRead, String> {
        // Idle apps re-read the revision they already hold. Answer that from
        // the admitted heads. Restoring the room is only for a document this
        // caller does not yet have, or for a route those heads cannot vouch
        // for (encrypted, incomplete, or absent).
        if let Some(known) = known_revision
            && let Some(heads) = self.peer.plaintext_complete_heads(app, record)
        {
            let revision = revision_of(&heads);
            if revision == known {
                self.peer.check_document_read(reader, app, record, now)?;
                return Ok(DocumentRead::Unchanged(revision));
            }
        }
        let limits = PeerLimits::default();
        #[cfg(test)]
        ROOM_RESTORES.with(|count| count.set(count.get().saturating_add(1)));
        let restored = match BoundedStatePeer::restore(
            &self.record.peer,
            &self.record.room.to_string(),
            &self.record.governor,
            std::slice::from_ref(&self.record.device),
            &RevocationList::new(),
            now,
            &limits,
        ) {
            Ok(restored) => restored,
            Err(BoundedPeerError::Invalid(BoundedPeerInvalid::Coverage)) => {
                // Still answer only a reader who may read the record now.
                self.peer.check_document_read(reader, app, record, now)?;
                return Ok(DocumentRead::CheckpointPending);
            }
            Err(error) => return Err(error.to_string()),
        };
        let read = restored.read();
        let document =
            read.document_read(reader, app, record, now)
                .map_err(|error| match error {
                    PeerDocumentReadError::Denied => "the reader may not read that document".into(),
                    PeerDocumentReadError::Missing => {
                        format!("the room does not hold {app} record {record:?}")
                    }
                    other => other.to_string(),
                })?;
        let heads = sorted(document.heads());
        let revision = revision_of_sorted(&heads);
        if known_revision == Some(revision) {
            return Ok(DocumentRead::Unchanged(revision));
        }
        let bytes = document.document().clone().save();
        if bytes.len() > maximum {
            return Err("the document is larger than the reader can hold".into());
        }
        Ok(DocumentRead::Captured(CommittedDocument {
            revision,
            heads,
            bytes,
        }))
    }

    /// Read the changes to `app`'s `record` since the revision with the
    /// sorted `heads` (as an earlier read handed them out) for the device
    /// `reader`, from the committed document's own changes: no room restore
    /// and no save of the whole document.
    ///
    /// Checks the reader's document Read permission, as
    /// [`Self::holds_revision`] does, before answering anything.
    /// [`ChangesRead::Whole`] leaves the answer to [`Self::read_document`]:
    /// a checkpoint is pending; the document is encrypted, incomplete or
    /// absent; `heads` are not sorted and distinct or not all in the
    /// document; or the changes exceed `maximum` bytes.
    ///
    /// # Errors
    /// The reader may not read the record now.
    pub fn read_changes(
        &self,
        reader: [u8; 32],
        app: &str,
        record: &str,
        heads: &[[u8; 32]],
        maximum: usize,
        now: SystemTime,
    ) -> Result<ChangesRead, String> {
        if self.peer.checkpoint_pending(app, record) {
            return Ok(ChangesRead::Whole);
        }
        let Some(current) = self.peer.plaintext_complete_heads(app, record) else {
            return Ok(ChangesRead::Whole);
        };
        self.peer.check_document_read(reader, app, record, now)?;
        let revision = revision_of(&current);
        if revision == revision_of_sorted(heads) {
            return Ok(ChangesRead::Unchanged(revision));
        }
        if !heads
            .windows(2)
            .all(|pair| matches!(pair, [first, second] if first < second))
        {
            return Ok(ChangesRead::Whole);
        }
        let base = heads
            .iter()
            .copied()
            .map(automerge::ChangeHash)
            .collect::<Vec<_>>();
        let Some(changes) = self
            .peer
            .plaintext_changes_since(app, record, &base, maximum)
        else {
            return Ok(ChangesRead::Whole);
        };
        let heads = sorted(&changes.heads);
        Ok(ChangesRead::Changes(DocumentChanges {
            revision: revision_of_sorted(&heads),
            heads,
            bytes: changes.bytes,
        }))
    }
}
