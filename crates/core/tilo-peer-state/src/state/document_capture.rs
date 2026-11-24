//! An admitted record captured for a reader, from the live peer, within a
//! byte limit.
use super::{AutoCommit, Route, StatePeer, SystemTime, backend::PeerDocument};
use automerge::transaction::Transactable as _;
use automerge::{Automerge, ChangeHash};
use collab_types::{Ed25519PublicKey, OpKind, SealedCompaction, SignedOp};

/// The most bytes one whole-record capture hands out, whatever the caller allows: a reader that
/// keeps the record opened reads it under its own [`StatePeer::open_document`] allowance, which
/// is the caller's.
const MAX_CAPTURE_BYTES: usize = 8 * 1024 * 1024;

/// Opens one sealed operation of an encrypted record into its plaintext,
/// through the embedding owner's channel keys.
pub type OpenSealed<'a> = &'a mut dyn FnMut(&SignedOp) -> Result<Vec<u8>, String>;

/// Why a record was not captured. These are observations; the embedding
/// owner authenticates caller and time.
#[derive(Debug)]
pub enum PeerDocumentReadError {
    /// The reader is no current member of the workspace.
    Membership(crate::state::MembershipError),
    Denied,
    Missing,
    Encrypted,
    Incomplete,
    Capacity,
    Opening(String),
    /// A write this device opened was refused as a received change is: the
    /// refusal names the write, and nothing of it is applied or projected.
    Refused(Box<crate::ChangeRefused>),
}
impl std::fmt::Display for PeerDocumentReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(refused) => write!(f, "document read refusal: {refused}"),
            other => write!(f, "document read refusal: {other:?}"),
        }
    }
}
impl std::error::Error for PeerDocumentReadError {}

/// Why a sealed write this device opened cannot be applied or projected.
pub(crate) enum OpenedRefusal {
    /// The workspace's rules refuse the write, as they refuse a change it
    /// received in the clear.
    Refused(Box<crate::ChangeRefused>),
    /// This device cannot decide the write here and now.
    CannotDecide(String),
}

impl std::fmt::Display for OpenedRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(refused) => f.write_str(&refused.reason),
            Self::CannotDecide(reason) => f.write_str(reason),
        }
    }
}

/// Complete history bytes and heads from one currently permitted read.
/// It carries data only, with no owner credential or mutable peer.
pub struct PeerDocumentCapture {
    heads: Vec<ChangeHash>,
    bytes: Vec<u8>,
    /// The sealed writes the read left out because the workspace's rules
    /// refuse them, or because they build on one that is.
    refused: Vec<super::sealed_compaction::UnmergedWrite>,
}

impl StatePeer {
    /// A digest of `record` of `app` as this state holds it now: any write
    /// admitted to it since changes it, in the clear or sealed, and reading it
    /// saves and opens nothing. `None` for a record not held. Equal revisions
    /// at two moments mean the record took no write between them.
    #[must_use]
    pub fn document_revision(&self, app: &str, record: &str) -> Option<[u8; 32]> {
        let document = self.core.documents.get(&Route {
            app: app.into(),
            record: record.into(),
        })?;
        let mut hasher = blake3::Hasher::new_derive_key("tilo record revision v1");
        for head in document.doc.base_heads() {
            hasher.update(&head.0);
        }
        hasher.update(
            &u64::try_from(document.sealed.len())
                .unwrap_or(u64::MAX)
                .to_le_bytes(),
        );
        hasher.update(
            &u64::try_from(document.sealed.bytes())
                .unwrap_or(u64::MAX)
                .to_le_bytes(),
        );
        if let Some(last) = document.sealed.last() {
            hasher.update(blake3::hash(&last.payload).as_bytes());
        }
        Some(*hasher.finalize().as_bytes())
    }
}

/// Stable revision of the exact committed graph, shared by native owners.
pub fn committed_document_revision(heads: &[ChangeHash]) -> [u8; 32] {
    let mut heads = heads.iter().map(|head| head.0).collect::<Vec<_>>();
    heads.sort_unstable();
    let mut hasher = blake3::Hasher::new_derive_key("tilo committed document revision v1");
    for head in heads {
        hasher.update(&head);
    }
    *hasher.finalize().as_bytes()
}

impl PeerDocumentCapture {
    pub fn revision(&self) -> [u8; 32] {
        committed_document_revision(&self.heads)
    }
    pub fn heads(&self) -> &[ChangeHash] {
        &self.heads
    }

    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// The sealed writes this read left out because the workspace's rules
    /// refuse them ([`super::sealed_compaction::UnmergedReason::Refused`]), or
    /// because they build on one that is
    /// ([`super::sealed_compaction::UnmergedReason::BuiltOnRefused`]): the
    /// record reads without them.
    #[must_use]
    pub fn refused(&self) -> &[super::sealed_compaction::UnmergedWrite] {
        &self.refused
    }

