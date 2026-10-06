//! Versioned, signed clear metadata around opaque MLS governance bytes.
//!
//! Relays validate this frame and target its opaque Welcome/commit payloads;
//! they never receive group keys or decrypt application data.

use std::collections::BTreeSet;

use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

use crate::{
    CanonicalError, ChannelId, Ed25519PublicKey, Signature, VerifyError, canonical_bytes,
    sign_event, verify_event,
};

/// Initial governance wire format. Unknown versions must be rejected.
pub const MLS_GOVERNANCE_VERSION: u16 = 1;
/// Prevent a signed frame from becoming an unbounded allocation vector.
pub const MAX_MLS_CONTROL_BYTES: usize = 16 * 1024 * 1024;

/// Opaque control material plus the identity it must affect.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum MlsGovernanceAction {
    /// A Welcome is targeted only to `recipient`; `existing_member_commit`
    /// is fanned out to pre-existing endpoints other than the issuer.
    Add {
        recipient: Ed25519PublicKey,
        welcome: Vec<u8>,
        existing_member_commit: Option<Vec<u8>>,
    },
    /// A removal commit is delivered to every pre-change endpoint,
    /// including the removed endpoint, except the issuer that already merged.
    Remove {
        removed: Ed25519PublicKey,
        commit: Vec<u8>,
    },
    /// Permanently closes the plaintext migration gate after the encrypted
    /// snapshot named by this digest has a durable broker acknowledgement.
    EnableEncryption { encrypted_snapshot_digest: [u8; 32] },
    /// [`Self::Add`] plus the joiner's content-key history, sealed for the
    /// post-add epoch, so the joiner can decrypt every earlier op without
    /// any other member online. Delivered with the Welcome. Appended as a
    /// new variant so frames written before it keep decoding.
    AddWithKeyHistory {
        recipient: Ed25519PublicKey,
        welcome: Vec<u8>,
        existing_member_commit: Option<Vec<u8>>,
        key_history: Vec<u8>,
    },
    /// The first frame of a channel created encrypted: the issuer's device
    /// is the group's only member and the channel's steward, and no
    /// plaintext document op is accepted for the channel from then on. A
    /// channel that carried plaintext before is migrated by the same frame:
    /// new content is encrypted, earlier plaintext stays with the members
    /// that already hold it. Appended so frames written before it decode.
    Genesis,
}

impl MlsGovernanceAction {
    /// The admission fields of an [`Self::Add`] or
    /// [`Self::AddWithKeyHistory`]: recipient, Welcome, existing-member
    /// commit and (when present) key history.
    #[allow(
        clippy::type_complexity,
        reason = "a borrowed view of one enum variant's fields"
    )]
    pub fn admission(&self) -> Option<(&Ed25519PublicKey, &[u8], Option<&[u8]>, Option<&[u8]>)> {
        match self {
            Self::Add {
                recipient,
                welcome,
                existing_member_commit,
            } => Some((recipient, welcome, existing_member_commit.as_deref(), None)),
            Self::AddWithKeyHistory {
                recipient,
                welcome,
                existing_member_commit,
                key_history,
            } => Some((
                recipient,
                welcome,
                existing_member_commit.as_deref(),
                Some(key_history),
            )),
            Self::Remove { .. } | Self::EnableEncryption { .. } | Self::Genesis => None,
        }
    }
}

/// Signed, replay-linked governance frame.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MlsGovernanceFrameV1 {
    pub version: u16,
    /// Authority-issued idempotency key, unique within this channel.
    pub transaction_id: [u8; 32],
    pub channel: ChannelId,
    /// Revision of the authoritative channel-membership record this change
    /// was checked against.
    pub membership_revision: u64,
    /// Hash of the complete preceding frame, or all-zero for the first.
    pub previous_frame_digest: [u8; 32],
    /// MLS epoch after the represented commit (or current epoch for enable).
    pub epoch: u64,
    pub action: MlsGovernanceAction,
    pub signed_by: Ed25519PublicKey,
    pub signature: Signature,
}

#[derive(Serialize)]
struct SigningView<'a> {
    domain: &'static [u8],
    version: u16,
    transaction_id: [u8; 32],
    channel: ChannelId,
    membership_revision: u64,
    previous_frame_digest: [u8; 32],
    epoch: u64,
    action: &'a MlsGovernanceAction,
    signed_by: Ed25519PublicKey,
}

