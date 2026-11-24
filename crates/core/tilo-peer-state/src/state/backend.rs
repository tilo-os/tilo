//! The peer's one in-memory representation.
use super::{
    AcceptedCheckpoint, BTreeMap, CachedDocument, DeviceCert, ExactAdmissions, GovernedIdentity,
    InboundState, NeighbourMsg, RedemptionReceipt, RevocationList, Role, Route, SystemTime,
    VecDeque, VerifiedCertificate, err, now_ns,
};
use crate::shared::Shared;
use std::sync::{Arc, OnceLock};
pub(super) mod access;
pub(super) mod governed;
pub(super) mod member;
mod pending;
pub(super) use pending::validate as validate_pending;
pub(super) mod revocations;
pub(super) use revocations::restore as restore_revocation_union;
mod roster;
pub(super) use roster::restore as restore_certificate_roster;

pub(super) struct PeerDocument {
    pub(super) doc: CachedDocument,
    pub(super) channel: InboundState,
    pub(super) encryption_required: bool,
    /// The highest write of each writer, by its hex key, that this record's
    /// content carries: the watermarks of a governed checkpoint, and every
    /// write admitted in the clear, which its own document holds.
    pub(super) covered: BTreeMap<String, u64>,
    pub(super) covered_snapshot: Option<AcceptedCheckpoint>,
    /// Sealed writes this replica admitted but holds no key to open, in
    /// admission order: the durable record of an encrypted document here.
    /// Restore validates every retained envelope before replay.
    pub(super) sealed: SealedLog,
    /// The writes of the record sealed before reader sets
    /// ([`collab_types::artifact_before_reader_sets`]) whose content a key holder has
    /// since sealed again under the record's own scope, in a compaction it signed: each kept as
    /// its author signed it, so who wrote what then stays provable. They are never history to
    /// replay or merge again; the compaction that took their place says which they are.
    pub(super) resealed: Arc<[collab_types::SignedOp]>,
    /// The writes the record's leading checkpoint stands in for, each as its author signed it, in
    /// the order they left its history: what a device with the record's keys checks the checkpoint
    /// against before it reads it, and reads in its place when the checkpoint holds anything else
    /// ([`super::checkpoints`]). They are dropped only once every device with the record's keys has
    /// checked the checkpoint before them, which then leads them; until then a checkpoint that
    /// folds this one keeps them too. Never history to replay or merge as writes of their own.
    pub(super) checkpoint_writes: Arc<[collab_types::SignedOp]>,
}

impl PeerDocument {
    /// Replace the record's sealed history with `history`, which a compaction made: of the writes
    /// it folds away, those sealed before reader sets are kept as the record's originals
    /// ([`Self::resealed`]), never dropped. Every write it folds away is kept beside the new
    /// checkpoint ([`Self::checkpoint_writes`]): `checked` says whether every device with the
    /// record's keys checked the checkpoint the history led with, and only then do the
    /// writes kept before it go, that checkpoint leading what is kept from then on.
    pub(super) fn fold_sealed(
        &mut self,
        history: SealedLog,
        before_reader_sets: impl Fn(&collab_types::SignedOp) -> bool,
        checked: bool,
    ) {
        let kept: std::collections::HashSet<[u8; 64]> =
            history.iter().map(|op| op.signature.0).collect();
        let folded: Vec<collab_types::SignedOp> = self
            .sealed
            .iter()
            .filter(|op| !kept.contains(&op.signature.0))
            .cloned()
            .collect();
        let originals: Vec<collab_types::SignedOp> = folded
            .iter()
            .filter(|op| before_reader_sets(op))
            .cloned()
            .collect();
        if !originals.is_empty() {
            self.resealed = self.resealed.iter().cloned().chain(originals).collect();
        }
        if !folded.is_empty() {
            let leads = folded
                .first()
                .is_some_and(super::checkpoints::is_checkpoint);
            self.checkpoint_writes = if leads && checked {
                folded.into()
            } else if leads && self.checkpoint_writes.is_empty() {
                // A checkpoint not every device with the record's keys checked leads what is kept
                // only as one they all did: it goes after the writes it folds, which are read in
                // its place.
                let (set, writes): (Vec<_>, Vec<_>) = folded.into_iter().partition(|op| {
                    op.kind == collab_types::OpKind::Compaction
                        && self
                            .sealed
                            .first()
                            .is_some_and(|first| first.author == op.author && first.seq == op.seq)
                });
                writes.into_iter().chain(set).collect()
            } else {
                self.checkpoint_writes
                    .iter()
                    .cloned()
                    .chain(folded)
                    .collect()
            };
        }
        self.sealed = history;
    }