    fn from_document(document: &Automerge, maximum: usize) -> Result<Self, PeerDocumentReadError> {
        Ok(Self {
            heads: document.get_heads(),
            bytes: saved_whole(document, maximum)?,
            refused: Vec::new(),
        })
    }
}

/// `document` whole, as a reader loads it: saved compressed, at most `maximum` bytes. A record's
/// history handed out change by change loads by applying each change in turn, several times
/// slower than a saved document (whose columns load at once) and many times its size, and
/// every reader of a whole record (an app opening it, the person's mentions, the page keeping it
/// for whoever opens it) loads it. The save keeps every change and who made it.
///
/// # Errors
/// The document misses a change one it holds depends on, or its save exceeds `maximum`.
pub fn saved_whole(document: &Automerge, maximum: usize) -> Result<Vec<u8>, PeerDocumentReadError> {
    if !automerge::ReadDoc::get_missing_deps(document, &[]).is_empty() {
        return Err(PeerDocumentReadError::Incomplete);
    }
    let bytes = document.save();
    if bytes.len() > maximum {
        return Err(PeerDocumentReadError::Capacity);
    }
    Ok(bytes)
}

/// The changes of `document` a holder of `since` lacks, as raw change chunks: every change for an
/// empty `since`, whose original writers they all keep, and which load as a complete document
/// without allocating an unbounded compressed save first. At most `maximum` bytes.
///
/// # Errors
/// The document misses a change one it holds depends on, or the changes exceed `maximum`.
pub fn changes_since(
    document: &Automerge,
    since: &[ChangeHash],
    maximum: usize,
) -> Result<Vec<u8>, PeerDocumentReadError> {
    if !automerge::ReadDoc::get_missing_deps(document, &[]).is_empty() {
        return Err(PeerDocumentReadError::Incomplete);
    }
    let changes = document.get_changes(since);
    if changes.is_empty() && since.is_empty() {
        // An empty graph still needs a canonical header: a zero-byte
        // stream is not a valid broker capture. This is library-format
        // data, never cached record state or an authority fallback.
        static EMPTY_DOCUMENT: std::sync::LazyLock<Vec<u8>> =
            std::sync::LazyLock::new(|| AutoCommit::new().save());
        if EMPTY_DOCUMENT.len() > maximum {
            return Err(PeerDocumentReadError::Capacity);
        }
        return Ok(EMPTY_DOCUMENT.clone());
    }
    // Count the chunks before allocating the output.
    let size = changes.iter().try_fold(0usize, |size, change| {
        size.checked_add(change.raw_bytes().len())
            .filter(|size| *size <= maximum)
            .ok_or(PeerDocumentReadError::Capacity)
    })?;
    let mut bytes = Vec::with_capacity(size);
    for change in changes {
        bytes.extend_from_slice(change.raw_bytes());
    }
    Ok(bytes)
}

/// A sealed record as one reader last opened it, kept for its next read: that read opens only the
/// writes sealed since, onto what this holds, unless the record's history was compacted or
/// replaced meanwhile. A reader that keeps none opens the whole history on every read.
///
/// What it holds is plaintext the reader's keys opened, so it lives with that reader, in its
/// memory, and is never stored here. A read the workspace refuses (the reader was removed, may no
/// longer read the record, or the history no longer covers every write) forgets it: what is kept
/// never answers a reader the workspace would not answer. A write that does not open with the
/// keys the reader holds now ends what it holds until they do.
#[derive(Default)]
pub struct KeptDocument(Option<Opened>);

