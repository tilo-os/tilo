//! One private peer representation; storage qualification cannot be copied.
use super::{
    AcceptedCheckpoint, Actor, ArtifactKind, AutoCommit, BTreeMap, CachedDocument, CapDecision,
    CapOverride, ChannelState, DeviceCert, DurableV4, DurableV5, DurableV6, Ed25519PublicKey,
    ExactAdmissions, GovernedIdentity, NeighbourMsg, Principal, RedemptionReceipt, ResourceRef,
    RevocationList, Role, Route, Scope, StatePeer, Subject, SystemTime, VecDeque, Verb,
    VerifiedCertificate, decide_capability, err, now_ns, now_secs, room,
};
use crate::resources::native::NativeDocument;
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
    type CertificateRoster = BTreeMap<[u8; 32], VerifiedCertificate>;
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
}

// Historical restore keeps checkpoint bytes lazy. No eager load rejects
// previously accepted opaque checkpoint bytes.
pub(super) struct NativeCheckpoint {
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "kept for a later decode; only tests read it today"
        )
    )]
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
    /// Bounded reads refuse encrypted documents and leave it empty.
    pub(super) sealed: Vec<collab_types::SignedOp>,
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
    pub(super) receipts: BTreeMap<String, Vec<u8>>,
    pub(super) room_doc: Option<S::Room>,
    pub(super) documents: BTreeMap<Route, PeerDocument<S>>,
    pub(super) pending: VecDeque<([u8; 32], NeighbourMsg, bool)>,
    pub(super) pending_bytes: usize,
    pub(super) checkpoint_times: BTreeMap<Route, SystemTime>,
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
}

#[cfg(test)]
mod storage_tests;