    /// Whether the record keeps anything of a sealed history: writes to replay, originals of
    /// writes sealed again, or the writes its checkpoint stands in for.
    pub(super) fn keeps_sealed(&self) -> bool {
        !self.sealed.is_empty() || !self.resealed.is_empty() || !self.checkpoint_writes.is_empty()
    }

    /// What leads the record's entry of the durable sealed histories, before the history itself:
    /// its originals of writes sealed again, then the writes its checkpoint stands in for
    /// ([`super::sealed_history::restore`] tells them apart).
    pub(super) fn kept_beside(&self) -> Vec<collab_types::SignedOp> {
        self.resealed
            .iter()
            .chain(self.checkpoint_writes.iter())
            .cloned()
            .collect()
    }
}

/// A record's sealed writes in admission order. Every admission decides in
/// a copy of the state, and a busy record's history holds thousands of
/// writes: it is kept in shared chunks, so a copy costs a pointer per chunk
/// and a write copies at most the chunk it joins, not the whole history.
/// Each chunk keeps its encoding once made, so a durable copy of the history
/// encodes only the chunk a write joined.
#[derive(Clone, Default)]
pub(crate) struct SealedLog {
    chunks: Vec<Arc<SealedChunk>>,
    len: usize,
    /// The payload bytes of every write held.
    bytes: usize,
    /// The ops and payload bytes of the compaction set it starts with: its
    /// leading compaction and the segments it names.
    set: (usize, usize),
}

#[derive(Default)]
struct SealedChunk {
    ops: Vec<collab_types::SignedOp>,
    /// The concatenated postcard encoding of `ops`, once asked for.
    encoded: OnceLock<Arc<[u8]>>,
    /// The length and digest of that encoding, once asked for: what a durable hash folds in,
    /// taken without keeping the bytes.
    digest: OnceLock<(u64, [u8; 32])>,
}
impl Clone for SealedChunk {
    /// A copy is only ever made to be changed: it starts unencoded.
    fn clone(&self) -> Self {
        Self {
            ops: self.ops.clone(),
            encoded: OnceLock::new(),
            digest: OnceLock::new(),
        }
    }
}

/// Writes one shared chunk of a [`SealedLog`] holds.
const SEALED_CHUNK: usize = 64;