impl KeptDocument {
    /// Whether a record is kept opened.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.0.is_some()
    }

    /// The bytes kept: what was opened of the history, and the last whole capture of it. The
    /// opened document takes several times as much in memory again, in proportion.
    #[must_use]
    pub fn size(&self) -> usize {
        self.0.as_ref().map_or(0, |opened| {
            opened
                .size
                .saturating_add(opened.captured.as_ref().map_or(0, |(bytes, _)| bytes.len()))
        })
    }

    /// Let go of what is kept.
    pub fn forget(&mut self) {
        self.0 = None;
    }

    /// The sealed writes the reads of what is kept left out because the workspace's rules refuse
    /// them, or because they build on one that is: the record reads without them, and every
    /// reader that opens them leaves out the same.
    #[must_use]
    pub fn refused(&self) -> &[super::sealed_compaction::UnmergedWrite] {
        self.0.as_ref().map_or(&[], |opened| &opened.refused)
    }

    /// The kept record captured whole, at most `maximum` bytes: the bytes the last capture made
    /// (the document saved whole) with the changes since appended, rather than the document
    /// saved again. `None` when nothing is kept.
    ///
    /// # Errors
    ///
    /// The document exceeds `maximum`, or it does not save.
    pub fn capture(&mut self, maximum: usize) -> Option<Result<Vec<u8>, PeerDocumentReadError>> {
        let opened = self.0.as_mut()?;
        let document = opened.document.committed_document();
        let heads = document.get_heads();
        let bytes = match opened.captured.take() {
            Some((mut bytes, at)) if !bytes.is_empty() => {
                if at != heads {
                    for change in document.get_changes(&at) {
                        bytes.extend_from_slice(change.raw_bytes());
                    }
                }
                bytes
            }
            _ => match saved_whole(document, usize::MAX) {
                Ok(bytes) => bytes,
                Err(error) => return Some(Err(error)),
            },
        };
        let captured = if bytes.len() > maximum {
            Err(PeerDocumentReadError::Capacity)
        } else {
            // Whoever loads these bytes next on this thread (the window showing the record,
            // the activity reading its list of records) takes a copy of the document they
            // were saved from instead of loading it again.
            tilo_automerge::shared::hold(&bytes, &opened.document);
            Ok(bytes.clone())
        };
        opened.captured = Some((bytes, heads));
        Some(captured)
    }
}

struct Opened {
    /// The heads of the cleartext document the writes were merged onto.
    base: Vec<ChangeHash>,
    /// How many writes of the history were opened.
    count: usize,
    /// The signature of the last of them.
    last: Option<collab_types::Signature>,
    /// The bytes opened, counted against a read's maximum.
    size: usize,
    /// Segments of writes not yet complete.
    transfers: crate::document_frame::Transfers,
    document: AutoCommit,
    /// The bytes the last whole capture of `document` handed out, and the heads they stand at.
    captured: Option<(Vec<u8>, Vec<ChangeHash>)>,
    /// Writes opened and read without, each stuck behind a change its writer never sent, by their
    /// place in the history: tried again on every read, so one whose base turns up after all is
    /// taken in.
    stuck: Vec<(usize, collab_types::DocumentMutation)>,
    /// Writes opened by a read that stopped when its turn was over (`crate::opening`) and that
    /// wait for a change a write not opened yet may carry: merged with the writes the next read
    /// opens.
    waiting: Vec<(usize, collab_types::DocumentMutation)>,
    /// The changes of the writes the workspace's rules refuse, and of those built on them: a
    /// write opened later that builds on one is left out with it.
    refusing: super::sealed_compaction::RefusedChanges,
    /// The writes left out for that.
    refused: Vec<super::sealed_compaction::UnmergedWrite>,
}

impl Opened {
    /// The record opened, without a write whose segments are not all here yet, as it read the
    /// moment before that write began to arrive; its segments are kept for the next read, which
    /// opens only what was admitted since. A write whose writer stopped sending it (it ended,
    /// or the workspace refused a segment) never completes: refused while one was arriving,
    /// the record did not read for any reader, for good. Whatever builds on such a write waits
    /// for it as a write whose base is still on its way does.
    fn read(&self) -> &Automerge {
        self.document.committed_document()
    }

    /// Whether `document`'s history continues the one this opened: its cleartext document is
    /// the one these writes were merged onto, and the sealed writes opened are still the ones
    /// the history starts with.
    fn continues(&self, document: &PeerDocument) -> bool {
        document.doc.committed_document().get_heads() == self.base
            && document.sealed.len() >= self.count
            && self.count.checked_sub(1).is_none_or(|last| {
                document.sealed.iter().nth(last).map(|op| &op.signature) == self.last.as_ref()
            })
    }
}

