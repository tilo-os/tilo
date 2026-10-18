//! One private peer representation; storage qualification cannot be copied.
use super::{
    AcceptedCheckpoint, AutoCommit, BTreeMap, CachedDocument, ChannelState, DeviceCert, DurableV4,
    DurableV5, DurableV6, Ed25519PublicKey, ExactAdmissions, GovernedIdentity, NeighbourMsg,
    RedemptionReceipt, RevocationList, Role, Route, StatePeer, SystemTime, VecDeque,
    VerifiedCertificate, err, now_ns, room,
};
use crate::resources::native::NativeDocument;
use crate::shared::Shared;
use std::sync::{Arc, OnceLock};
pub(super) mod access;
pub(super) mod delivery_policy;
mod durable;
pub(super) mod governed;
pub(super) mod media;
pub(super) mod member;
mod pending;
pub(super) mod private_policy;
pub(super) mod restore;
pub(super) use pending::validate as validate_pending;
pub(super) mod revocations;
pub(super) use revocations::restore_legacy as restore_revocation_union;
mod roster;
pub(super) use roster::restore_legacy as restore_certificate_roster;

mod sealed {
    pub trait Sealed {}
}
pub(super) trait Storage: sealed::Sealed {
    type Room;
    type Document;
    type Checkpoint;
    type Retention;
    type CertificateRoster;
    type RevocationUnion;
    type ExactEvidence;
    fn certificate<'a>(
        roster: &'a Self::CertificateRoster,
        key: &[u8; 32],
    ) -> Option<&'a VerifiedCertificate>;
    fn is_revoked(revocations: &Self::RevocationUnion, key: &[u8; 32]) -> bool;
    fn exact_admission<'a>(
        exact: &'a Self::ExactEvidence,
        key: &crate::admission::AdmissionKey,
    ) -> Option<&'a [u8; 32]>;
}
pub(super) struct LegacyNative;
pub(super) struct BoundedNative;
impl sealed::Sealed for LegacyNative {}
impl sealed::Sealed for BoundedNative {}
impl Storage for LegacyNative {
    type Room = CachedDocument;
    type Document = CachedDocument;
    type Checkpoint = AcceptedCheckpoint;
    type Retention = ();
    type CertificateRoster = Shared<BTreeMap<[u8; 32], VerifiedCertificate>>;
    type RevocationUnion = RevocationList;
    type ExactEvidence = ExactAdmissions;
    fn certificate<'a>(
        roster: &'a Self::CertificateRoster,
        key: &[u8; 32],
    ) -> Option<&'a VerifiedCertificate> {
        roster.get(key)
    }
    fn is_revoked(revocations: &Self::RevocationUnion, key: &[u8; 32]) -> bool {
        revocations.is_revoked(key)
    }
    fn exact_admission<'a>(
        exact: &'a Self::ExactEvidence,
        key: &crate::admission::AdmissionKey,
    ) -> Option<&'a [u8; 32]> {
        exact.get(key)
    }
}
impl Storage for BoundedNative {
    type Room = NativeDocument;
    type Document = NativeDocument;
    type Checkpoint = NativeCheckpoint;
    type Retention = ();
    type CertificateRoster = roster::BoundedRoster;
    type RevocationUnion = revocations::BoundedRevocations;
    type ExactEvidence = Option<crate::admission::BoundedExactAdmissions>;
    fn certificate<'a>(
        roster: &'a Self::CertificateRoster,
        key: &[u8; 32],
    ) -> Option<&'a VerifiedCertificate> {
        roster.verified(key)
    }
    fn is_revoked(revocations: &Self::RevocationUnion, key: &[u8; 32]) -> bool {
        revocations.is_revoked(key)
    }
    fn exact_admission<'a>(
        exact: &'a Self::ExactEvidence,
        key: &crate::admission::AdmissionKey,
    ) -> Option<&'a [u8; 32]> {
        exact.as_ref()?.get(key)
    }
}

// Historical restore keeps checkpoint bytes lazy. No eager load rejects
// previously accepted opaque checkpoint bytes.
pub(super) struct NativeCheckpoint {
    pub(super) original: Vec<u8>,
}

pub(super) struct PeerDocument<S: Storage> {
    pub(super) doc: S::Document,
    pub(super) channel: ChannelState,
    pub(super) encryption_required: bool,
    pub(super) covered: BTreeMap<String, u64>,
    pub(super) covered_snapshot: Option<S::Checkpoint>,
    /// Sealed writes this replica admitted but holds no key to open, in
    /// admission order: the durable record of an encrypted document here.
    /// Both restore paths validate every retained envelope before replay.
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
pub(super) struct StoredDocument<S: Storage> {
    document: Arc<PeerDocument<S>>,
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
impl<S: Storage> Clone for StoredDocument<S> {
    fn clone(&self) -> Self {
        Self {
            document: Arc::clone(&self.document),
            encoded: Arc::clone(&self.encoded),
        }
    }
}
impl<S: Storage> From<PeerDocument<S>> for StoredDocument<S> {
    fn from(document: PeerDocument<S>) -> Self {
        Self {
            document: Arc::new(document),
            encoded: Arc::default(),
        }
    }
}
impl<S: Storage> std::ops::Deref for StoredDocument<S> {
    type Target = PeerDocument<S>;
    fn deref(&self) -> &PeerDocument<S> {
        &self.document
    }
}
impl std::ops::DerefMut for StoredDocument<LegacyNative> {
    fn deref_mut(&mut self) -> &mut PeerDocument<LegacyNative> {
        self.encoded = Arc::default();
        Arc::make_mut(&mut self.document)
    }
}
impl<S: Storage> StoredDocument<S> {
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

impl Clone for PeerDocument<LegacyNative> {
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

pub(super) struct PeerCore<S: Storage> {
    pub(super) room: String,
    pub(super) authority: DeviceCert,
    pub(super) certificates: S::CertificateRoster,
    pub(super) revocations: S::RevocationUnion,
    pub(super) receipts: Shared<BTreeMap<String, Vec<u8>>>,
    pub(super) room_doc: Option<S::Room>,
    pub(super) documents: Shared<BTreeMap<Route, StoredDocument<S>>>,
    pub(super) pending: VecDeque<([u8; 32], NeighbourMsg, bool)>,
    pub(super) pending_bytes: usize,
    pub(super) checkpoint_times: Shared<BTreeMap<Route, SystemTime>>,
    pub(super) exact_admissions: S::ExactEvidence,
    pub(super) admission_v5: bool,
    // Transient native cut context is never part of the durable DTO.
    pub(super) native_cut_publisher: Option<[u8; 32]>,
    pub(super) _retention: S::Retention,
}
impl Clone for PeerCore<LegacyNative> {
    fn clone(&self) -> Self {
        Self {
            room: self.room.clone(),
            authority: self.authority.clone(),
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
            _retention: (),
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
impl<S: Storage> PeerCore<S> {
    pub(super) fn certificate_typed(
        &self,
        source: &[u8; 32],
        now: SystemTime,
    ) -> Result<&DeviceCert, CertificateLookupError> {
        let certificate = S::certificate(&self.certificates, source)
            .ok_or(CertificateLookupError::Missing)?
            .current_typed(now_ns(now))
            .map_err(CertificateLookupError::Validity)?;
        if S::is_revoked(&self.revocations, source) {
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
            TransportOrigin::RelayDelta(peer.authority().device_public_key),
            NeighbourMsg::IdentityHello {
                protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
                cert: Box::new(peer.authority().clone()),
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

#[cfg(test)]
mod storage_tests;