impl SealedLog {
    pub(crate) fn len(&self) -> usize {
        self.len
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }
    /// The payload bytes of every write held.
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }
    /// The writes held since the compaction set it starts with, and their
    /// payload bytes.
    pub(crate) fn writes(&self) -> (usize, usize) {
        (
            self.len.saturating_sub(self.set.0),
            self.bytes.saturating_sub(self.set.1),
        )
    }
    /// Whether the history is within its bounds: its compaction set within
    /// the set's, the writes after it within theirs.
    pub(crate) fn within_bounds(&self) -> bool {
        super::sealed_within_bounds(self.set, self.writes())
    }
    /// Whether a write of `payload` bytes after the history keeps it within
    /// its bounds.
    pub(crate) fn takes(&self, payload: usize) -> bool {
        let (writes, bytes) = self.writes();
        writes < super::MAX_SEALED_OPS && bytes.saturating_add(payload) <= super::MAX_SEALED_BYTES
    }
    /// Whether the writes since the last compaction have used three quarters
    /// of their bound.
    pub(crate) fn due(&self) -> bool {
        let (writes, bytes) = self.writes();
        writes >= super::MAX_SEALED_OPS / 4 * 3 || bytes >= super::MAX_SEALED_BYTES / 4 * 3
    }
    pub(crate) fn iter(&self) -> impl Iterator<Item = &collab_types::SignedOp> {
        self.chunks.iter().flat_map(|chunk| chunk.ops.iter())
    }
    pub(crate) fn first(&self) -> Option<&collab_types::SignedOp> {
        self.iter().next()
    }
    /// The write admitted last.
    pub(crate) fn last(&self) -> Option<&collab_types::SignedOp> {
        self.chunks.last()?.ops.last()
    }
    pub(crate) fn to_vec(&self) -> Vec<collab_types::SignedOp> {
        self.iter().cloned().collect()
    }
    pub(crate) fn push(&mut self, op: collab_types::SignedOp) {
        self.len = self.len.saturating_add(1);
        self.bytes = self.bytes.saturating_add(op.payload.len());
        // Only a compaction leads a history, and only its segments follow it.
        if op.kind == collab_types::OpKind::Compaction && self.set.0 == self.len.saturating_sub(1) {
            self.set = (self.len, self.set.1.saturating_add(op.payload.len()));
        }
        match self.chunks.last_mut() {
            Some(chunk) if chunk.ops.len() < SEALED_CHUNK => {
                let chunk = Arc::make_mut(chunk);
                chunk.encoded = OnceLock::new();
                chunk.digest = OnceLock::new();
                chunk.ops.push(op);
            }
            _ => self.chunks.push(Arc::new(SealedChunk {
                ops: vec![op],
                encoded: OnceLock::new(),
                digest: OnceLock::new(),
            })),
        }
    }
    /// The head of a document's entry of the durable sealed histories, `(route, count)` as postcard
    /// writes it, and the encoding of `resealed`, which lead the writes held in the entry. A
    /// restore tells them apart by what they are ([`super::sealed_history::restore`]): writes
    /// sealed before reader sets ahead of a compaction sealed since.
    fn head(
        route: &Route,
        held: usize,
        resealed: &[collab_types::SignedOp],
    ) -> Result<(Vec<u8>, Option<Vec<u8>>), String> {
        let count = held.saturating_add(resealed.len());
        let route_and_count = postcard::to_allocvec(&(route, count)).map_err(err)?;
        if resealed.is_empty() {
            return Ok((route_and_count, None));
        }
        let mut originals = Vec::new();
        for op in resealed {
            originals = postcard::to_extend(op, originals).map_err(err)?;
        }
        Ok((route_and_count, Some(originals)))
    }

    /// What `postcard` makes of `(route, every write held)`, in parts: the
    /// route and count, then each chunk, encoded once per state of it and
    /// the same allocation while it lasts. A write changes the last chunk
    /// alone.
    /// The lengths and digests of [`Self::encode_parts`], taken without keeping the parts: a
    /// session that stores nothing hashes its records' histories instead of holding a second
    /// copy of every one.
    pub(super) fn digested_parts(
        &self,
        route: &Route,
        resealed: &[collab_types::SignedOp],
    ) -> Result<Vec<super::StretchDigest>, String> {
        let (head, originals) = Self::head(route, self.len, resealed)?;
        let mut parts = Vec::with_capacity(self.chunks.len().saturating_add(2));
        parts.push((
            u64::try_from(head.len()).unwrap_or(u64::MAX),
            *blake3::hash(&head).as_bytes(),
        ));
        if let Some(originals) = originals {
            parts.push((
                u64::try_from(originals.len()).unwrap_or(u64::MAX),
                *blake3::hash(&originals).as_bytes(),
            ));
        }
        for chunk in &self.chunks {
            if let Some(stretch) = chunk.digest.get() {
                parts.push(*stretch);
                continue;
            }
            let mut encoded = Vec::new();
            for op in &chunk.ops {
                encoded = postcard::to_extend(op, encoded).map_err(err)?;
            }
            let stretch = (
                u64::try_from(encoded.len()).unwrap_or(u64::MAX),
                *blake3::hash(&encoded).as_bytes(),
            );
            // The digest is the whole of what is kept here: a chunk's bytes are grown again
            // whenever a hash asks, and kept only when something stores them.
            let _ = chunk.digest.set(stretch);
            parts.push(stretch);
        }
        Ok(parts)
    }

    pub(super) fn encode_parts(
        &self,
        route: &Route,
        resealed: &[collab_types::SignedOp],
    ) -> Result<Vec<Arc<[u8]>>, String> {
        let (head, originals) = Self::head(route, self.len, resealed)?;
        let mut parts = Vec::with_capacity(self.chunks.len().saturating_add(2));
        parts.push(head.into());
        if let Some(originals) = originals {
            parts.push(originals.into());
        }
        for chunk in &self.chunks {
            if let Some(encoded) = chunk.encoded.get() {
                parts.push(Arc::clone(encoded));
                continue;
            }
            let mut encoded = Vec::new();
            for op in &chunk.ops {
                encoded = postcard::to_extend(op, encoded).map_err(err)?;
            }
            let _ = chunk.digest.set((
                u64::try_from(encoded.len()).unwrap_or(u64::MAX),
                *blake3::hash(&encoded).as_bytes(),
            ));
            parts.push(Arc::clone(chunk.encoded.get_or_init(|| encoded.into())));
        }
        Ok(parts)
    }
}