impl StatePeer {
    /// Decide a sealed `write` this device opened as `mutation` before it is
    /// applied or projected: the checks admission runs on the same write in
    /// the clear, including the invocation a change made for a caller keeps,
    /// as of when the write was written, so every device that opens it, at any
    /// time, decides it alike. The claims its envelope made to devices without
    /// the key must name exactly the changes it carries, as they say them. A
    /// refusal names the write as a received change's is named, so the reader
    /// tells its author.
    pub(crate) fn check_opened_write(
        &self,
        (app, record): (&str, &str),
        write: &SignedOp,
        mutation: &collab_types::DocumentMutation,
    ) -> Result<(), OpenedRefusal> {
        // A checkpoint is no write of its own and nothing a reader takes on its writer's word:
        // it is read against the writes it stands in for, each decided here as itself
        // ([`super::checkpoints::read_checkpoint`]).
        if write.kind == OpKind::Compaction {
            return Err(OpenedRefusal::CannotDecide(
                "a checkpoint is read against the writes it stands in for, never decided as a write"
                    .into(),
            ));
        }
        // What a device without the key decided the write by must be what it carries: a write
        // that claims harmless actions to them and carries others is refused here.
        if !write.claims.is_empty() {
            collab_types::check_opened_mutation(write, mutation)
                .map_err(|refusal| self.refused_opened(app, record, write, &refusal))?;
        }
        let mut opened = write.clone();
        // A cleartext op names each change's action in the change itself; the
        // sealed envelope's claims were what a device without the key decided.
        opened.claims.clear();
        opened.encryption = None;
        // A write small enough to carry as itself is decided whole, as
        // admission decides the same write in the clear.
        if let Ok(payload) = mutation.encode() {
            opened.payload = payload;
            return self.decide_opened(app, record, write, (&opened, None));
        }
        // A write past what one op carries was written as segments, none of
        // which carries its changes whole: it is decided by the changes it was
        // assembled into, as admission decides them, grants and callers alike.
        match mutation {
            collab_types::DocumentMutation::Snapshot(_) => {
                // A whole record is decided by who wrote it, never by what it holds: what an
                // empty one carries decides it as this one.
                opened.payload = collab_types::DocumentMutation::Snapshot(Vec::new())
                    .encode()
                    .map_err(|error| OpenedRefusal::CannotDecide(error.to_string()))?;
                self.decide_opened(app, record, write, (&opened, None))
            }
            collab_types::DocumentMutation::Incremental(bytes) => {
                let changes = tilo_automerge_isolate::parse_changes(bytes).map_err(|_| {
                    self.refused_opened(
                        app,
                        record,
                        write,
                        &collab_types::ReceiptRefusal::Unclaimed,
                    )
                })?;
                opened.payload = Vec::new();
                self.decide_opened(app, record, write, (&opened, Some(&changes)))
            }
        }
    }

    /// `write` refused by the workspace's rules as `refusal`, named as a received change's
    /// refusal is.
    fn refused_opened(
        &self,
        app: &str,
        record: &str,
        write: &SignedOp,
        refusal: &collab_types::ReceiptRefusal,
    ) -> OpenedRefusal {
        OpenedRefusal::Refused(Box::new(crate::ChangeRefused::of(
            self.core.workspace.clone(),
            self.core.workspace.clone(),
            app,
            record,
            write,
            refusal,
            format!("the opened write is refused: {refusal}"),
        )))
    }

    /// Decide the opened change `opened` of `write` as admission decides a
    /// change it received in the clear: by the changes `opened` carries, or by
    /// those it was assembled into when it is too large to carry them.
    fn decide_opened(
        &self,
        app: &str,
        record: &str,
        write: &SignedOp,
        (opened, changes): (&SignedOp, Option<&[automerge::Change]>),
    ) -> Result<(), OpenedRefusal> {
        match self.authorize_document_as(
            write.author.as_bytes(),
            app,
            record,
            write,
            (opened, changes),
            write.timestamp,
            true,
        ) {
            Ok(super::authority::Authorized::Admit { .. }) => Ok(()),
            Ok(super::authority::Authorized::Hold(waiting)) => Err(OpenedRefusal::CannotDecide(
                format!("the opened write waits for {waiting}"),
            )),
            Err(super::authority::AuthorizeRefusal::Rules(refusal, said)) => {
                Err(OpenedRefusal::Refused(Box::new(crate::ChangeRefused::of(
                    self.core.workspace.clone(),
                    self.core.workspace.clone(),
                    app,
                    record,
                    if changes.is_some() { write } else { opened },
                    &refusal,
                    said,
                ))))
            }
            Err(super::authority::AuthorizeRefusal::Denied(reason)) => {
                Err(OpenedRefusal::CannotDecide(reason.to_string()))
            }
        }
    }
}

impl StatePeer {
    /// Capture `app`'s `record` for the device `reader` under its current
    /// membership and Read permission, at most `maximum` bytes of it. An
    /// encrypted record is refused without `open`; with it, its retained
    /// sealed history is opened through the embedding owner's already-held
    /// channel keys, once that history is known to cover every admitted
    /// write.
    pub fn capture_document(
        &self,
        reader: [u8; 32],
        route: (&str, &str),
        now: SystemTime,
        maximum: usize,
        open: Option<OpenSealed<'_>>,
    ) -> Result<PeerDocumentCapture, PeerDocumentReadError> {
        let maximum = maximum.min(MAX_CAPTURE_BYTES);
        let mut kept = KeptDocument::default();
        let document = self.open_document(reader, route, now, maximum, open, &mut kept)?;
        let mut capture = PeerDocumentCapture::from_document(document, maximum)?;
        capture.refused = kept.refused().to_vec();
        Ok(capture)
    }

