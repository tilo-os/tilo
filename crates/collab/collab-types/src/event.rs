//! The append-only governance log: `ChannelEvent`.
//!
//! Every channel-level governance action (genesis, attestation
//! issuance/revocation, ownership transfer, rename, settings update) is
//! recorded as a `ChannelEvent`. Peers replay the event log to
//! reconstruct the channel's grants; the validator consults
//! that state to decide whether an inbound `SignedOp` is valid.
//!
//! Each variant carries its own `signature`, produced by a per-variant
//! `signing_bytes()` helper mirroring the payload minus the signature.
//! Verifiers reconstruct the same view and check against the claimed
//! signer's public key.
//!
//! A tagged enum (not a `kind: String + payload: bytes` envelope) so
//! postcard prefixes each event with a discriminant byte for the
//! receiver to pattern-match — the cleanest fit for six fixed shapes.

use serde::{Deserialize, Serialize};
use web_time::SystemTime;

use crate::canonical::{CanonicalError, canonical_bytes};
use crate::channel::{ChannelId, ChannelSettingsDelta};
use crate::crypto::{Ed25519PublicKey, Signature};
use crate::identity::{Attestation, AttestationId};
use crate::role::Role;
use crate::space::SpaceId;

/// Signed attestation granting a role to a member in a channel.
///
/// Carried inside `ChannelEvent::AttestationIssued`. `role` declares
/// which channel role the bundled `attestation` underwrites — stored
/// here (rather than re-derived from the attestation's claims) to keep
/// the role lookup O(1) at validation time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleAttestation {
    /// Channel this role grant applies to — attestations are scoped per
    /// channel, so a member can be `Owner` of one and `Viewer` of another.
    pub channel: ChannelId,
    /// The role being granted.
    pub role: Role,
    /// The signed attestation record. Its `subject` is the member receiving
    /// the role; its `issued_by` must trace to the channel's `owner`
    /// (directly or via a delegated trust chain).
    pub attestation: Attestation,
}

/// Channel-level governance events, replayed in order to reconstruct the
/// channel's state. Each variant carries a signature by `signed_by`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ChannelEvent {
    /// First event of a channel's history, signing the channel-creation
    /// parameters. `owner` is the implicit signer.
    ///
    /// A channel without a `Genesis` event is rejected by the validator — it
    /// is the trust anchor every later event's signer is checked against.
    Genesis {
        /// New channel id, also embedded so a stray genesis event floating in a
        /// buffer is self-describing.
        channel_id: ChannelId,
        /// Parent space.
        space: SpaceId,
        /// Display name at creation time; renamed later via `ChannelEvent::Rename`.
        name: String,
        /// Owner identity. Equivalent to the signer for this event.
        owner: Ed25519PublicKey,
        /// Wall-clock time of creation, carried so peers can display "Created
        /// on …" without scanning history.
        created_at: SystemTime,
        /// Detached signature by `owner` over the canonical-bytes view minus this field.
        signature: Signature,
    },
    /// Mint a new attestation, signed by the issuer (channel owner or a
    /// delegated authority) — `attestation.attestation.issued_by`.
    AttestationIssued(RoleAttestation),
    /// Revoke a previously-issued attestation by id, signed by the channel
    /// owner (or a delegated revoke authority).
    AttestationRevoked {
        /// Id of the attestation being revoked.
        attestation: AttestationId,
        /// Signer of this revocation — must be `Channel::owner` or hold a
        /// `Govern` grant.
        signed_by: Ed25519PublicKey,
        signature: Signature,
    },
    /// Change the channel's display name. Signed by the owner.
    Rename {
        /// New display name.
        new_name: String,
        /// Signer of this rename.
        signed_by: Ed25519PublicKey,
        signature: Signature,
    },
    /// Apply a `ChannelSettingsDelta`. Signed by the owner.
    SettingsUpdate {
        /// The delta; each field is `Option<…>` and `None` leaves the existing
        /// setting untouched.
        delta: ChannelSettingsDelta,
        /// Signer of this update.
        signed_by: Ed25519PublicKey,
        /// Detached signature by `signed_by`.
        signature: Signature,
    },
    /// A new member was admitted to the channel's MLS group.
    ///
    /// Carries the MLS `Welcome` message (TLS-serialised by
    /// `channel_crypto`) for delivery to the joining endpoint. A production
    /// broker must durably store both the event and its targeted Welcome
    /// before reporting admission complete. Replaying a Welcome alone cannot
    /// reconstruct a lost endpoint key package or group secret.
    ///
    /// `welcome` bytes are opaque to the validator — only the recipient
    /// (whose `KeyPackage` the Welcome references) can decrypt them. The
    /// signature is over (tag, welcome, `signed_by`), so the broker can verify
    /// the addition was issued by an authorised member.
    MemberAdded {
        /// TLS-serialised MLS `Welcome` message bytes, opaque to non-recipients.
        welcome: Vec<u8>,
        /// Identity that issued the addition — must be authorised to add members
        /// (channel owner, or a delegate holding an equivalent `Govern` grant;
        /// enforced against the author's grants on replay).
        signed_by: Ed25519PublicKey,
        /// Detached signature by `signed_by` over the canonical view minus this field.
        signature: Signature,
    },
    /// A member was removed from the channel's MLS group.
    ///
    /// Carries the MLS commit bytes surviving members must process to
    /// advance their epoch (removal produces a commit, not a Welcome). Shape
    /// mirrors `MemberAdded` so the broker code path stays symmetric.
    ///
    /// `channel_crypto::ChannelGroup::remove_member` produces these commit
    /// bytes. The authoritative broker must durably append and fan them out
    /// before reporting the governance change as complete.
    MemberRemoved {
        /// TLS-serialised MLS commit bytes advancing the survivors' epoch,
        /// opaque outside the group.
        removal: Vec<u8>,
        /// Identity that issued the removal.
        signed_by: Ed25519PublicKey,
        /// Detached signature by `signed_by` over the canonical view minus this field.
        signature: Signature,
    },
}