impl From<Vec<collab_types::SignedOp>> for SealedLog {
    fn from(ops: Vec<collab_types::SignedOp>) -> Self {
        let mut log = Self::default();
        for op in ops {
            log.push(op);
        }
        log
    }
}

impl<'a> IntoIterator for &'a SealedLog {
    type Item = &'a collab_types::SignedOp;
    type IntoIter = Box<dyn Iterator<Item = &'a collab_types::SignedOp> + 'a>;
    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter())
    }
}
/// A record as a peer keeps it: shared between copies of the peer until one
/// changes it, with its durable encoding made at most once per state of it.
/// Mutable access (`DerefMut`) detaches a shared copy and drops the
/// encoding, so no change outlives a stale one.
pub(super) struct StoredDocument {
    document: Arc<PeerDocument>,
    encoded: Arc<OnceLock<EncodedDocument>>,
    /// The lengths and digests of this record's part of a durable encoding, once asked: what
    /// [`StatePeer::durable_hash`] folds in, kept instead of the part itself, so a session that
    /// stores nothing holds one copy of each record, not two.
    digests: Arc<OnceLock<Vec<super::StretchDigest>>>,
}
/// A record's part of a durable peer encoding.
pub(super) struct EncodedDocument {
    pub(super) route: Route,
    /// Its `DurableDocument`, in stretches.
    pub(super) document: Vec<super::durable::Piece>,
    /// Its `(Route, Vec<SignedOp>)` sealed history entry, when it holds
    /// sealed writes, in parts ([`SealedLog::encode_parts`]).
    pub(super) sealed: Option<Vec<Arc<[u8]>>>,
}
impl Clone for StoredDocument {
    fn clone(&self) -> Self {
        Self {
            document: Arc::clone(&self.document),
            encoded: Arc::clone(&self.encoded),
            digests: Arc::clone(&self.digests),
        }
    }
}
impl From<PeerDocument> for StoredDocument {
    fn from(document: PeerDocument) -> Self {
        Self {
            document: Arc::new(document),
            encoded: Arc::default(),
            digests: Arc::default(),
        }
    }
}
impl std::ops::Deref for StoredDocument {
    type Target = PeerDocument;
    fn deref(&self) -> &PeerDocument {
        &self.document
    }
}
impl std::ops::DerefMut for StoredDocument {
    fn deref_mut(&mut self) -> &mut PeerDocument {
        self.encoded = Arc::default();
        self.digests = Arc::default();
        Arc::make_mut(&mut self.document)
    }
}
impl StoredDocument {
    /// Whether `a` and `b` are copies of one record state no copy changed.
    pub(super) fn ptr_eq(a: &Self, b: &Self) -> bool {
        Arc::ptr_eq(&a.document, &b.document)
    }
    /// Whether `a` and `b` are copies of one document state whose durable part
    /// nothing changed either: what is made of a document's part is made again
    /// for one that is not.
    pub(super) fn same(a: &Self, b: &Self) -> bool {
        Self::ptr_eq(a, b)
            && Arc::ptr_eq(&a.encoded, &b.encoded)
            && Arc::ptr_eq(&a.digests, &b.digests)
    }
    /// Install the sealed history a restore validated for this record, which
    /// the restore holds alone.
    pub(super) fn restore_sealed(
        &mut self,
        sealed: SealedLog,
        resealed: Arc<[collab_types::SignedOp]>,
        checkpoint_writes: Arc<[collab_types::SignedOp]>,
    ) -> Result<(), String> {
        self.encoded = Arc::default();
        self.digests = Arc::default();
        let document = Arc::get_mut(&mut self.document).ok_or("restored record is shared")?;
        document.sealed = sealed;
        document.resealed = resealed;
        document.checkpoint_writes = checkpoint_writes;
        Ok(())
    }
    /// The record's durable encoding as `encode` makes it for `route`,
    /// made once per state of the record.
    pub(super) fn encoded(
        &self,
        route: &Route,
        encode: impl FnOnce() -> Result<EncodedDocument, String>,
    ) -> Result<&EncodedDocument, String> {
        if let Some(encoded) = self.encoded.get() {
            return Ok(encoded);
        }
        let encoded = encode()?;
        debug_assert_eq!(&encoded.route, route);
        Ok(self.encoded.get_or_init(|| encoded))
    }