    /// Whether the device `reader` may read the resource at `route` now and, for one read through
    /// its keys (`sealed`), whether the sealed writes held cover every admitted write: what
    /// [`Self::open_document`] decides before it opens anything, decided without opening it.
    ///
    /// # Errors
    /// As [`Self::open_document`] refuses before it opens: the reader may not read the resource,
    /// it is not held, or its history does not cover every admitted write.
    pub fn document_opens(
        &self,
        reader: [u8; 32],
        route: (&str, &str),
        now: SystemTime,
        sealed: bool,
    ) -> Result<(), PeerDocumentReadError> {
        self.readable(reader, route, now, sealed).map(drop)
    }

    /// `app`'s `record` for the device `reader`, under its current membership and Read
    /// permission: the document itself when it is held in the clear; an encrypted record's
    /// retained sealed history opened through the embedding owner's channel keys (`open`) onto
    /// what `kept` opened of it before, once that history is known to cover every admitted write.
    /// What it opens is at most `maximum` bytes: a sealed record reads in the workspace its caller
    /// allows, as one held in the clear does. Its sealed history is bounded where it is admitted
    /// (`MAX_SEALED_BYTES` of writes after a compaction set of at most
    /// `MAX_COMPACTION_SET_BYTES`), so the workspace a caller allows covers a record whatever its history
    /// holds; a fixed bound of the capture's own refused a late joiner's record for good while
    /// every caller allowed several times its size.
    ///
    /// # Errors
    /// The reader may not read the record, it is not held, it is encrypted and nothing opens it,
    /// its history does not cover every admitted write or none of it opens, or it exceeds
    /// `maximum`.
    pub fn open_document<'a>(
        &'a self,
        reader: [u8; 32],
        (app, record): (&str, &str),
        now: SystemTime,
        maximum: usize,
        open: Option<OpenSealed<'_>>,
        kept: &'a mut KeptDocument,
    ) -> Result<&'a Automerge, PeerDocumentReadError> {
        let document = match self.readable(reader, (app, record), now, open.is_some()) {
            Ok(document) => document,
            // A reader who may no longer read the resource keeps nothing of it. A history still
            // arriving (the workspace's admissions not all here yet) keeps what opened: the next
            // read goes on from it once the history continues it, where forgetting it made a
            // page catching up on a large resource open every part from its first write again
            // after each write it took in.
            Err(
                error @ (PeerDocumentReadError::Membership(_)
                | PeerDocumentReadError::Denied
                | PeerDocumentReadError::Missing),
            ) => {
                kept.forget();
                return Err(error);
            }
            Err(error) => return Err(error),
        };
        match (document.encryption_required, open) {
            (true, Some(open)) => open_history(self, (app, record), document, open, maximum, kept),
            (true, None) => Err(PeerDocumentReadError::Encrypted),
            (false, _) => {
                kept.forget();
                Ok(document.doc.committed_document())
            }
        }
    }

    /// `app`'s `record` when the device `reader` may read it now: a current member with Read
    /// permission on it, and for a sealed one read through keys (`opening`), a history that
    /// covers every admitted write.
    fn readable(
        &self,
        reader: [u8; 32],
        (app, record): (&str, &str),
        now: SystemTime,
        opening: bool,
    ) -> Result<&PeerDocument, PeerDocumentReadError> {
        let member = self
            .permissions(reader, now)
            .map_err(PeerDocumentReadError::Membership)?;
        if !super::backend::access::document_read(
            &member.authority(),
            &self.core.workspace,
            (app, record),
            now,
        ) {
            return Err(PeerDocumentReadError::Denied);
        }
        let route = Route {
            app: app.into(),
            record: record.into(),
        };
        let document = self
            .core
            .documents
            .get(&route)
            .ok_or(PeerDocumentReadError::Missing)?;
        if document.encryption_required && opening {
            self.check_sealed_coverage(&route, document)?;
        }
        Ok(document)
    }

    fn check_sealed_coverage(
        &self,
        route: &Route,
        document: &PeerDocument,
    ) -> Result<(), PeerDocumentReadError> {
        if document.sealed.is_empty() {
            return Err(PeerDocumentReadError::Encrypted);
        }
        if !self.core.exact_selected {
            return Err(PeerDocumentReadError::Incomplete);
        }
        let held: std::collections::HashSet<_> = document
            .sealed
            .iter()
            .map(|op| (*op.author.as_bytes(), op.seq))
            .collect();
        let compaction = document
            .sealed
            .first()
            .filter(|op| op.kind == OpKind::Compaction)
            .map(SealedCompaction::parse)
            .transpose()
            .map_err(|error| PeerDocumentReadError::Opening(error.to_string()))?;
        let covered = |author: [u8; 32], sequence| {
            document
                .covered
                .get(&hex::encode(author))
                .is_some_and(|floor| *floor >= sequence)
                || held.contains(&(author, sequence))
                || compaction.as_ref().is_some_and(|checkpoint| {
                    checkpoint.covers.iter().any(|(key, floor)| {
                        *key == Ed25519PublicKey::from_bytes(author) && *floor >= sequence
                    })
                })
        };
        if self.core.exact_admissions.iter().any(|(key, _)| {
            key.app == route.app && key.record == route.record && !covered(key.author, key.sequence)
        }) || document
            .channel
            .last_seq_per_author
            .iter()
            .any(|(author, floor)| !covered(*author.as_bytes(), *floor))
        {
            return Err(PeerDocumentReadError::Incomplete);
        }
        Ok(())
    }
}

