//! Spaces — the social-formation container that groups channels.
//!
//! A space is the outermost layer of the qOS collaboration model.
//! Membership in a space is rare-ish (you join an org once); inside
//! the space you create + leave + rejoin channels constantly. See
//! the design spec's "Five primitives" table for the full picture.
//!
//! A space carries:
//!
//! - a trust root: the Ed25519 public key authorised to issue + revoke
//!   attestations for this space's members. Initially the space
//!   creator's identity; can be delegated by signing additional
//!   attestations rooted in the original key;
//! - a default identity label so a user with multiple identities knows
//!   which one to present when entering this space;
//! - a cached member directory + a list of channels.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::channel::ChannelRef;
use crate::crypto::Ed25519PublicKey;
use crate::identity::Identity;

/// Stable id for a space. UUID v4 for the same reasons as
/// [`crate::identity::AttestationId`]: globally unique without
/// coordination.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SpaceId(pub Uuid);

impl SpaceId {
    /// Mint a fresh space id.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for SpaceId {
    fn default() -> Self {
        Self::new()
    }
}

/// A social-formation container.
///
/// The fields mirror the spec. `member_directory` is a cache like
/// `Identity::attestations` — what the local device has seen, not an
/// authoritative roster. The authoritative roster is reconstructable
/// from the union of valid attestations issued by the `trust_root`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Space {
    /// Stable id for this space.
    pub id: SpaceId,
    /// Human-meaningful name. "ACME Corporation", "Household",
    /// "Personal". Not unique across the user's spaces.
    pub name: String,
    /// Public key authorised to sign attestations for this space's
    /// members. Initially the space creator's identity public key.
    /// Delegation: the trust root can sign a secondary attestation
    /// granting another key the same authority, recursively.
    pub trust_root: Ed25519PublicKey,
    /// Which identity the user should present when entering this
    /// space. `None` means "ask the user". Stored as a label rather
    /// than an `Ed25519PublicKey` so the field survives identity
    /// rotation; the identity-switcher resolves label → key at
    /// switch time.
    pub default_identity_label: Option<String>,
    /// Channels inside this space, by id. Order is the user's
    /// preferred order in the wheel disc / channel browser.
    pub channels: Vec<ChannelRef>,
    /// Everyone the space knows about, cached. Re-fetch from the
    /// trust root for an authoritative answer.
    pub member_directory: Vec<Identity>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::{canonical_bytes, from_canonical_bytes};
    use crate::crypto::generate_keypair;

    #[test]
    fn space_id_unique() {
        assert_ne!(SpaceId::new(), SpaceId::new());
    }

    #[test]
    fn empty_space_round_trip() {
        let (_, pk) = generate_keypair();
        let s = Space {
            id: SpaceId::new(),
            name: "Personal".into(),
            trust_root: pk,
            default_identity_label: Some("me".into()),
            channels: vec![],
            member_directory: vec![],
        };
        let bytes = canonical_bytes(&s).unwrap();
        let back: Space = from_canonical_bytes(&bytes).unwrap();
        assert_eq!(s, back);
    }
}
