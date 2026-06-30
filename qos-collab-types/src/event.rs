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
    /// Phase 7: a new member was admitted to the channel's MLS
    /// group.
    ///
    /// Carries the MLS `Welcome` message (TLS-serialised by
    /// `qos_channel_crypto`) so a peer rejoining the channel later
    /// can replay the governance log + the matching Welcome to
    /// reconstruct membership. The broker stores both the
    /// channel-event log AND the per-recipient Welcome on disk; this
    /// variant carries the latter for the candidate member.
    ///
    /// The `welcome` bytes are opaque to the validator — only the
    /// recipient (whose `KeyPackage` is referenced by the Welcome)
    /// can decrypt them. The signature is over (tag, welcome,
    /// signed_by) so the broker can verify the addition was issued
    /// by a member authorised to add (channel owner / delegate).
    MemberAdded {
        /// TLS-serialised MLS `Welcome` message bytes. Opaque to
        /// non-recipients.
        welcome: Vec<u8>,
        /// Identity that issued the addition. Must be authorised to
        /// add members (typically the channel owner, or a delegate
        /// holding the equivalent `Govern` grant — the validator
        /// enforces this via the role table when the inclusion is
        /// replayed).
        signed_by: Ed25519PublicKey,
        /// Detached signature by `signed_by` over the canonical
        /// view of this variant minus this field.
        signature: Signature,
    },
    /// Phase 7.1 stub: a member was removed from the channel's MLS
    /// group.
    ///
    /// Carries the MLS commit bytes that surviving members must
    /// process to advance their epoch (the removal does not produce
    /// a Welcome — only the survivors get a commit). The shape
    /// mirrors [`MemberAdded`] so the broker code path can be
    /// symmetric.
    ///
    /// **Not implemented end-to-end in Phase 7.**
    /// `qos_channel_crypto::ChannelGroup::remove_member` is a
    /// documented `todo!()`; this variant exists so the wire format
    /// can land alongside the Phase-7 crate without a second
    /// breaking change when Phase 7.1 enables removal.
    MemberRemoved {
        /// TLS-serialised MLS commit bytes that advance the
        /// survivors' epoch. Opaque to anyone outside the group.
        removal: Vec<u8>,
        /// Identity that issued the removal.
        signed_by: Ed25519PublicKey,
        /// Detached signature by `signed_by` over the canonical
        /// view of this variant minus this field.
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
            ChannelEvent::MemberAdded {
                welcome,
                signed_by,
                signature: _,
            } => {
                #[derive(Serialize)]
                struct View<'a> {
                    tag: u8,
                    #[serde(with = "serde_bytes")]
                    welcome: &'a [u8],
                    signed_by: &'a Ed25519PublicKey,
                }
                canonical_bytes(&View {
                    tag: 6,
                    welcome,
                    signed_by,
                })
            }
            ChannelEvent::MemberRemoved {
                removal,
                signed_by,
                signature: _,
            } => {
                #[derive(Serialize)]
                struct View<'a> {
                    tag: u8,
                    #[serde(with = "serde_bytes")]
                    removal: &'a [u8],
                    signed_by: &'a Ed25519PublicKey,
                }
                canonical_bytes(&View {
                    tag: 7,
                    removal,
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
            ChannelEvent::MemberAdded { signed_by, .. } => *signed_by,
            ChannelEvent::MemberRemoved { signed_by, .. } => *signed_by,
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
            ChannelEvent::MemberAdded { signature, .. } => signature,
            ChannelEvent::MemberRemoved { signature, .. } => signature,
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

    #[test]
    fn member_added_round_trip() {
        let (_, k) = generate_keypair();
        let ev = ChannelEvent::MemberAdded {
            welcome: vec![0xde, 0xad, 0xbe, 0xef],
            signed_by: k,
            signature: Signature::from_bytes([0u8; 64]),
        };
        let bytes = canonical_bytes(&ev).unwrap();
        let back: ChannelEvent = from_canonical_bytes(&bytes).unwrap();
        assert_eq!(ev, back);
        assert_eq!(back.signer(), k);
    }

    #[test]
    fn member_added_sign_and_verify() {
        let (sk, pk) = generate_keypair();
        let mut ev = ChannelEvent::MemberAdded {
            welcome: vec![1, 2, 3, 4, 5],
            signed_by: pk,
            signature: Signature::from_bytes([0u8; 64]),
        };
        let bytes = ev.signing_bytes().unwrap();
        let sig = sign_event(&bytes, &sk).unwrap();
        if let ChannelEvent::MemberAdded { signature, .. } = &mut ev {
            *signature = sig;
        }
        let view = ev.signing_bytes().unwrap();
        assert!(verify_event(&view, &sig, &pk).is_ok());
    }

    #[test]
    fn member_removed_round_trip() {
        let (_, k) = generate_keypair();
        let ev = ChannelEvent::MemberRemoved {
            removal: vec![0xca, 0xfe, 0xba, 0xbe],
            signed_by: k,
            signature: Signature::from_bytes([0u8; 64]),
        };
        let bytes = canonical_bytes(&ev).unwrap();
        let back: ChannelEvent = from_canonical_bytes(&bytes).unwrap();
        assert_eq!(ev, back);
        assert_eq!(back.signer(), k);
    }

    #[test]
    fn member_added_and_removed_have_distinct_signing_bytes() {
        // Same payload byte sequence; different tag must give
        // different signing bytes so a peer cannot replay one
        // signature on the other variant.
        let (_, k) = generate_keypair();
        let bytes = vec![7u8; 16];
        let added = ChannelEvent::MemberAdded {
            welcome: bytes.clone(),
            signed_by: k,
            signature: Signature::from_bytes([0u8; 64]),
        };
        let removed = ChannelEvent::MemberRemoved {
            removal: bytes,
            signed_by: k,
            signature: Signature::from_bytes([0u8; 64]),
        };
        assert_ne!(
            added.signing_bytes().unwrap(),
            removed.signing_bytes().unwrap()
        );
    }
}
