//! The human-facing identity primitive.
//!
//! An `Identity` represents one human across all their devices: a stable
//! Ed25519 public key (the "I am me" anchor), a human-meaningful label
//! and optional avatar, and zero or more `Attestation`s — signed claims
//! about this identity, issued by trust roots (an org's master key, a
//! peer user, etc.).
//!
//! The private half of the keypair is currently held in a single-device
//! file under `~/.config/tilo/identity/`; a future FROST 2-of-N
//! threshold-sharing upgrade changes only how `sign_*` calls are routed,
//! not this type.
//!
//! ## Attestation lifecycle
//!
//! Each attestation has a validity window and a signature from the
//! issuer; verifiers check it traces to an accepted trust root (see
//! [`crate::validator`]). Revocation is a separate event
//! ([`crate::event::ChannelEvent::AttestationRevoked`]) — the attestation
//! itself is immutable.
//!
//! `Identity::attestations` is a *cache* of what the local device has
//! observed, not authoritative — fresh attestations may arrive later.
//! Callers must re-query the issuer (or the channel's live grants) before
//! making a security decision.

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use web_time::SystemTime;

use crate::canonical::{CanonicalError, canonical_bytes};
use crate::crypto::{Ed25519PublicKey, Signature};

/// Stable identifier for an attestation — a UUID v4 (unique across
/// issuers with no coordination), assigned by the issuer at mint time,
/// referenced from `MemberRef::attestation` and `SignedOp::attestation_id`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AttestationId(pub Uuid);

impl AttestationId {
    /// Mint a fresh attestation id.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for AttestationId {
    fn default() -> Self {
        Self::new()
    }
}

/// Security-clearance level for classified artifacts.
///
/// A `u8` so comparisons work directly: `0` Public (no restriction), `1`
/// Internal (any org member), `2` Confidential (cleared subset), `3`
/// Restricted (smallest cleared set). Higher = stricter; the check is
/// `identity_clearance >= artifact_classification`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Clearance(pub u8);

impl Clearance {
    /// Public — no clearance requirement. Equivalent to `Clearance(0)`.
    pub const PUBLIC: Self = Clearance(0);
    /// Internal — visible to any member of the issuing org.
    pub const INTERNAL: Self = Clearance(1);
    /// Confidential — restricted to cleared subset.
    pub const CONFIDENTIAL: Self = Clearance(2);
    /// Restricted — smallest cleared set.
    pub const RESTRICTED: Self = Clearance(3);
}

/// A signed claim about an identity, issued by a trust root.
///
/// Trust roots are Ed25519 public keys the validator must be configured
/// with per channel (see
/// [`crate::validator::ChannelState::accepted_trust_roots`]). `signature`
/// covers everything else — see `signing_bytes`. Revoking produces a
/// separate event; this struct is immutable once minted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attestation {
    /// Unique id, set by the issuer at mint time.
    pub id: AttestationId,
    /// Trust root that issued this attestation; the signature must verify
    /// against this key.
    pub issued_by: Ed25519PublicKey,
    /// The identity this attestation makes claims about.
    pub subject: Ed25519PublicKey,
    /// One or more claims, packed into one attestation to amortise signature
    /// cost (e.g. "EmployeeOf(ACME) + ClearedAt(Confidential)" in one envelope).
    pub claims: Vec<Claim>,
    /// Earliest moment this attestation is valid. `SystemTime` is the
    /// spec-mandated type; validators handle clock skew (recommended: up to
    /// 5 minutes either side).
    pub valid_from: SystemTime,
    /// Last moment this attestation is valid; `valid_until > valid_from` is
    /// checked by the validator, a misconfigured window treated as expired.
    pub valid_until: SystemTime,
    /// Detached Ed25519 signature by `issued_by` over the canonical-bytes
    /// view minus the signature field — see `signing_bytes`.
    pub signature: Signature,
}

