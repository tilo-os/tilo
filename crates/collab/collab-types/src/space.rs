//! Spaces — the social-formation container that groups channels.
//!
//! A space is the outermost layer of the Tilo collaboration model.
//! Membership is rare-ish (you join an org once); inside it you create +
//! leave + rejoin channels constantly.
//!
//! A space carries: a trust root (the Ed25519 key authorised to issue +
//! revoke attestations for its members, initially the creator's identity,
//! delegable by signing additional attestations rooted in the original
//! key); a default identity label so a multi-identity user knows which
//! to present entering this space; a cached member directory + channel list.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::channel::ChannelRef;
use crate::crypto::Ed25519PublicKey;
use crate::identity::Identity;

/// Stable id for a space — UUID v4, globally unique with no coordination.
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

/// A social-formation container. `member_directory` is a cache (like
/// `Identity::attestations`) of what the local device has seen, not an
/// authoritative roster — the authoritative roster is reconstructable
/// from the union of valid attestations issued by `trust_root`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Space {
    /// Stable id for this space.
    pub id: SpaceId,
    /// Human-meaningful name ("ACME Corporation", "Household", "Personal");
    /// not unique across the user's spaces.
    pub name: String,
    /// Public key authorised to sign attestations for this space's members,
    /// initially the creator's. Delegable: the trust root can sign a
    /// secondary attestation granting another key the same authority, recursively.
    pub trust_root: Ed25519PublicKey,
    /// Which identity the user should present entering this space. `None`
    /// means "ask". Stored as a label (not a key) so it survives identity
    /// rotation — the identity-switcher resolves label → key at switch time.
    pub default_identity_label: Option<String>,
    /// Channels inside this space, by id, in the user's preferred wheel-disc order.
    pub channels: Vec<ChannelRef>,
    /// Everyone the space knows about, cached — re-fetch from the trust root
    /// for an authoritative answer.
    pub member_directory: Vec<Identity>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::{canonical_bytes, from_canonical_bytes};
    use crate::crypto::generate_keypair;

    #[test]
    fn space_ids_are_unique_and_an_empty_space_round_trips() {
        assert_ne!(SpaceId::new(), SpaceId::new());

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
