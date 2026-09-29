//! Bounded reads of an admitted document in an owner-hosted room.
//!
//! The committed view's live [`tilo_room::StatePeer`] is never cloned
//! or saved for a reader. A read restores the room's durable bytes into a
//! [`tilo_room::BoundedStatePeer`] under explicit limits, rechecks the
//! reader's current membership and document Read permission there, and
//! refuses a document that is encrypted, causally incomplete or larger than
//! the caller can hold.
use crate::NativeCommittedView;
use std::time::SystemTime;
use tilo_room::{BoundedStatePeer, PeerDocumentReadError, PeerLimits};
use tilo_user_identity::RevocationList;

/// A document as the room last admitted it.
pub struct CommittedDocument {
    /// Names exactly these heads; equal revisions are equal documents.
    pub revision: [u8; 32],
    pub bytes: Vec<u8>,
}

/// What a bounded read found.
pub enum DocumentRead {
    /// The reader already holds this revision.
    Unchanged([u8; 32]),
    Captured(CommittedDocument),
}

fn revision_of(heads: &[automerge::ChangeHash]) -> [u8; 32] {
    let mut heads = heads.iter().map(|head| head.0).collect::<Vec<_>>();
    heads.sort_unstable();
    let mut hasher = blake3::Hasher::new_derive_key("tilo committed document revision v1");
    for head in heads {
        hasher.update(&head);
    }
    *hasher.finalize().as_bytes()
}

impl NativeCommittedView {
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
        let limits = PeerLimits::default();
        let restored = BoundedStatePeer::restore(
            &self.record.peer,
            &self.record.room.to_string(),
            &self.record.governor,
            std::slice::from_ref(&self.record.device),
            &RevocationList::new(),
            now,
            &limits,
        )
        .map_err(|error| error.to_string())?;
        let read = restored.read();
        let document =
            read.document_read(reader, app, record, now)
                .map_err(|error| match error {
                    PeerDocumentReadError::Denied => "the reader may not read that document".into(),
                    PeerDocumentReadError::Missing => "the room does not hold that document".into(),
                    other => other.to_string(),
                })?;
        let revision = revision_of(document.heads());
        if known_revision == Some(revision) {
            return Ok(DocumentRead::Unchanged(revision));
        }
        let bytes = document.document().clone().save();
        if bytes.len() > maximum {
            return Err("the document is larger than the reader can hold".into());
        }
        Ok(DocumentRead::Captured(CommittedDocument {
            revision,
            bytes,
        }))
    }
}