/// Live authoritative membership inputs required to accept a frame.
pub struct MlsGovernanceContext<'a> {
    pub channel: ChannelId,
    pub membership_revision: u64,
    pub previous_frame_digest: [u8; 32],
    pub expected_epoch: u64,
    pub governors: &'a BTreeSet<Ed25519PublicKey>,
    /// Membership after the authoritative workspace/channel mutation.
    pub desired_members: &'a BTreeSet<Ed25519PublicKey>,
    /// Endpoints present in MLS before this frame.
    pub current_mls_members: &'a BTreeSet<Ed25519PublicKey>,
    /// Set only after the broker durably stores the encrypted snapshot.
    pub durable_snapshot_ack: Option<[u8; 32]>,
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum MlsGovernanceError {
    #[error("unsupported MLS governance version {0}")]
    Version(u16),
    #[error("governance frame does not match the live channel/revision/epoch/head")]
    StaleOrMisrouted,
    #[error("governance signer is not currently authorised to govern")]
    Unauthorized,
    #[error("governance frame signature is invalid: {0}")]
    Signature(String),
    #[error("governance action contradicts authoritative membership")]
    MembershipMismatch,
    #[error("required Welcome or commit is missing or oversized")]
    InvalidControlPayload,
    #[error("encrypted snapshot lacks the matching durable acknowledgement")]
    SnapshotNotDurable,
    #[error("canonical encoding failed: {0}")]
    Canonical(String),
}

impl MlsGovernanceFrameV1 {
    /// Create and sign a V1 frame.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalError`] if the signing view cannot be encoded.
    pub fn signed(
        channel: ChannelId,
        transaction_id: [u8; 32],
        membership_revision: u64,
        previous_frame_digest: [u8; 32],
        epoch: u64,
        action: MlsGovernanceAction,
        signer: &SigningKey,
    ) -> Result<Self, CanonicalError> {
        let mut frame = Self::unsigned(
            channel,
            transaction_id,
            membership_revision,
            previous_frame_digest,
            epoch,
            action,
            Ed25519PublicKey::from_bytes(signer.verifying_key().to_bytes()),
        );
        frame.signature = sign_event(&frame.signing_view(), signer)?;
        Ok(frame)
    }

    /// A V1 frame whose signature is still to be made by `signed_by`, see
    /// [`MlsAccessRequestV1::unsigned`]: sign [`Self::signing_bytes`] and
    /// store it in `signature`.
    pub fn unsigned(
        channel: ChannelId,
        transaction_id: [u8; 32],
        membership_revision: u64,
        previous_frame_digest: [u8; 32],
        epoch: u64,
        action: MlsGovernanceAction,
        signed_by: Ed25519PublicKey,
    ) -> Self {
        Self {
            version: MLS_GOVERNANCE_VERSION,
            transaction_id,
            channel,
            membership_revision,
            previous_frame_digest,
            epoch,
            action,
            signed_by,
            signature: Signature::from_bytes([0; 64]),
        }
    }

