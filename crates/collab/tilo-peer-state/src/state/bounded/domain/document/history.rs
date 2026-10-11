//! Open only the retained, validated history of an admitted record.
use super::{AutoCommit, ChangeHash, PeerDocumentReadError, PeerRead, SystemTime};
use crate::resources::native::NativeBatch;
use crate::state::Route;
use crate::state::backend::{BoundedNative, PeerDocument};
use collab_types::{DocumentMutation, Ed25519PublicKey, OpClass, SealedCompaction, SignedOp};

/// Complete history bytes and heads from one bounded, currently permitted read.
/// It carries data only, with no owner credential or mutable peer.
pub struct PeerDocumentCapture {
    heads: Vec<ChangeHash>,
    bytes: Vec<u8>,
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

    fn from_document(
        mut document: AutoCommit,
        maximum: usize,
    ) -> Result<Self, PeerDocumentReadError> {
        if !automerge::ReadDoc::get_missing_deps(&document, &[]).is_empty() {
            return Err(PeerDocumentReadError::Incomplete);
        }
        let heads = document.get_heads();
        let changes = document.get_changes(&[]);
        if changes.is_empty() {
            // An empty graph still needs a canonical header: a zero-byte
            // stream is not a valid broker capture. This is library-format
            // data, never cached record state or an authority fallback.
            static EMPTY_DOCUMENT: std::sync::LazyLock<Vec<u8>> =
                std::sync::LazyLock::new(|| AutoCommit::new().save());
            if EMPTY_DOCUMENT.len() > maximum {
                return Err(PeerDocumentReadError::Capacity);
            }
            return Ok(Self {
                heads,
                bytes: EMPTY_DOCUMENT.clone(),
            });
        }
        // Count borrowed admitted chunks before allocating the output. Raw
        // change chunks preserve every original writer and load as a complete
        // document without allocating an unbounded compressed save first.
        let size = changes.iter().try_fold(0usize, |size, change| {
            size.checked_add(change.raw_bytes().len())
                .filter(|size| *size <= maximum)
                .ok_or(PeerDocumentReadError::Capacity)
        })?;
        let mut bytes = Vec::with_capacity(size);
        for change in changes {
            bytes.extend_from_slice(change.raw_bytes());
        }
        Ok(Self { heads, bytes })
    }
}

impl PeerRead<'_> {
    /// Capture an exact record under current Read permission. Encrypted
    /// history is opened only after bounded restore verified every envelope;
    /// `open` is supplied by the embedding owner's existing channel service.
    /// No cached opened document or live peer is used as an authority fallback.
    ///
    /// # Errors
    /// Refuses denied access, incomplete admission coverage, unavailable epoch
    /// keys, invalid mutations, conflicting writers or an exceeded byte limit.
    pub fn capture_document(
        &self,
        device: [u8; 32],
        route: (&str, &str),
        now: SystemTime,
        maximum: usize,
        open: &mut dyn FnMut(&SignedOp) -> Result<Vec<u8>, String>,
    ) -> Result<PeerDocumentCapture, PeerDocumentReadError> {
        let (app, record) = route;
        if !self
            .document_read_allowed(device, app, record, now)
            .map_err(PeerDocumentReadError::Peer)?
        {
            return Err(PeerDocumentReadError::Denied);
        }
        let (route, document) = self
            .peer
            .core
            .documents
            .iter()
            .find(|(route, _)| route.app == app && route.record == record)
            .ok_or(PeerDocumentReadError::Missing)?;
        let maximum = maximum.min(self.limits.normalized_bytes);
        let document = if document.encryption_required {
            self.check_sealed_coverage(route, document)?;
            open_history(document, open, maximum)?
        } else {
            document.doc.document().clone()
        };
        PeerDocumentCapture::from_document(document, maximum)
    }

    fn check_sealed_coverage(
        &self,
        route: &Route,
        document: &PeerDocument<BoundedNative>,
    ) -> Result<(), PeerDocumentReadError> {
        if document.sealed.is_empty() {
            return Err(PeerDocumentReadError::Encrypted);
        }
        let exact = self
            .peer
            .core
            .exact_admissions
            .as_ref()
            .ok_or(PeerDocumentReadError::Incomplete)?;
        let held: std::collections::HashSet<_> = document
            .sealed
            .iter()
            .map(|op| (*op.author.as_bytes(), op.seq))
            .collect();
        let compaction = document
            .sealed
            .first()
            .filter(|op| op.op_class == OpClass::Compaction)
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
        if exact.iter().any(|(key, _)| {
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

fn open_history(
    document: &PeerDocument<BoundedNative>,
    open: &mut dyn FnMut(&SignedOp) -> Result<Vec<u8>, String>,
    maximum: usize,
) -> Result<AutoCommit, PeerDocumentReadError> {
    let mut remaining = maximum;
    let mut merged = match &document.covered_snapshot {
        Some(base) => NativeBatch::continuing(&mut remaining)
            .load(&base.original)
            .map_err(|error| PeerDocumentReadError::Peer(error.into()))?
            .document()
            .clone(),
        None => AutoCommit::new(),
    };
    let mut waiting = Vec::with_capacity(document.sealed.len());
    for op in &document.sealed {
        let plaintext = open(op).map_err(PeerDocumentReadError::Opening)?;
        remaining = remaining
            .checked_sub(plaintext.len())
            .ok_or(PeerDocumentReadError::Capacity)?;
        waiting.push(
            DocumentMutation::decode(&plaintext)
                .map_err(|error| PeerDocumentReadError::Opening(error.to_string()))?,
        );
    }
    // Admission order may differ from causal order on a keyless replica.
    while !waiting.is_empty() {
        let before = waiting.len();
        let mut blocked = Vec::new();
        for mutation in waiting {
            match crate::document::merge_committed_with_status(&mut merged, &mutation)
                .map_err(|error| PeerDocumentReadError::Opening(format!("{error:?}")))?
            {
                crate::document::MergeOutcome::Changed(_) => {}
                crate::document::MergeOutcome::MissingDependencies => blocked.push(mutation),
            }
        }
        if blocked.len() == before {
            return Err(PeerDocumentReadError::Incomplete);
        }
        waiting = blocked;
    }
    Ok(merged)
}