impl ChannelEvent {
    /// Canonical bytes signed for this event variant.
    ///
    /// Each variant constructs a "view" mirroring its payload minus
    /// `signature`. `RoleAttestation` signs over the embedded `Attestation`'s
    /// own `signing_bytes`, so the validator can re-verify both the outer
    /// event signature and the inner attestation signature independently.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, CanonicalError> {
        match self {
            ChannelEvent::Genesis {
                channel_id,
                space,
                name,
                owner,
                created_at,
                signature: _,
            } => genesis_signing_bytes(channel_id, space, name, owner, *created_at),
            ChannelEvent::AttestationIssued(ra) => attestation_issued_signing_bytes(ra),
            ChannelEvent::AttestationRevoked {
                attestation,
                signed_by,
                signature: _,
            } => attestation_revoked_signing_bytes(attestation, signed_by),
            ChannelEvent::Rename {
                new_name,
                signed_by,
                signature: _,
            } => rename_signing_bytes(new_name, signed_by),
            ChannelEvent::SettingsUpdate {
                delta,
                signed_by,
                signature: _,
            } => settings_update_signing_bytes(delta, signed_by),
            ChannelEvent::MemberAdded {
                welcome,
                signed_by,
                signature: _,
            } => member_added_signing_bytes(welcome, signed_by),
            ChannelEvent::MemberRemoved {
                removal,
                signed_by,
                signature: _,
            } => member_removed_signing_bytes(removal, signed_by),
        }
    }

    /// The signer of this event, as declared by the event: `owner` for
    /// `Genesis`, `signed_by` for the others. Used to pick the public key to
    /// verify against.
    pub fn signer(&self) -> Ed25519PublicKey {
        match self {
            ChannelEvent::Genesis { owner, .. } => *owner,
            ChannelEvent::AttestationIssued(ra) => ra.attestation.issued_by,
            ChannelEvent::AttestationRevoked { signed_by, .. }
            | ChannelEvent::Rename { signed_by, .. }
            | ChannelEvent::SettingsUpdate { signed_by, .. }
            | ChannelEvent::MemberAdded { signed_by, .. }
            | ChannelEvent::MemberRemoved { signed_by, .. } => *signed_by,
        }
    }

    /// The signature carried by this event variant.
    pub fn signature(&self) -> &Signature {
        match self {
            ChannelEvent::Genesis { signature, .. }
            | ChannelEvent::AttestationRevoked { signature, .. }
            | ChannelEvent::Rename { signature, .. }
            | ChannelEvent::SettingsUpdate { signature, .. }
            | ChannelEvent::MemberAdded { signature, .. }
            | ChannelEvent::MemberRemoved { signature, .. } => signature,
            ChannelEvent::AttestationIssued(ra) => &ra.attestation.signature,
        }
    }
}

