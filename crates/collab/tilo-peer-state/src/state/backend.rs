//! The peer's one in-memory representation.
use super::{
    AcceptedCheckpoint, AutoCommit, BTreeMap, CachedDocument, ChannelState, DeviceCert,
    ExactAdmissions, GovernedIdentity, NeighbourMsg, RedemptionReceipt, RevocationList, Role,
    Route, StatePeer, SystemTime, VecDeque, VerifiedCertificate, err, now_ns, room,
};
use crate::shared::Shared;
use std::sync::{Arc, OnceLock};
pub(super) mod access;
pub(super) mod governed;
pub(super) mod media;
pub(super) mod member;
mod pending;
pub(super) mod private_policy;
pub(super) use pending::validate as validate_pending;
pub(super) mod revocations;
pub(super) use revocations::restore_legacy as restore_revocation_union;
mod roster;
pub(super) use roster::restore_legacy as restore_certificate_roster;

pub(super) struct PeerDocument {
    pub(super) doc: CachedDocument,
    pub(super) channel: ChannelState,
    pub(super) encryption_required: bool,
    pub(super) covered: BTreeMap<String, u64>,
    pub(super) covered_snapshot: Option<AcceptedCheckpoint>,
    /// Sealed writes this replica admitted but holds no key to open, in
    /// admission order: the durable record of an encrypted document here.
    /// Restore validates every retained envelope before replay.
    pub(super) sealed: SealedLog,
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
}

#[derive(Default)]
struct SealedChunk {
    ops: Vec<collab_types::SignedOp>,
    /// The concatenated postcard encoding of `ops`, once asked for.
    encoded: OnceLock<Vec<u8>>,
}
impl Clone for SealedChunk {
    /// A copy is only ever made to be changed: it starts unencoded.
    fn clone(&self) -> Self {
        Self {
            ops: self.ops.clone(),
            encoded: OnceLock::new(),
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
    pub(crate) fn iter(&self) -> impl Iterator<Item = &collab_types::SignedOp> {
        self.chunks.iter().flat_map(|chunk| chunk.ops.iter())
    }
    pub(crate) fn first(&self) -> Option<&collab_types::SignedOp> {
        self.iter().next()
    }
    pub(crate) fn to_vec(&self) -> Vec<collab_types::SignedOp> {
        self.iter().cloned().collect()
    }
    pub(crate) fn push(&mut self, op: collab_types::SignedOp) {
        self.len = self.len.saturating_add(1);
        self.bytes = self.bytes.saturating_add(op.payload.len());
        match self.chunks.last_mut() {
            Some(chunk) if chunk.ops.len() < SEALED_CHUNK => {
                let chunk = Arc::make_mut(chunk);
                chunk.encoded = OnceLock::new();
                chunk.ops.push(op);
            }
            _ => self.chunks.push(Arc::new(SealedChunk {
                ops: vec![op],
                encoded: OnceLock::new(),
            })),
        }
    }
    /// Append the postcard encoding of `(route, every write held)` to
    /// `out`: what `postcard` makes of `(Route, Vec<SignedOp>)`, with each
    /// chunk encoded once per state of it.
    pub(super) fn encode_into(&self, route: &Route, out: Vec<u8>) -> Result<Vec<u8>, String> {
        let mut out = postcard::to_extend(&(route, self.len), out).map_err(err)?;
        for chunk in &self.chunks {
            if let Some(encoded) = chunk.encoded.get() {
                out.extend_from_slice(encoded);
                continue;
            }
            let mut encoded = Vec::new();
            for op in &chunk.ops {
                encoded = postcard::to_extend(op, encoded).map_err(err)?;
            }
            out.extend_from_slice(chunk.encoded.get_or_init(|| encoded));
        }
        Ok(out)
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
}
/// A record's part of a durable peer encoding.
pub(super) struct EncodedDocument {
    pub(super) route: Route,
    /// Its `DurableDocument`.
    pub(super) document: Arc<[u8]>,
    /// Its `(Route, Vec<SignedOp>)` sealed history entry, when it holds
    /// sealed writes.
    pub(super) sealed: Option<Arc<[u8]>>,
}
impl Clone for StoredDocument {
    fn clone(&self) -> Self {
        Self {
            document: Arc::clone(&self.document),
            encoded: Arc::clone(&self.encoded),
        }
    }
}
impl From<PeerDocument> for StoredDocument {
    fn from(document: PeerDocument) -> Self {
        Self {
            document: Arc::new(document),
            encoded: Arc::default(),
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
        Arc::make_mut(&mut self.document)
    }
}
impl StoredDocument {
    /// Whether `a` and `b` are copies of one record state no copy changed.
    pub(super) fn ptr_eq(a: &Self, b: &Self) -> bool {
        Arc::ptr_eq(&a.document, &b.document)
    }
    /// Install the sealed history a restore validated for this record, which
    /// the restore holds alone.
    pub(super) fn restore_sealed(&mut self, sealed: SealedLog) -> Result<(), String> {
        self.encoded = Arc::default();
        Arc::get_mut(&mut self.document)
            .ok_or("restored record is shared")?
            .sealed = sealed;
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
        }
    }
}

pub(super) struct PeerCore {
    pub(super) room: String,
    /// The room owner's user root: every device it certifies may write as the owner.
    pub(super) owner: [u8; 32],
    /// The device this replica was made on or joined through. A device on
    /// an operating system Tilo does not control is held to the foreign
    /// bound in the room, except this one: a room made on such a device is
    /// governed from it.
    pub(super) origin: [u8; 32],
    pub(super) certificates: Shared<BTreeMap<[u8; 32], VerifiedCertificate>>,
    pub(super) revocations: RevocationList,
    pub(super) receipts: Shared<BTreeMap<String, Vec<u8>>>,
    pub(super) room_doc: Option<CachedDocument>,
    pub(super) documents: Shared<BTreeMap<Route, StoredDocument>>,
    pub(super) pending: VecDeque<([u8; 32], NeighbourMsg, bool)>,
    pub(super) pending_bytes: usize,
    pub(super) checkpoint_times: Shared<BTreeMap<Route, SystemTime>>,
    pub(super) exact_admissions: ExactAdmissions,
    pub(super) admission_v5: bool,
    // Transient native cut context is never part of the durable DTO.
    pub(super) native_cut_publisher: Option<[u8; 32]>,
}
impl Clone for PeerCore {
    fn clone(&self) -> Self {
        Self {
            room: self.room.clone(),
            owner: self.owner,
            origin: self.origin,
            certificates: self.certificates.clone(),
            revocations: self.revocations.clone(),
            receipts: self.receipts.clone(),
            room_doc: self.room_doc.clone(),
            documents: self.documents.clone(),
            pending: self.pending.clone(),
            pending_bytes: self.pending_bytes,
            checkpoint_times: self.checkpoint_times.clone(),
            exact_admissions: self.exact_admissions.clone(),
            admission_v5: self.admission_v5,
            native_cut_publisher: self.native_cut_publisher,
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
        let certificate = self
            .certificates
            .get(source)
            .ok_or(CertificateLookupError::Missing)?
            .current_typed(now_ns(now))
            .map_err(CertificateLookupError::Validity)?;
        if self.revocations.is_revoked(source) {
            return Err(CertificateLookupError::Revoked);
        }
        Ok(certificate)
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
            TransportOrigin::RelayDelta(peer.owner_certificates()[0].device_public_key),
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
