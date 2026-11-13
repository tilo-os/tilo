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
}
impl std::fmt::Display for PeerDocumentReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "document read refusal: {self:?}")
    }
}
impl std::error::Error for PeerDocumentReadError {}

/// Complete history bytes and heads from one currently permitted read.
/// It carries data only, with no owner credential or mutable peer.
pub struct PeerDocumentCapture {
    heads: Vec<ChangeHash>,
    bytes: Vec<u8>,
}

impl StatePeer {
    /// A digest of `record` of `app` as this state holds it now: any write
    /// admitted to it since changes it, in the clear or sealed, and reading it
    /// saves and opens nothing. `None` for a record not held. Equal revisions
    /// at two moments mean the record took no write between them.
    #[must_use]
    pub fn record_revision(&self, app: &str, record: &str) -> Option<[u8; 32]> {
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

    fn from_document(document: &Automerge, maximum: usize) -> Result<Self, PeerDocumentReadError> {
        Ok(Self {
            heads: document.get_heads(),
            bytes: saved_whole(document, maximum)?,
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
/// longer read the record, or the history no longer covers every write) forgets it, as does a
/// write that does not open with the keys the reader holds now: what is kept never answers a
/// reader the workspace or the keys would not answer whole.
#[derive(Default)]
pub struct KeptRecord(Option<Opened>);

impl KeptRecord {
    /// Whether a record is kept opened.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.0.is_some()
    }

    /// Let go of what is kept.
    pub fn forget(&mut self) {
        self.0 = None;
    }

    /// The kept record captured whole, at most `maximum` bytes: the bytes the last capture made
    /// (the document saved whole) with the changes since appended, rather than the document
    /// saved again. `None` when nothing is kept.
    pub(crate) fn capture(
        &mut self,
        maximum: usize,
    ) -> Option<Result<Vec<u8>, PeerDocumentReadError>> {
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
            Ok(bytes.clone())
        };
        opened.captured = Some((bytes, heads));
        Some(captured)
    }
}

struct Opened {
    /// The covered snapshot the writes were merged onto.
    snapshot: Option<super::AcceptedCheckpoint>,
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
}

impl Opened {
    /// Whether `document`'s history continues the one this opened.
    fn continues(&self, document: &PeerDocument) -> bool {
        let snapshot = match (&self.snapshot, &document.covered_snapshot) {
            (None, None) => true,
            (Some(kept), Some(now)) => std::sync::Arc::ptr_eq(&kept.0, &now.0),
            _ => false,
        };
        snapshot
            && document.sealed.len() >= self.count
            && self.count.checked_sub(1).is_none_or(|last| {
                document.sealed.iter().nth(last).map(|op| &op.signature) == self.last.as_ref()
            })
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
        let mut kept = KeptRecord::default();
        let document = self.open_document(reader, route, now, maximum, open, &mut kept)?;
        PeerDocumentCapture::from_document(document, maximum)
    }

    /// `app`'s `record` for the device `reader`, under its current membership and Read
    /// permission: the document itself when it is held in the clear; an encrypted record's
    /// retained sealed history opened through the embedding owner's channel keys (`open`) onto
    /// what `kept` opened of it before, once that history is known to cover every admitted write.
    /// What it opens is at most `maximum` bytes: a sealed record reads in the room its caller
    /// allows, as one held in the clear does. Its sealed history is bounded where it is admitted
    /// (`MAX_SEALED_BYTES` of writes after a compaction set of at most
    /// `MAX_COMPACTION_SET_BYTES`), so the room a caller allows covers a record whatever its history
    /// holds; a fixed bound of the capture's own refused a late joiner's record for good while
    /// every caller allowed several times its size.
    ///
    /// # Errors
    /// The reader may not read the record, it is not held, it is encrypted and nothing opens it,
    /// its history does not cover every admitted write or does not open, or it exceeds `maximum`.
    pub fn open_document<'a>(
        &'a self,
        reader: [u8; 32],
        (app, record): (&str, &str),
        now: SystemTime,
        maximum: usize,
        open: Option<OpenSealed<'_>>,
        kept: &'a mut KeptRecord,
    ) -> Result<&'a Automerge, PeerDocumentReadError> {
        let document = match self.readable(reader, (app, record), now, open.is_some()) {
            Ok(document) => document,
            Err(error) => {
                kept.forget();
                return Err(error);
            }
        };
        match (document.encryption_required, open) {
            (true, Some(open)) => open_history(document, open, maximum, kept),
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
            app,
            record,
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
        if !self.core.admission_v5 {
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

/// `document`'s sealed history opened onto its covered snapshot, continuing what `kept` opened
/// of it before. A history that does not open whole leaves nothing kept.
fn open_history<'a>(
    document: &PeerDocument,
    open: OpenSealed<'_>,
    maximum: usize,
    kept: &'a mut KeptRecord,
) -> Result<&'a Automerge, PeerDocumentReadError> {
    let base = document
        .covered_snapshot
        .as_ref()
        .map_or(0, |base| base.0.bytes.len());
    if base > maximum {
        return Err(PeerDocumentReadError::Capacity);
    }
    let mut opened = match kept.0.take().filter(|opened| opened.continues(document)) {
        Some(opened) => opened,
        None => Opened {
            snapshot: document.covered_snapshot.clone(),
            count: 0,
            last: None,
            size: base,
            transfers: crate::document_frame::Transfers::default(),
            captured: None,
            stuck: Vec::new(),
            document: match &document.covered_snapshot {
                Some(base) => base
                    .decoded()
                    .map_err(PeerDocumentReadError::Opening)?
                    .doc
                    .clone(),
                None => AutoCommit::new(),
            },
        },
    };
    let mut writes = Vec::new();
    // A write too large for one frame was written as segments, each a sealed write of its own:
    // it is read once all of them are held.
    for (index, op) in document.sealed.iter().enumerate().skip(opened.count) {
        let plaintext = open(op).map_err(PeerDocumentReadError::Opening)?;
        opened.size = opened.size.saturating_add(plaintext.len());
        if opened.size > maximum {
            return Err(PeerDocumentReadError::Capacity);
        }
        if let Some(mutation) = opened
            .transfers
            .take(*op.author.as_bytes(), &plaintext)
            .map_err(PeerDocumentReadError::Opening)?
        {
            writes.push((index, mutation));
        }
        opened.count = index.saturating_add(1);
        opened.last = Some(op.signature);
    }
    if !opened.transfers.is_empty() {
        return Err(PeerDocumentReadError::Incomplete);
    }
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
    let (mut merged, left) = super::sealed_compaction::merge_writes(
        opened.document,
        writes,
        super::sealed_compaction::LeftOut::Named,
        Some(&signers),
    )
    .map_err(PeerDocumentReadError::Opening)?;
    // A write read without before stays so until what it builds on is here: only its arrival
    // changes that, and the merge tells it.
    let mut stuck = Vec::new();
    for (index, mutation) in std::mem::take(&mut opened.stuck) {
        match super::sealed_compaction::merge_write(&mut merged, &mutation) {
            super::sealed_compaction::WriteMerge::Waiting => stuck.push((index, mutation)),
            super::sealed_compaction::WriteMerge::Applied
            | super::sealed_compaction::WriteMerge::AlreadyHeld { .. } => {}
            super::sealed_compaction::WriteMerge::Refused(why) => {
                return Err(PeerDocumentReadError::Opening(why));
            }
        }
    }
    opened.stuck = stuck;
    for (index, reason, mutation) in left {
        match reason {
            super::sealed_compaction::UnmergedReason::AlreadyHeld { .. } => {}
            super::sealed_compaction::UnmergedReason::BehindUnsent { .. } => {
                opened.stuck.push((index, mutation));
            }
            super::sealed_compaction::UnmergedReason::MissingDependencies { .. } => {
                return Err(PeerDocumentReadError::Incomplete);
            }
            super::sealed_compaction::UnmergedReason::Unreadable(why) => {
                return Err(PeerDocumentReadError::Opening(why));
            }
        }
    }
    opened.document = merged;
    Ok(kept.0.insert(opened).document.committed_document())
}