/// `Genesis`'s signing view (tag 0).
fn genesis_signing_bytes(
    channel_id: &ChannelId,
    space: &SpaceId,
    name: &str,
    owner: &Ed25519PublicKey,
    created_at: SystemTime,
) -> Result<Vec<u8>, CanonicalError> {
    #[derive(Serialize)]
    struct View<'a> {
        tag: u8,
        channel_id: &'a ChannelId,
        space: &'a SpaceId,
        name: &'a str,
        owner: &'a Ed25519PublicKey,
        created_at: SystemTime,
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

/// `AttestationIssued`'s signing view (tag 1) — folds in the embedded
/// attestation's own `signing_bytes` so both signatures are independently
/// re-verifiable.
fn attestation_issued_signing_bytes(ra: &RoleAttestation) -> Result<Vec<u8>, CanonicalError> {
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

/// `AttestationRevoked`'s signing view (tag 2).
fn attestation_revoked_signing_bytes(
    attestation: &AttestationId,
    signed_by: &Ed25519PublicKey,
) -> Result<Vec<u8>, CanonicalError> {
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

/// `Rename`'s signing view (tag 4).
fn rename_signing_bytes(
    new_name: &str,
    signed_by: &Ed25519PublicKey,
) -> Result<Vec<u8>, CanonicalError> {
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

/// `SettingsUpdate`'s signing view (tag 5).
fn settings_update_signing_bytes(
    delta: &ChannelSettingsDelta,
    signed_by: &Ed25519PublicKey,
) -> Result<Vec<u8>, CanonicalError> {
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

/// `MemberAdded`'s signing view (tag 6).
fn member_added_signing_bytes(
    welcome: &[u8],
    signed_by: &Ed25519PublicKey,
) -> Result<Vec<u8>, CanonicalError> {
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

/// `MemberRemoved`'s signing view (tag 7).
fn member_removed_signing_bytes(
    removal: &[u8],
    signed_by: &Ed25519PublicKey,
) -> Result<Vec<u8>, CanonicalError> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::from_canonical_bytes;
    use crate::crypto::{generate_keypair, sign_event, verify_event};

    /// Genesis, member-added and member-removed events round-trip canonically.
    #[test]
    fn channel_events_round_trip_canonically() {
        // Phase: genesis_round_trip.
        {
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

        // Phase: member_added_round_trip.
        {
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

        // Phase: member_removed_round_trip.
        {
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
    }

    /// Genesis and member-added events sign and verify, added and removed have distinct signing bytes,
    /// and every variant names its signer.
    #[test]
    fn channel_events_sign_over_distinct_views_and_name_their_signer() {
        // Phase: genesis_sign_and_verify.
        {
            let (sk, pk) = generate_keypair();
            let cid = ChannelId::new();
            let sid = SpaceId::new();
            let now = SystemTime::UNIX_EPOCH;

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
            let view = g.signing_bytes().unwrap();
            assert!(verify_event(&view, &sig, &pk).is_ok());
        }

        // Phase: member_added_sign_and_verify.
        {
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

        // Phase: member_added_and_removed_have_distinct_signing_bytes.
        {
            // Same payload byte sequence; different tag must give different
            // signing bytes so a peer can't replay one signature on the other variant.
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

        // Phase: signer_for_each_variant.
        {
            let (_, k) = generate_keypair();
            let v1 = ChannelEvent::AttestationRevoked {
                attestation: AttestationId::new(),
                signed_by: k,
                signature: Signature::from_bytes([0u8; 64]),
            };
            assert_eq!(v1.signer(), k);
        }
    }
}