    /// The lengths and digests of the record's part of a durable encoding, as `digest` takes
    /// them for `route`: made once per state of the record, and no part kept.
    pub(super) fn digests(
        &self,
        digest: impl FnOnce() -> Result<Vec<super::StretchDigest>, String>,
    ) -> Result<&[super::StretchDigest], String> {
        if let Some(digests) = self.digests.get() {
            return Ok(digests);
        }
        let digests = digest()?;
        Ok(self.digests.get_or_init(|| digests))
    }

    /// The bytes a kept durable part holds, for accounting in tests.
    #[cfg(test)]
    pub(crate) fn encoded_bytes(&self) -> usize {
        self.encoded.get().map_or(0, |encoded| {
            encoded
                .document
                .iter()
                .map(|piece| match piece {
                    super::durable::Piece::Part(part) => part.len(),
                    super::durable::Piece::Since(runs) => runs.iter().map(|run| run.len()).sum(),
                    super::durable::Piece::Digested(..) => 0,
                })
                .sum()
        })
    }
}

impl Clone for PeerDocument {
    fn clone(&self) -> Self {
        Self {
            doc: self.doc.clone(),
            channel: self.channel.clone(),
            encryption_required: self.encryption_required,
            covered: self.covered.clone(),
            covered_snapshot: self.covered_snapshot.clone(),
            sealed: self.sealed.clone(),
            resealed: std::sync::Arc::clone(&self.resealed),
            checkpoint_writes: std::sync::Arc::clone(&self.checkpoint_writes),
        }
    }
}

pub(super) struct PeerCore {
    pub(super) workspace: String,
    /// The workspace owner's user root: every device it certifies may write as the owner.
    pub(super) owner: [u8; 32],
    /// The device this replica was made on or joined through. A device on
    /// an operating system Tilo does not control is held to the foreign
    /// bound in the workspace, except this one: a workspace made on such a device is
    /// governed from it.
    pub(super) origin: [u8; 32],
    pub(super) certificates: Shared<BTreeMap<[u8; 32], VerifiedCertificate>>,
    pub(super) revocations: RevocationList,
    pub(super) receipts: Shared<BTreeMap<String, Vec<u8>>>,
    pub(super) workspace_doc: Option<CachedDocument>,
    pub(super) documents: super::documents::Documents,
    pub(super) pending: VecDeque<([u8; 32], NeighbourMsg, bool)>,
    pub(super) pending_bytes: usize,
    pub(super) exact_admissions: ExactAdmissions,
    pub(super) exact_selected: bool,
}
impl Clone for PeerCore {
    fn clone(&self) -> Self {
        Self {
            workspace: self.workspace.clone(),
            owner: self.owner,
            origin: self.origin,
            certificates: self.certificates.clone(),
            revocations: self.revocations.clone(),
            receipts: self.receipts.clone(),
            workspace_doc: self.workspace_doc.clone(),
            documents: self.documents.clone(),
            pending: self.pending.clone(),
            pending_bytes: self.pending_bytes,
            exact_admissions: self.exact_admissions.clone(),
            exact_selected: self.exact_selected,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum CertificateLookupError {
    Missing,
    Validity(tilo_user_identity::DeviceCertError),
    Revoked,
}
impl std::fmt::Display for CertificateLookupError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => formatter.write_str("certified identity missing"),
            Self::Validity(error) => error.fmt(formatter),
            Self::Revoked => formatter.write_str("device revoked"),
        }
    }
}
impl PeerCore {
    pub(super) fn certificate_typed(
        &self,
        source: &[u8; 32],
        now: SystemTime,
    ) -> Result<&DeviceCert, CertificateLookupError> {
        let certificate = self.certificate_held(source, now)?;
        if self
            .revocations
            .is_revoked(&certificate.user_public_key, source)
        {
            return Err(CertificateLookupError::Revoked);
        }
        Ok(certificate)
    }