/// `document`'s history as `kept` opened it so far, or fresh from its cleartext part, which counts
/// toward `maximum` and is refused when it is already past it.
fn opened_history(
    document: &PeerDocument,
    kept: &mut KeptDocument,
    maximum: usize,
) -> Result<Opened, PeerDocumentReadError> {
    let layout = document.doc.stored_layout();
    let base = layout.base_len.saturating_add(layout.since_len);
    if base > maximum {
        return Err(PeerDocumentReadError::Capacity);
    }
    Ok(
        match kept.0.take().filter(|opened| opened.continues(document)) {
            Some(opened) => opened,
            None => Opened {
                base: document.doc.committed_document().get_heads(),
                count: 0,
                last: None,
                size: base,
                transfers: crate::document_frame::Transfers::default(),
                captured: None,
                stuck: Vec::new(),
                waiting: Vec::new(),
                refusing: super::sealed_compaction::RefusedChanges::default(),
                refused: Vec::new(),
                // The document's cleartext part as it stands, which is what a sealed history opens
                // onto ([`super::sealed_compaction::open_sealed_after`]): a compaction folds the
                // writes it opened into it and leaves the checkpoint it was covered by behind, so
                // opening onto that checkpoint read a document as it was before its compaction.
                document: (*document.doc).clone(),
            },
        },
    )
}

/// Merge the writes of a look within a turn, answering whether the turn is over.
fn merge_a_look(
    opened: &mut Opened,
    writes: &mut Vec<(usize, collab_types::DocumentMutation)>,
    signers: &super::sealed_compaction::Signers<'_>,
) -> Result<bool, PeerDocumentReadError> {
    let (merged, waiting) = merge_some(
        std::mem::take(&mut opened.document),
        std::mem::take(writes),
        signers,
        (&mut opened.refusing, &mut opened.refused),
    )?;
    opened.document = merged;
    *writes = waiting;
    Ok(crate::opening::due())
}