    /// The bytes `signed_by` signs.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalError`] if the signing view cannot be encoded.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, CanonicalError> {
        canonical_bytes(&self.signing_view())
    }

    fn signing_view(&self) -> SigningView<'_> {
        SigningView {
            domain: b"tilo.mls-governance.v1",
            version: self.version,
            transaction_id: self.transaction_id,
            channel: self.channel,
            membership_revision: self.membership_revision,
            previous_frame_digest: self.previous_frame_digest,
            epoch: self.epoch,
            action: &self.action,
            signed_by: self.signed_by,
        }
    }

    /// SHA-256 link value covering the signature as well as all metadata.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalError`] if the complete frame cannot be encoded.
    pub fn digest(&self) -> Result<[u8; 32], CanonicalError> {
        Ok(Sha256::digest(canonical_bytes(self)?).into())
    }

    /// Verify the frame's detached signature without applying live policy.
    ///
    /// # Errors
    ///
    /// Returns [`VerifyError`] for malformed keys, encoding failures, or a
    /// signature mismatch.
    pub fn verify_signature(&self) -> Result<(), VerifyError> {
        verify_event(&self.signing_view(), &self.signature, &self.signed_by)
    }

    /// Validate signature, replay link, live revision/capability, membership
    /// transition, payload bounds, and migration acknowledgement.
    ///
    /// # Errors
    ///
    /// Returns [`MlsGovernanceError`] when any cryptographic or live-policy
    /// precondition fails.
    pub fn validate(&self, context: &MlsGovernanceContext<'_>) -> Result<(), MlsGovernanceError> {
        if self.version != MLS_GOVERNANCE_VERSION {
            return Err(MlsGovernanceError::Version(self.version));
        }
        let route_matches = self.channel == context.channel;
        let revision_matches = self.membership_revision == context.membership_revision;
        let head_matches = self.previous_frame_digest == context.previous_frame_digest;
        let epoch_matches = self.epoch == context.expected_epoch;
        if !(route_matches && revision_matches && head_matches && epoch_matches) {
            return Err(MlsGovernanceError::StaleOrMisrouted);
        }
        if !context.governors.contains(&self.signed_by) {
            return Err(MlsGovernanceError::Unauthorized);
        }
        self.verify_signature()
            .map_err(|error: VerifyError| MlsGovernanceError::Signature(error.to_string()))?;
        match &self.action {
            action @ (MlsGovernanceAction::Add { .. }
            | MlsGovernanceAction::AddWithKeyHistory { .. }) => {
                let Some((recipient, welcome, existing_member_commit, key_history)) =
                    action.admission()
                else {
                    return Err(MlsGovernanceError::InvalidControlPayload);
                };
                if !context.desired_members.contains(recipient)
                    || context.current_mls_members.contains(recipient)
                {
                    return Err(MlsGovernanceError::MembershipMismatch);
                }
                let needs_commit = context.current_mls_members.len() > 1;
                let bad = |v: &[u8]| v.is_empty() || v.len() > MAX_MLS_CONTROL_BYTES;
                if bad(welcome)
                    || needs_commit != existing_member_commit.is_some()
                    || existing_member_commit.is_some_and(bad)
                    || key_history.is_some_and(bad)
                {
                    return Err(MlsGovernanceError::InvalidControlPayload);
                }
            }
            MlsGovernanceAction::Remove { removed, commit } => {
                if !context.current_mls_members.contains(removed)
                    || context.desired_members.contains(removed)
                {
                    return Err(MlsGovernanceError::MembershipMismatch);
                }
                if commit.is_empty() || commit.len() > MAX_MLS_CONTROL_BYTES {
                    return Err(MlsGovernanceError::InvalidControlPayload);
                }
            }
            MlsGovernanceAction::EnableEncryption {
                encrypted_snapshot_digest,
            } => {
                if context.current_mls_members != context.desired_members {
                    return Err(MlsGovernanceError::MembershipMismatch);
                }
                if context.durable_snapshot_ack != Some(*encrypted_snapshot_digest) {
                    return Err(MlsGovernanceError::SnapshotNotDurable);
                }
            }
            MlsGovernanceAction::Genesis => {
                let only_issuer = context.current_mls_members.len() == 1
                    && context.current_mls_members.contains(&self.signed_by);
                if self.previous_frame_digest != [0; 32] || !only_issuer {
                    return Err(MlsGovernanceError::MembershipMismatch);
                }
            }
        }
        Ok(())
    }
}

/// Version of [`MlsAccessRequestV1`] and [`MlsGovernanceAckV1`].
pub const MLS_ACCESS_VERSION: u16 = 1;

/// One device asking a channel's steward for membership: a fresh MLS key
/// package for that channel, signed by the device's certified key.
///
/// The steward admits the device only when the channel's access policy
/// (room membership through the person's certified device set, or a live
/// service grant) names this device key. The signature binds the key
/// package to the device, so a relay that stores or forwards the request
/// cannot substitute its own package.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MlsAccessRequestV1 {
    pub version: u16,
    pub channel: ChannelId,
    /// Certified device key of the requester; also its MLS credential name.
    pub device: Ed25519PublicKey,
    /// TLS-serialised MLS `KeyPackage` minted for this channel only.
    pub key_package: Vec<u8>,
    /// When the device minted the request (seconds since the Unix epoch),
    /// so a steward keeps only the newest request per device.
    pub issued_at: u64,
    pub signature: Signature,
}

#[derive(Serialize)]
struct AccessRequestView<'a> {
    domain: &'static [u8],
    version: u16,
    channel: ChannelId,
    device: Ed25519PublicKey,
    key_package: &'a [u8],
    issued_at: u64,
}

