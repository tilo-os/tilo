//! The append-only governance log: `ChannelEvent`.
//!
//! Every channel-level governance action (genesis, attestation
//! issuance, attestation revocation, ownership transfer, rename,
//! settings update) is recorded as a `ChannelEvent`. Peers replay
//! the event log to reconstruct the channel's current role table;
//! the validator consults that reconstructed state to decide
//! whether an inbound `SignedOp` is valid.
//!
//! Each variant carries its own `signature` field. The signed bytes
//! are produced by a per-variant `signing_bytes()` helper that
//! mirrors the variant's payload minus the signature. Verifiers
//! reconstruct the same view and check the signature against the
//! claimed signer's public key.
//!
//! ## Why a tagged enum
//!
//! The spec lists six variants; we use a tagged enum so postcard
//! prefixes each event with a discriminant byte and the receiver
//! pattern-matches. This is the cleanest representation for a small
//! fixed set of message shapes; we don't need the open-extension
//! flexibility of a `kind: String + payload: bytes` envelope.

use serde::{Deserialize, Serialize};
use std::time::SystemTime;

use crate::canonical::{CanonicalError, canonical_bytes};
use crate::channel::{ChannelId, ChannelSettingsDelta};
use crate::crypto::{Ed25519PublicKey, Signature};
use crate::identity::{Attestation, AttestationId};
use crate::role::Role;
use crate::space::SpaceId;

/// Signed attestation granting a role to a member in a channel.
///
/// Carried inside `ChannelEvent::AttestationIssued`. The
/// `attestation` field is the full immutable `Attestation` record;
/// the `role` field declares which channel role the bundled
/// attestation underwrites. Storing the role here (rather than
/// re-deriving it from the attestation's claims) keeps the role
/// lookup O(1) at validation time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleAttestation {
    /// Channel this role grant applies to. The validator scopes
    /// attestations per channel; a member can be `Owner` of one
    /// channel and `Viewer` of another in the same space.
    pub channel: ChannelId,
    /// The role being granted.
    pub role: Role,
    /// The signed attestation record. Its `subject` is the member
    /// receiving the role; its `issued_by` must trace to the
    /// channel's `owner` (directly or via a delegated trust chain).
    pub attestation: Attestation,
}

/// Channel-level governance events. Replayed in order to reconstruct
/// the channel's current state. Each variant carries a signature by
/// `signed_by` over the variant's pre-signature view.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ChannelEvent {
    /// First event of a channel's history. Signs the
    /// channel-creation parameters. `owner` is also the implicit
    /// signer; the genesis event's signature is verified against
    /// `owner`'s public key.
    ///
    /// A channel without a `Genesis` event is rejected by the
    /// validator; the genesis event is the trust anchor that lets
    /// every later event's signer be checked.
    Genesis {
        /// New channel id, also embedded so a stray genesis event
        /// floating in a buffer is self-describing.
        channel_id: ChannelId,
        /// Parent space.
        space: SpaceId,
        /// Display name at creation time. Renamed later via
        /// `ChannelEvent::Rename`.
        name: String,
        /// Owner identity. Equivalent to the signer for this event.
        owner: Ed25519PublicKey,
        /// Wall-clock time of creation. Carried so peers can
        /// display "Created on …" without scanning history.
        created_at: SystemTime,
        /// Detached signature by `owner` over the canonical-bytes
        /// view of this variant minus this field.
        signature: Signature,
    },
    /// Mint a new attestation. Signed by the issuer (channel owner
    /// or a delegated authority); the issuer is `attestation.attestation.issued_by`.
    AttestationIssued(RoleAttestation),
    /// Revoke a previously-issued attestation by id. Signed by the
    /// channel owner (or a delegated revoke authority).
    AttestationRevoked {
        /// Id of the attestation being revoked.
        attestation: AttestationId,
        /// Signer of this revocation. Must be authorised to revoke
        /// (i.e. is `Channel::owner` or holds a `Govern` grant).
        signed_by: Ed25519PublicKey,
        /// Detached signature by `signed_by`.
        signature: Signature,
    },
    /// Transfer channel ownership to a different identity. Signed
    /// by the current owner.
    OwnershipTransfer {
        /// The incoming owner's public key.
        new_owner: Ed25519PublicKey,
        /// Signer of this transfer. Must equal the current
        /// `Channel::owner` at the time of the event.
        signed_by: Ed25519PublicKey,
        /// Detached signature by `signed_by`.
        signature: Signature,
    },
    /// Change the channel's display name. Signed by the owner.
    Rename {
        /// New display name.
        new_name: String,
        /// Signer of this rename.
        signed_by: Ed25519PublicKey,
        /// Detached signature by `signed_by`.
        signature: Signature,
    },
    /// Apply a `ChannelSettingsDelta`. Signed by the owner.
    SettingsUpdate {
        /// The delta. Each field is `Option<…>`; `None` leaves the
        /// existing setting untouched.
        delta: ChannelSettingsDelta,
        /// Signer of this update.
        signed_by: Ed25519PublicKey,
        /// Detached signature by `signed_by`.
        signature: Signature,
    },
}