/// `document`'s sealed history opened onto its cleartext document, continuing what `kept` opened
/// of it before. A history that does not open leaves nothing kept; one that waits for the rest
/// of a write's segments keeps what opened.
fn open_history<'a>(
    peer: &StatePeer,
    (app, record): (&str, &str),
    document: &PeerDocument,
    open: OpenSealed<'_>,
    maximum: usize,
    kept: &'a mut KeptDocument,
) -> Result<&'a Automerge, PeerDocumentReadError> {
    // The cleartext document the history opens onto counts toward `maximum`: one already past it
    // is refused before anything of the history is opened.
    let mut opened = opened_history(document, kept, maximum)?;
    // Admission order may differ from causal order on a keyless replica. A
    // write whose sequences the record already holds is folded away: the
    // record cannot carry both changes of one writer's sequence, and the
    // capture carries the change that reached it first. A write stuck behind
    // a change its own writer never sent is read without: no device will ever
    // send what it waits for, and refusing the record for it refused every
    // reader the whole record for good.
    let signed: Vec<super::unsent_bases::Signed> = document
        .sealed
        .iter()
        .map(|op| super::unsent_bases::Signed {
            author: *op.author.as_bytes(),
            seq: op.seq,
        })
        .collect();
    let signers = super::sealed_compaction::Signers {
        of: &|index| signed.get(index).copied(),
        held: &|| super::unsent_bases::HeldThrough::of(document),
    };
    // Within a turn (`crate::opening`) the writes merge a few at a time, and the read stops once
    // the turn is over: what it opened is kept, with the writes still waiting for a change a later
    // write may carry, and the next read goes on from there.
    let in_turns = crate::opening::in_a_turn();
    let mut writes = std::mem::take(&mut opened.waiting);
    let mut fresh = 0_usize;
    let mut ops = document
        .sealed
        .iter()
        .enumerate()
        .skip(opened.count)
        .peekable();
    // A write too large for one frame was written as segments, each a sealed write of its own:
    // it is read once all of them are held.
    while let Some((index, op)) = ops.next() {
        let plaintext = match open(op) {
            Ok(plaintext) => plaintext,
            // A write whose key has not reached this reader yet is read without, with every write
            // after it, until the key is here: what opened before it still reads, as the reader
            // read it a moment ago. A record none of whose writes open is still arriving.
            Err(_) if index > 0 && opened.transfers.is_empty() => break,
            Err(why) => return Err(PeerDocumentReadError::Opening(why)),
        };
        // A write of an action whose writes to other records are not all here is read without,
        // with every write after it, until they are: the records change together.
        if peer
            .sealed_action_waits((app, record), op, &plaintext)
            .map_err(PeerDocumentReadError::Opening)?
            .is_some()
        {
            break;
        }
        opened.size = opened.size.saturating_add(plaintext.len());
        if opened.size > maximum {
            return Err(PeerDocumentReadError::Capacity);
        }
        if let Some(mutation) = opened
            .transfers
            .take(*op.author.as_bytes(), &plaintext)
            .map_err(PeerDocumentReadError::Opening)?
        {
            // A write is decided as admission decides the same write in the
            // clear before any of it is merged into what the reader reads. One
            // the workspace's rules refuse is read without, with every write
            // built on it, and named; one this device cannot decide yet is
            // still arriving. A checkpoint is read against the writes it stands in for: what
            // they merge to, without those the rules refuse; one whose writes are not here and
            // that not every device with the record's keys checked waits for them.
            if op.kind == OpKind::Compaction {
                let read = super::checkpoints::read_checkpoint(
                    document,
                    &mutation,
                    &mut *open,
                    &mut |write, mutation| peer.check_opened_write((app, record), write, mutation),
                    &|checkpoint| peer.checkpoint_settled((app, record), checkpoint),
                )
                .map_err(|refusal| match refusal {
                    super::checkpoints::CheckpointRefusal::Waits => {
                        PeerDocumentReadError::Incomplete
                    }
                    super::checkpoints::CheckpointRefusal::CannotDecide(reason) => {
                        PeerDocumentReadError::Opening(reason)
                    }
                })?;
                read.refuse_in(&mut opened.refusing);
                opened.refused.extend(read.refused.iter().cloned());
                writes.push((index, read.instead.unwrap_or(mutation)));
                fresh = fresh.saturating_add(1);
                opened.count = index.saturating_add(1);
                opened.last = Some(op.signature);
                continue;
            }
            match peer.check_opened_write((app, record), op, &mutation) {
                Ok(()) => {
                    writes.push((index, mutation));
                    fresh = fresh.saturating_add(1);
                }
                Err(OpenedRefusal::Refused(refused)) => {
                    opened.refusing.refuse(&mutation);
                    opened
                        .refused
                        .push(super::sealed_compaction::UnmergedWrite {
                            author: *op.author.as_bytes(),
                            seq: op.seq,
                            reason: super::sealed_compaction::UnmergedReason::Refused {
                                refusal: refused.reason,
                            },
                        });
                }
                Err(OpenedRefusal::CannotDecide(reason)) => {
                    return Err(PeerDocumentReadError::Opening(reason));
                }
            }
        }
        opened.count = index.saturating_add(1);
        opened.last = Some(op.signature);
        if in_turns && fresh >= WRITES_PER_LOOK && ops.peek().is_some() {
            fresh = 0;
            if merge_a_look(&mut opened, &mut writes, &signers)? {
                opened.waiting = writes;
                kept.0 = Some(opened);
                crate::opening::stopped();
                return Err(PeerDocumentReadError::Incomplete);
            }
        }
    }
    opened.merge_last(writes, &signers)?;
    Ok(kept.0.insert(opened).read())
}