    /// [`Self::certificate_typed`] of a device whether or not its person
    /// removed it since: the certificate a write of its history is decided
    /// with ([`crate::removals`]). Never an answer to what the device may do
    /// now.
    pub(super) fn certificate_held(
        &self,
        source: &[u8; 32],
        now: SystemTime,
    ) -> Result<&DeviceCert, CertificateLookupError> {
        self.certificates
            .get(source)
            .ok_or(CertificateLookupError::Missing)?
            .current_typed(now_ns(now))
            .map_err(CertificateLookupError::Validity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TransportOrigin;
    use ed25519_dalek::SigningKey;
    #[test]
    fn typed_certificate_lookup_uses_real_current_roster() {
        let (mut peer, _, _, member, now) = crate::admission_tests::seeded();
        let source = member.device_public_key;
        assert_eq!(peer.core.certificate_typed(&source, now).unwrap(), &member);
        assert_eq!(
            peer.core.certificate_typed(&[77; 32], now),
            Err(CertificateLookupError::Missing)
        );
        let mut entry = tilo_user_identity::RevocationEntry {
            user_public_key: member.user_public_key,
            revoked_device_public_key: source,
            revoked_at_ns: 1,
            reason: tilo_user_identity::RevocationReason::Compromised,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut entry, &SigningKey::from_bytes(&[1; 32]))
            .unwrap();
        peer.receive_from(
            TransportOrigin::ServerDelta(peer.owner_certificates()[0].device_public_key),
            NeighbourMsg::IdentityHello {
                protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
                cert: Box::new(peer.owner_certificates()[0].clone()),
                revocations: vec![entry],
            },
            now,
        )
        .unwrap();
        assert_eq!(
            peer.core.certificate_typed(&source, now),
            Err(CertificateLookupError::Revoked)
        );
    }

    /// A copy of a long sealed history shares its chunks; a write to the
    /// copy copies only the chunk it joins, and both read in order.
    #[test]
    fn a_sealed_log_copy_shares_its_chunks_and_keeps_order() {
        let op = |seq: u64| {
            collab_types::SignedOp::new_unsigned(
                vec![0; 10],
                collab_types::ArtifactId::new(),
                collab_types::Ed25519PublicKey::from_bytes([1; 32]),
                collab_types::AttestationId::new(),
                collab_types::OpKind::Change,
                web_time::SystemTime::UNIX_EPOCH,
                seq,
            )
        };
        let log = SealedLog::from((1..=200).map(op).collect::<Vec<_>>());
        let mut copy = log.clone();
        copy.push(op(201));
        let shared = log
            .chunks
            .iter()
            .zip(&copy.chunks)
            .filter(|(a, b)| std::sync::Arc::ptr_eq(a, b))
            .count();
        assert_eq!(
            shared,
            log.chunks.len() - 1,
            "only the last chunk is copied"
        );
        assert_eq!((log.len(), copy.len()), (200, 201));
        assert_eq!((log.bytes(), copy.bytes()), (2000, 2010));
        let seqs: Vec<u64> = copy.iter().map(|op| op.seq).collect();
        assert_eq!(seqs, (1..=201).collect::<Vec<_>>());
    }
}
