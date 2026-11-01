//! An admitted record captured for a reader, from the live peer, within a
//! byte limit.
use super::{AutoCommit, Route, StatePeer, SystemTime, backend::PeerDocument};
use automerge::ChangeHash;
use collab_types::{DocumentMutation, Ed25519PublicKey, OpKind, SealedCompaction, SignedOp};

/// The most bytes one capture hands out, whatever the caller allows.
const MAX_CAPTURE_BYTES: usize = 8 * 1024 * 1024;

/// Opens one sealed operation of an encrypted record into its plaintext,
/// through the embedding owner's channel keys.
pub type OpenSealed<'a> = &'a mut dyn FnMut(&SignedOp) -> Result<Vec<u8>, String>;

/// Why a record was not captured. These are observations; the embedding
/// owner authenticates caller and time.
#[derive(Debug)]
pub enum PeerDocumentReadError {
    /// The reader is no current member of the room.
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
        // Count the admitted chunks before allocating the output. Raw change
        // chunks preserve every original writer and load as a complete
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

impl StatePeer {
    /// Capture `app`'s `record` for the device `reader` under its current
    /// membership and Read permission, at most `maximum` bytes of it. An
    /// encrypted record is refused without `open`; with it, its retained
    /// sealed history is opened through the embedding owner's already-held
    /// channel keys, once that history is known to cover every admitted
    /// write.
    ///
    /// # Errors
    /// Refuses a non-member, denied access, an absent record, incomplete
    /// admission coverage, unavailable epoch keys, invalid mutations or an
    /// exceeded byte limit.
    pub fn capture_document(
        &self,
        reader: [u8; 32],
        (app, record): (&str, &str),
        now: SystemTime,
        maximum: usize,
        open: Option<OpenSealed<'_>>,
    ) -> Result<PeerDocumentCapture, PeerDocumentReadError> {
        let member = self
            .permissions(reader, now)
            .map_err(PeerDocumentReadError::Membership)?;
        let bound = tilo_action_policy::roles::foreign_device_bound();
        if !super::backend::access::document_read(
            &member.authority(&bound),
            &self.core.room,
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
        let maximum = maximum.min(MAX_CAPTURE_BYTES);
        let document = if document.encryption_required {
            let open = open.ok_or(PeerDocumentReadError::Encrypted)?;
            self.check_sealed_coverage(&route, document)?;
            open_history(document, open, maximum)?
        } else {
            (*document.doc).clone()
        };
        PeerDocumentCapture::from_document(document, maximum)
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

fn open_history(
    document: &PeerDocument,
    open: &mut dyn FnMut(&SignedOp) -> Result<Vec<u8>, String>,
    maximum: usize,
) -> Result<AutoCommit, PeerDocumentReadError> {
    let mut remaining = maximum;
    let mut merged = match &document.covered_snapshot {
        Some(base) => {
            remaining = remaining
                .checked_sub(base.0.bytes.len())
                .ok_or(PeerDocumentReadError::Capacity)?;
            base.decoded()
                .map_err(PeerDocumentReadError::Opening)?
                .doc
                .clone()
        }
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