impl Opened {
    /// Merge `writes`, the last of the history, and the writes read without before, over the
    /// document.
    ///
    /// # Errors
    /// A write does not read, or one waits for a change the history does not hold.
    fn merge_last(
        &mut self,
        writes: Vec<(usize, collab_types::DocumentMutation)>,
        signers: &super::sealed_compaction::Signers<'_>,
    ) -> Result<(), PeerDocumentReadError> {
        let (mut merged, left) = super::sealed_compaction::merge_writes(
            std::mem::take(&mut self.document),
            writes,
            super::sealed_compaction::LeftOut::Named,
            Some(signers),
            &mut self.refusing,
        )
        .map_err(PeerDocumentReadError::Opening)?;
        // A write read without before stays so until what it builds on is here: only its arrival
        // changes that, and the merge tells it. One built on a refused write is read without for
        // good.
        let (built, stuck_before) = self.refusing.left_out(std::mem::take(&mut self.stuck));
        name_refused(&mut self.refused, built, signers);
        let mut stuck = Vec::new();
        for (index, mutation) in stuck_before {
            match super::sealed_compaction::merge_write(&mut merged, &mutation) {
                super::sealed_compaction::WriteMerge::Waiting => stuck.push((index, mutation)),
                super::sealed_compaction::WriteMerge::Applied
                | super::sealed_compaction::WriteMerge::AlreadyHeld { .. } => {}
                super::sealed_compaction::WriteMerge::Refused(why)
                | super::sealed_compaction::WriteMerge::Device(why) => {
                    return Err(PeerDocumentReadError::Opening(why));
                }
            }
        }
        self.stuck = stuck;
        for (index, reason, mutation) in left {
            match reason {
                super::sealed_compaction::UnmergedReason::AlreadyHeld { .. } => {}
                reason @ (super::sealed_compaction::UnmergedReason::Refused { .. }
                | super::sealed_compaction::UnmergedReason::BuiltOnRefused { .. }) => {
                    name_refused(&mut self.refused, vec![(index, reason, mutation)], signers);
                }
                super::sealed_compaction::UnmergedReason::BehindUnsent { .. } => {
                    self.stuck.push((index, mutation));
                }
                super::sealed_compaction::UnmergedReason::MissingDependencies { .. }
                | super::sealed_compaction::UnmergedReason::WaitsForAction { .. } => {
                    return Err(PeerDocumentReadError::Incomplete);
                }
                super::sealed_compaction::UnmergedReason::Unreadable(why) => {
                    return Err(PeerDocumentReadError::Opening(why));
                }
            }
        }
        self.document = merged;
        Ok(())
    }
}

/// Writes a read within a turn merges between looks at whether the turn is over. Each: merging a
/// write into a large document costs milliseconds already, and a look costs nothing beside it.
pub(super) const WRITES_PER_LOOK: usize = 1;

/// Merge `writes`, some of a history whose later writes are still to open, over `document`: the
/// document, and the writes that wait for a change not in it yet, which a later write may carry.
/// A write whose sequences the document holds is folded away, as at the end of a read.
///
/// # Errors
/// A write does not read, or this device could not decide one.
fn merge_some(
    document: AutoCommit,
    writes: Vec<(usize, collab_types::DocumentMutation)>,
    signers: &super::sealed_compaction::Signers<'_>,
    (refusing, refused): (
        &mut super::sealed_compaction::RefusedChanges,
        &mut Vec<super::sealed_compaction::UnmergedWrite>,
    ),
) -> Result<(AutoCommit, Vec<(usize, collab_types::DocumentMutation)>), PeerDocumentReadError> {
    let (merged, left) = super::sealed_compaction::merge_writes(
        document,
        writes,
        super::sealed_compaction::LeftOut::Named,
        Some(signers),
        refusing,
    )
    .map_err(PeerDocumentReadError::Opening)?;
    let mut waiting = Vec::new();
    for (index, reason, mutation) in left {
        match reason {
            super::sealed_compaction::UnmergedReason::AlreadyHeld { .. } => {}
            reason @ (super::sealed_compaction::UnmergedReason::Refused { .. }
            | super::sealed_compaction::UnmergedReason::BuiltOnRefused { .. }) => {
                name_refused(refused, vec![(index, reason, mutation)], signers);
            }
            super::sealed_compaction::UnmergedReason::BehindUnsent { .. }
            | super::sealed_compaction::UnmergedReason::MissingDependencies { .. }
            | super::sealed_compaction::UnmergedReason::WaitsForAction { .. } => {
                waiting.push((index, mutation));
            }
            super::sealed_compaction::UnmergedReason::Unreadable(why) => {
                return Err(PeerDocumentReadError::Opening(why));
            }
        }
    }
    Ok((merged, waiting))
}

/// Name `left`, writes a read leaves out for the workspace's rules, in `refused`.
fn name_refused(
    refused: &mut Vec<super::sealed_compaction::UnmergedWrite>,
    left: Vec<super::sealed_compaction::LeftWrite>,
    signers: &super::sealed_compaction::Signers<'_>,
) {
    for (index, reason, _) in left {
        if let Some(signed) = (signers.of)(index) {
            refused.push(super::sealed_compaction::UnmergedWrite {
                author: signed.author,
                seq: signed.seq,
                reason,
            });
        }
    }
}