impl MlsAccessRequestV1 {
    /// Create and sign a request with the device key.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalError`] if the signing view cannot be encoded.
    pub fn signed(
        channel: ChannelId,
        key_package: Vec<u8>,
        issued_at: u64,
        device_key: &SigningKey,
    ) -> Result<Self, CanonicalError> {
        let mut request = Self {
            version: MLS_ACCESS_VERSION,
            channel,
            device: Ed25519PublicKey::from_bytes(device_key.verifying_key().to_bytes()),
            key_package,
            issued_at,
            signature: Signature::from_bytes([0; 64]),
        };
        request.signature = sign_event(&request.signing_view(), device_key)?;
        Ok(request)
    }

    /// A request for `device` whose signature is still to be made by a
    /// key this process cannot hold (a browser's non-extractable `WebCrypto`
    /// key): sign [`Self::signing_bytes`] and store it in `signature`.
    pub fn unsigned(
        channel: ChannelId,
        device: Ed25519PublicKey,
        key_package: Vec<u8>,
        issued_at: u64,
    ) -> Self {
        Self {
            version: MLS_ACCESS_VERSION,
            channel,
            device,
            key_package,
            issued_at,
            signature: Signature::from_bytes([0; 64]),
        }
    }

    /// The bytes the device key signs.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalError`] if the signing view cannot be encoded.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, CanonicalError> {
        canonical_bytes(&self.signing_view())
    }

    fn signing_view(&self) -> AccessRequestView<'_> {
        AccessRequestView {
            domain: b"tilo.mls-access-request.v1",
            version: self.version,
            channel: self.channel,
            device: self.device,
            key_package: &self.key_package,
            issued_at: self.issued_at,
        }
    }

    /// Verify the version, payload bound and device signature.
    ///
    /// # Errors
    ///
    /// Returns [`MlsGovernanceError`] for another version, an empty or
    /// oversized key package, or a bad signature.
    pub fn verify(&self) -> Result<(), MlsGovernanceError> {
        if self.version != MLS_ACCESS_VERSION {
            return Err(MlsGovernanceError::Version(self.version));
        }
        if self.key_package.is_empty() || self.key_package.len() > MAX_MLS_CONTROL_BYTES {
            return Err(MlsGovernanceError::InvalidControlPayload);
        }
        verify_event(&self.signing_view(), &self.signature, &self.device)
            .map_err(|error| MlsGovernanceError::Signature(error.to_string()))
    }
}

/// A device's cumulative acknowledgement: it has durably processed every
/// governance frame of `channel` up to and including `through_digest`.
///
/// Cumulative acknowledgements keep delivery idempotent and ordered: a
/// repeated or late acknowledgement never undoes what the steward already
/// recorded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MlsGovernanceAckV1 {
    pub version: u16,
    pub channel: ChannelId,
    pub device: Ed25519PublicKey,
    pub through_digest: [u8; 32],
    pub signature: Signature,
}

#[derive(Serialize)]
struct AckView {
    domain: &'static [u8],
    version: u16,
    channel: ChannelId,
    device: Ed25519PublicKey,
    through_digest: [u8; 32],
}

impl MlsGovernanceAckV1 {
    /// Create and sign an acknowledgement with the device key.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalError`] if the signing view cannot be encoded.
    pub fn signed(
        channel: ChannelId,
        through_digest: [u8; 32],
        device_key: &SigningKey,
    ) -> Result<Self, CanonicalError> {
        let mut ack = Self {
            version: MLS_ACCESS_VERSION,
            channel,
            device: Ed25519PublicKey::from_bytes(device_key.verifying_key().to_bytes()),
            through_digest,
            signature: Signature::from_bytes([0; 64]),
        };
        ack.signature = sign_event(&ack.signing_view(), device_key)?;
        Ok(ack)
    }

    /// An acknowledgement whose signature is still to be made, see
    /// [`MlsAccessRequestV1::unsigned`].
    pub fn unsigned(
        channel: ChannelId,
        device: Ed25519PublicKey,
        through_digest: [u8; 32],
    ) -> Self {
        Self {
            version: MLS_ACCESS_VERSION,
            channel,
            device,
            through_digest,
            signature: Signature::from_bytes([0; 64]),
        }
    }