impl ChannelEvent {
    /// Canonical bytes signed for this event variant.
    ///
    /// Each variant constructs a "view" struct mirroring the
    /// variant's payload but omitting the `signature` field. The
    /// `RoleAttestation` variant signs over the embedded
    /// `Attestation`'s own `signing_bytes` so the validator can
    /// re-verify both the outer event signature *and* the inner
    /// attestation signature independently.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, CanonicalError> {
        match self {
            ChannelEvent::Genesis {
                channel_id,
                space,
                name,
                owner,
                created_at,
                signature: _,
            } => {
                #[derive(Serialize)]
                struct View<'a> {
                    tag: u8,
                    channel_id: &'a ChannelId,
                    space: &'a SpaceId,
                    name: &'a str,
                    owner: &'a Ed25519PublicKey,
                    created_at: &'a SystemTime,
                }
                canonical_bytes(&View {
                    tag: 0,
                    channel_id,
                    space,
                    name,
                    owner,
                    created_at,
                })
            }
            ChannelEvent::AttestationIssued(ra) => {
                #[derive(Serialize)]
                struct View<'a> {
                    tag: u8,
                    channel: &'a ChannelId,
                    role: &'a Role,
                    attestation_signing_bytes: Vec<u8>,
                }
                let inner = ra.attestation.signing_bytes()?;
                canonical_bytes(&View {
                    tag: 1,
                    channel: &ra.channel,
                    role: &ra.role,
                    attestation_signing_bytes: inner,
                })
            }
            ChannelEvent::AttestationRevoked {
                attestation,
                signed_by,
                signature: _,
            } => {
                #[derive(Serialize)]
                struct View<'a> {
                    tag: u8,
                    attestation: &'a AttestationId,
                    signed_by: &'a Ed25519PublicKey,
                }
                canonical_bytes(&View {
                    tag: 2,
                    attestation,
                    signed_by,
                })
            }
            ChannelEvent::OwnershipTransfer {
                new_owner,
                signed_by,
                signature: _,
            } => {
                #[derive(Serialize)]
                struct View<'a> {
                    tag: u8,
                    new_owner: &'a Ed25519PublicKey,
                    signed_by: &'a Ed25519PublicKey,
                }
                canonical_bytes(&View {
                    tag: 3,
                    new_owner,
                    signed_by,
                })
            }
            ChannelEvent::Rename {
                new_name,
                signed_by,
                signature: _,
            } => {
                #[derive(Serialize)]
                struct View<'a> {
                    tag: u8,
                    new_name: &'a str,
                    signed_by: &'a Ed25519PublicKey,
                }
                canonical_bytes(&View {
                    tag: 4,
                    new_name,
                    signed_by,
                })
            }
            ChannelEvent::SettingsUpdate {
                delta,
                signed_by,
                signature: _,
            } => {
                #[derive(Serialize)]
                struct View<'a> {
                    tag: u8,
                    delta: &'a ChannelSettingsDelta,
                    signed_by: &'a Ed25519PublicKey,
                }
                canonical_bytes(&View {
                    tag: 5,
                    delta,
                    signed_by,
                })
            }
        }
    }

    /// The signer of this event, as declared by the event.
    ///
    /// For `Genesis` this is the owner; for the others it is the
    /// `signed_by` field. The validator uses this to pick the
    /// public key to verify against.
    pub fn signer(&self) -> Ed25519PublicKey {
        match self {
            ChannelEvent::Genesis { owner, .. } => *owner,
            ChannelEvent::AttestationIssued(ra) => ra.attestation.issued_by,
            ChannelEvent::AttestationRevoked { signed_by, .. } => *signed_by,
            ChannelEvent::OwnershipTransfer { signed_by, .. } => *signed_by,
            ChannelEvent::Rename { signed_by, .. } => *signed_by,
            ChannelEvent::SettingsUpdate { signed_by, .. } => *signed_by,
        }
    }

    /// The signature carried by this event variant.
    pub fn signature(&self) -> &Signature {
        match self {
            ChannelEvent::Genesis { signature, .. } => signature,
            ChannelEvent::AttestationIssued(ra) => &ra.attestation.signature,
            ChannelEvent::AttestationRevoked { signature, .. } => signature,
            ChannelEvent::OwnershipTransfer { signature, .. } => signature,
            ChannelEvent::Rename { signature, .. } => signature,
            ChannelEvent::SettingsUpdate { signature, .. } => signature,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::from_canonical_bytes;
    use crate::crypto::{generate_keypair, sign_event, verify_event};

    #[test]
    fn genesis_round_trip() {
        let (_, owner) = generate_keypair();
        let g = ChannelEvent::Genesis {
            channel_id: ChannelId::new(),
            space: SpaceId::new(),
            name: "general".into(),
            owner,
            created_at: SystemTime::UNIX_EPOCH,
            signature: Signature::from_bytes([0u8; 64]),
        };
        let bytes = canonical_bytes(&g).unwrap();
        let back: ChannelEvent = from_canonical_bytes(&bytes).unwrap();
        assert_eq!(g, back);
    }

    #[test]
    fn genesis_sign_and_verify() {
        let (sk, pk) = generate_keypair();
        let cid = ChannelId::new();
        let sid = SpaceId::new();
        let now = SystemTime::UNIX_EPOCH;

        // Build the view, sign over it, stamp the signature back in.
        let mut g = ChannelEvent::Genesis {
            channel_id: cid,
            space: sid,
            name: "general".into(),
            owner: pk,
            created_at: now,
            signature: Signature::from_bytes([0u8; 64]),
        };
        let bytes = g.signing_bytes().unwrap();
        let sig = sign_event(&bytes, &sk).unwrap();
        if let ChannelEvent::Genesis { signature, .. } = &mut g {
            *signature = sig;
        }
        // Re-derive view bytes and verify.
        let view = g.signing_bytes().unwrap();
        assert!(verify_event(&view, &sig, &pk).is_ok());
    }

    #[test]
    fn signer_for_each_variant() {
        let (_, k) = generate_keypair();
        let v1 = ChannelEvent::AttestationRevoked {
            attestation: AttestationId::new(),
            signed_by: k,
            signature: Signature::from_bytes([0u8; 64]),
        };
        assert_eq!(v1.signer(), k);
    }
}
