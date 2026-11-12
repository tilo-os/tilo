//! Bounded reads of an admitted document in an owner-hosted workspace.
//!
//! A read is captured from the committed view's live
//! [`crate::StatePeer`], which is never cloned or saved for a
//! reader, after rechecking the reader's current membership and document
//! Read permission. The plaintext API refuses encrypted documents; history
//! reads require an opener for already-held channel keys. Both refuse
//! causally incomplete documents or captures larger than the caller can
//! hold. A reader that already holds the admitted revision is answered from
//! those heads, without capturing the document again.
//!
//! A canonical checkpoint reaches a receiving owner as several signed frames.
//! Between the frame that moves the workspace's coverage and the frame carrying the
//! covered snapshot, the record's admitted checkpoint is older than its
//! coverage. That is a state the owner leaves by itself, not a refusal: the
//! read reports [`DocumentRead::CheckpointPending`] and hands out nothing.
//!
//! A reader that holds an earlier revision this owner handed it (by its
//! heads, [`Holding`]) is answered with only the changes since, from the
//! same document a whole read hands out: the committed view's own, or a
//! sealed record's history opened onto what the reader kept of it
//! ([`KeptRecord`]), so a read opens only the writes sealed since the last.
//! Whatever only a whole read can decide (a base the document does not
//! hold, changes over the limit) is answered whole.
use crate::PeerDocumentReadError;
use crate::owner::NativeCommittedView;
use crate::state::KeptRecord;
use automerge::Automerge;
use web_time::SystemTime;

type HistoryOpener<'a> = &'a mut dyn FnMut(&collab_types::SignedOp) -> Result<Vec<u8>, String>;

#[cfg(test)]
thread_local! {
    static CAPTURES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// How many times a document read captured the document. Tests use this to
/// prove an unchanged revision does not pay that cost.
#[cfg(test)]
pub(crate) fn captures() -> usize {
    CAPTURES.with(std::cell::Cell::get)
}

#[cfg(test)]
pub(crate) fn reset_captures() {
    CAPTURES.with(|count| count.set(0));
}

/// A document as the workspace last admitted it.
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
    pub revision: [u8; 32],
    /// Its heads, sorted.
    pub heads: Vec<[u8; 32]>,
    /// Automerge change bytes. They apply to a document holding every head
    /// of the revision they start from.
    pub bytes: Vec<u8>,
}

/// What a bounded read found.
pub enum DocumentRead {
    /// The reader already holds this revision.
    Unchanged([u8; 32]),
    Captured(CommittedDocument),
    /// Only the changes since the heads the reader holds ([`Holding::heads`]).
    Changes(DocumentChanges),
    /// The workspace is between the frames of one checkpoint: its coverage names
    /// a snapshot that has not arrived yet. Nothing was read; the reader may
    /// ask again once the rest of the checkpoint is admitted.
    CheckpointPending,
}

/// What a reader holds of a record it reads again.
#[derive(Clone, Copy, Default)]
pub struct Holding<'a> {
    /// The revision it read last: answered as unchanged while the record still stands there.
    pub revision: Option<[u8; 32]>,
    /// The sorted heads of a revision an earlier read handed it: a changed record is answered
    /// with only the changes since, when the document holds every one of them.
    pub heads: &'a [[u8; 32]],
}

/// A sealed record's retained history, opened through the owner's already-held channel keys
/// (`open`) onto what this reader opened of it before (`kept`).
pub struct History<'a> {
    pub open: HistoryOpener<'a>,
    pub kept: &'a mut KeptRecord,
}

fn sorted(heads: &[automerge::ChangeHash]) -> Vec<[u8; 32]> {
    let mut heads = heads.iter().map(|head| head.0).collect::<Vec<_>>();
    heads.sort_unstable();
    heads
}

fn revision_of(heads: &[automerge::ChangeHash]) -> [u8; 32] {
    crate::committed_document_revision(heads)
}

/// The revision a record standing at the sorted `heads` reads at, as a read names it.
#[must_use]
pub fn revision_of_heads(heads: &[[u8; 32]]) -> [u8; 32] {
    revision_of_sorted(heads)
}

fn revision_of_sorted(heads: &[[u8; 32]]) -> [u8; 32] {
    revision_of(
        &heads
            .iter()
            .copied()
            .map(automerge::ChangeHash)
            .collect::<Vec<_>>(),
    )
}

/// The changes `document` holds since `heads`, at most `maximum` bytes; `None` when only a whole
/// read answers: no heads, heads not sorted and distinct, a head the document does not hold, or
/// changes over `maximum`.
fn changes_after(document: &Automerge, heads: &[[u8; 32]], maximum: usize) -> Option<Vec<u8>> {
    if heads.is_empty()
        || !heads
            .windows(2)
            .all(|pair| matches!(pair, [first, second] if first < second))
    {
        return None;
    }
    let base = heads
        .iter()
        .copied()
        .map(automerge::ChangeHash)
        .collect::<Vec<_>>();
    if base
        .iter()
        .any(|head| automerge::ReadDoc::get_change_by_hash(document, head).is_none())
    {
        return None;
    }
    crate::state::changes_since(document, &base, maximum).ok()
}