    /// The bytes the device key signs.
    ///
    /// # Errors
    ///
    /// Returns [`CanonicalError`] if the signing view cannot be encoded.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, CanonicalError> {
        canonical_bytes(&self.signing_view())
    }

    fn signing_view(&self) -> AckView {
        AckView {
            domain: b"tilo.mls-governance-ack.v1",
            version: self.version,
            channel: self.channel,
            device: self.device,
            through_digest: self.through_digest,
        }
    }

    /// Verify the version and device signature.
    ///
    /// # Errors
    ///
    /// Returns [`MlsGovernanceError`] for another version or a bad signature.
    pub fn verify(&self) -> Result<(), MlsGovernanceError> {
        if self.version != MLS_ACCESS_VERSION {
            return Err(MlsGovernanceError::Version(self.version));
        }
        verify_event(&self.signing_view(), &self.signature, &self.device)
            .map_err(|error| MlsGovernanceError::Signature(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate_keypair;

    #[test]
    fn access_request_is_bound_to_its_device_channel_and_package() {
        let (device, device_pk) = generate_keypair();
        let channel = ChannelId::new();
        let request = MlsAccessRequestV1::signed(channel, vec![1, 2, 3], 7, &device).unwrap();
        assert_eq!(request.device, device_pk);
        request.verify().unwrap();

        let mut swapped = request.clone();
        swapped.key_package = vec![9];
        assert!(swapped.verify().is_err());
        let mut moved = request.clone();
        moved.channel = ChannelId::new();
        assert!(moved.verify().is_err());
        let (_, other) = generate_keypair();
        let mut impersonated = request;
        impersonated.device = other;
        assert!(impersonated.verify().is_err());
        let empty = MlsAccessRequestV1::signed(channel, Vec::new(), 7, &device).unwrap();
        assert_eq!(
            empty.verify(),
            Err(MlsGovernanceError::InvalidControlPayload)
        );
    }

    #[test]
    fn acknowledgement_is_bound_to_its_device_and_digest() {
        let (device, _) = generate_keypair();
        let ack = MlsGovernanceAckV1::signed(ChannelId::new(), [4; 32], &device).unwrap();
        ack.verify().unwrap();
        let mut forged = ack;
        forged.through_digest = [5; 32];
        assert!(forged.verify().is_err());
    }

    #[test]
    fn a_frame_signed_elsewhere_matches_one_signed_here() {
        let (steward, steward_pk) = generate_keypair();
        let channel = ChannelId::new();
        let signed = MlsGovernanceFrameV1::signed(
            channel,
            [2; 32],
            1,
            [0; 32],
            0,
            MlsGovernanceAction::Genesis,
            &steward,
        )
        .unwrap();
        let mut unsigned = MlsGovernanceFrameV1::unsigned(
            channel,
            [2; 32],
            1,
            [0; 32],
            0,
            MlsGovernanceAction::Genesis,
            steward_pk,
        );
        assert!(unsigned.verify_signature().is_err());
        let message = unsigned.signing_bytes().unwrap();
        unsigned.signature =
            Signature::from_bytes(ed25519_dalek::Signer::sign(&steward, &message).to_bytes());
        unsigned.verify_signature().unwrap();
        assert_eq!(unsigned, signed);
    }

    #[test]
    fn genesis_must_open_the_chain_with_only_its_issuer() {
        let (steward, steward_pk) = generate_keypair();
        let (_, other) = generate_keypair();
        let channel = ChannelId::new();
        let governors = BTreeSet::from([steward_pk]);
        let only_steward = BTreeSet::from([steward_pk]);
        let crowded = BTreeSet::from([steward_pk, other]);
        let context = |previous, members| MlsGovernanceContext {
            channel,
            membership_revision: 1,
            previous_frame_digest: previous,
            expected_epoch: 0,
            governors: &governors,
            desired_members: members,
            current_mls_members: members,
            durable_snapshot_ack: None,
        };
        let genesis = |previous| {
            MlsGovernanceFrameV1::signed(
                channel,
                [1; 32],
                1,
                previous,
                0,
                MlsGovernanceAction::Genesis,
                &steward,
            )
            .unwrap()
        };
        genesis([0; 32])
            .validate(&context([0; 32], &only_steward))
            .unwrap();
        assert_eq!(
            genesis([0; 32]).validate(&context([0; 32], &crowded)),
            Err(MlsGovernanceError::MembershipMismatch)
        );
        assert_eq!(
            genesis([3; 32]).validate(&context([3; 32], &only_steward)),
            Err(MlsGovernanceError::MembershipMismatch)
        );
    }
}