impl Attestation {
    /// Canonical bytes the issuer signs to mint this attestation; omits
    /// `signature` so signing is well-defined.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, CanonicalError> {
        // Postcard is structural, so build a struct-shaped tuple mirroring
        // `Attestation`'s field order minus `signature` — a named helper
        // struct avoids drifting from the wire encoding if a field is added.
        #[derive(Serialize)]
        struct View<'a> {
            id: &'a AttestationId,
            issued_by: &'a Ed25519PublicKey,
            subject: &'a Ed25519PublicKey,
            claims: &'a [Claim],
            valid_from: &'a SystemTime,
            valid_until: &'a SystemTime,
        }
        canonical_bytes(&View {
            id: &self.id,
            issued_by: &self.issued_by,
            subject: &self.subject,
            claims: &self.claims,
            valid_from: &self.valid_from,
            valid_until: &self.valid_until,
        })
    }
}

/// A single semantic claim packed into an attestation.
///
/// `Custom` lets private deployments attach extensions without a spec
/// amendment — `value` is opaque bytes, `kind` picks the decoder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Claim {
    /// "This identity is a member of the named space." Issued by the space's
    /// trust root at join time.
    MemberOf {
        /// Space id this membership covers.
        space: crate::space::SpaceId,
    },
    /// "This identity is cleared at the given level." Gates
    /// `Artifact::classification`. Issued by an org's trust root (or delegate).
    ClearedAt(Clearance),
    /// "This identity is an employee of the named org." Human-meaningful,
    /// not parsed by the validator; surfaces in UI ("Signed by Alice @ ACME").
    EmployeeOf(String),
    /// Extension point for deployment-specific claims: `kind` is a stable
    /// DNS-style string, `value` opaque bytes the matching decoder interprets.
    Custom {
        /// Discriminator string; the validator doesn't interpret it.
        kind: String,
        /// Opaque bytes. Decoded by whoever knows the `kind`.
        #[serde(with = "serde_bytes")]
        value: Vec<u8>,
    },
}

/// The full identity record cached on the local device. `public_key` is
/// the stable handle; `attestations` is an *observed* cache, not
/// authoritative — see the module-level note on freshness.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    /// Stable Ed25519 public key — the "I am me" anchor used as the key in
    /// every member map and the author field on every signed op.
    pub public_key: Ed25519PublicKey,
    /// Human-meaningful label, defaulting to the OS username at first boot;
    /// freely renamable, not unique, not used in equality.
    pub label: String,
    /// Optional avatar image bytes (PNG/JPEG, ~128px). Stored inline so a
    /// member directory snapshot needs no separate fetch round-trip.
    pub avatar: Option<AvatarBytes>,
    /// Cached attestations for this identity. May be stale — re-fetch before
    /// a security decision.
    pub attestations: Vec<Attestation>,
}

/// Avatar image bytes, wrapped in a newtype with a `serde_bytes` adapter
/// so postcard encodes it length-prefixed (compact) rather than one
/// varint per byte.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AvatarBytes(#[serde(with = "serde_bytes")] pub Vec<u8>);

#[cfg(test)]
mod tests {
    use super::*;

    /// Attestation ids are unique, clearances order by level, and claims
    /// round-trip canonically.
    #[test]
    fn identity_types_are_unique_ordered_and_round_trip() {
        // Case: attestation ids are unique.
        let a = AttestationId::new();
        let b = AttestationId::new();
        assert_ne!(a, b);

        // Case: clearance ordering.
        assert!(Clearance::PUBLIC < Clearance::RESTRICTED);
        assert!(Clearance::INTERNAL < Clearance::CONFIDENTIAL);
        assert_eq!(Clearance::PUBLIC, Clearance(0));
        assert_eq!(Clearance::RESTRICTED, Clearance(3));

        // Case: claim round trip.
        let c = Claim::EmployeeOf("ACME".into());
        let bytes = canonical_bytes(&c).unwrap();
        let back: Claim = crate::canonical::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(c, back);
    }
}