impl NativeCommittedView {
    /// Read `app`'s `record` for the device `reader`.
    pub fn read_document(
        &self,
        reader: [u8; 32],
        app: &str,
        record: &str,
        known_revision: Option<[u8; 32]>,
        maximum: usize,
        now: SystemTime,
    ) -> Result<DocumentRead, String> {
        let holding = Holding {
            revision: known_revision,
            heads: &[],
        };
        self.read_document_inner(reader, (app, record), holding, maximum, now, None)
    }

    /// Read the same admitted record for a reader `holding` what it read before, opening its
    /// retained encrypted `history` when necessary.
    pub fn read_document_with_history(
        &self,
        reader: [u8; 32],
        route: (&str, &str),
        holding: Holding<'_>,
        maximum: usize,
        now: SystemTime,
        history: History<'_>,
    ) -> Result<DocumentRead, String> {
        self.read_document_inner(reader, route, holding, maximum, now, Some(history))
    }

    /// The revision `app`'s `record` stands at for the device `reader`, decided as a read decides
    /// it but handing out nothing: `None` while a checkpoint is pending.
    ///
    /// # Errors
    /// As a read's.
    pub fn document_revision(
        &self,
        reader: [u8; 32],
        (app, record): (&str, &str),
        now: SystemTime,
        history: History<'_>,
    ) -> Result<Option<[u8; 32]>, String> {
        if self.peer.checkpoint_pending(app, record) {
            self.peer.check_document_read(reader, app, record, now)?;
            return Ok(None);
        }
        let History { open, kept } = history;
        let document = self
            .peer
            .open_document(reader, (app, record), now, usize::MAX, Some(open), kept)
            .map_err(|error| refusal(app, record, error))?;
        Ok(Some(revision_of(&document.get_heads())))
    }

    fn read_document_inner(
        &self,
        reader: [u8; 32],
        route: (&str, &str),
        holding: Holding<'_>,
        maximum: usize,
        now: SystemTime,
        history: Option<History<'_>>,
    ) -> Result<DocumentRead, String> {
        let (app, record) = route;
        // Idle apps re-read the revision they already hold. Answer that from
        // the admitted heads. Capturing the document is only for one this
        // caller does not yet have, or for a route those heads cannot vouch
        // for (encrypted, incomplete, or absent).
        if let Some(known) = holding.revision
            && let Some(heads) = self.peer.plaintext_complete_heads(app, record)
        {
            let revision = crate::committed_document_revision(&heads);
            if revision == known {
                self.peer.check_document_read(reader, app, record, now)?;
                return Ok(DocumentRead::Unchanged(revision));
            }
        }
        if self.peer.checkpoint_pending(app, record) {
            // Still answer only a reader who may read the record now.
            self.peer.check_document_read(reader, app, record, now)?;
            return Ok(DocumentRead::CheckpointPending);
        }
        let mut unkept = KeptRecord::default();
        let (open, kept) = match history {
            Some(History { open, kept }) => (Some(open), kept),
            None => (None, &mut unkept),
        };
        let document = self
            .peer
            .open_document(reader, route, now, maximum, open, kept)
            .map_err(|error| refusal(app, record, error))?;
        let heads = sorted(&document.get_heads());
        let revision = revision_of_sorted(&heads);
        if holding.revision == Some(revision) {
            return Ok(DocumentRead::Unchanged(revision));
        }
        if let Some(bytes) = changes_after(document, holding.heads, maximum) {
            return Ok(DocumentRead::Changes(DocumentChanges {
                revision,
                heads,
                bytes,
            }));
        }
        #[cfg(test)]
        CAPTURES.with(|count| count.set(count.get().saturating_add(1)));
        // A record kept opened extends what its last capture made; any other is captured whole.
        let bytes = match kept.capture(maximum) {
            Some(bytes) => bytes,
            None => crate::state::saved_whole(
                self.peer
                    .open_document(
                        reader,
                        route,
                        now,
                        maximum,
                        None,
                        &mut KeptRecord::default(),
                    )
                    .map_err(|error| refusal(app, record, error))?,
                maximum,
            ),
        }
        .map_err(|error| refusal(app, record, error))?;
        Ok(DocumentRead::Captured(CommittedDocument {
            revision,
            heads,
            bytes,
        }))
    }
}

fn refusal(app: &str, record: &str, error: PeerDocumentReadError) -> String {
    match error {
        PeerDocumentReadError::Denied => "the reader may not read that document".into(),
        PeerDocumentReadError::Missing => {
            format!("the workspace does not hold {app} record {record:?}")
        }
        other => other.to_string(),
    }
}
