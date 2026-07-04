//! The human-facing identity primitive.
//!
//! An `Identity` represents one human across all their devices. It
//! carries:
//!
//! - a stable Ed25519 public key (the "I am me" anchor);
//! - a human-meaningful label and an optional avatar;
//! - zero or more `Attestation`s — signed claims about this identity,
//!   issued by trust roots (an organisation's master key, a peer
//!   user, etc.).
//!
//! In Phase 2 the *private* half of the keypair is held in a
//! single-device file under `~/.config/qos/identity/`. Phase 8
//! upgrades to FROST 2-of-N threshold sharing without changing this
//! type — only the way `sign_*` calls are routed changes.
//!
//! ## Attestation lifecycle
//!
//! Each attestation has a validity window (`valid_from` / `valid_until`)
//! and a signature from the issuer. Verifiers check the signature
//! traces back to a trust root they accept (see
//! [`crate::validator`]). Revocation is a separate event
//! ([`crate::event::ChannelEvent::AttestationRevoked`]) — the
//! attestation itself is immutable.
//!
//! The `attestations` field on `Identity` is a *cache* of
//! attestations the local device has observed for this identity. It
//! is not authoritative — fresh attestations may arrive over the
//! wire after this identity was first cached. Callers must re-query
//! the issuer (or, in the channel case, the channel's role table)
//! for the current set of attestations before making a security
//! decision.

use serde::{Deserialize, Serialize};
use std::time::SystemTime;
use uuid::Uuid;

use crate::canonical::{CanonicalError, canonical_bytes};
use crate::crypto::{Ed25519PublicKey, Signature};

/// Stable identifier for an attestation.
///
/// UUID v4 because attestation ids must be unique across all
/// issuers without coordination, and a 128-bit random space gives
/// us that. The id is assigned by the issuer at attestation-mint
/// time and is the handle by which the attestation is referenced
/// from `MemberRef::attestation` and `SignedOp::attestation_id`.
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
/// Modelled as a `u8` so callers can write `Clearance(2)` for a
/// "Confidential" requirement and arithmetic comparisons work
/// directly. The spec specifies four named levels:
///
/// - `0` — Public (no restriction).
/// - `1` — Internal (any member of the issuing org).
/// - `2` — Confidential (cleared subset of the org).
/// - `3` — Restricted (smallest cleared set).
///
/// Higher numeric value = stricter requirement. An identity with
/// `ClearedAt(2)` may access artifacts with classification 0, 1, or
/// 2, but not 3. The check is `identity_clearance >= artifact_classification`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Clearance(pub u8);

impl Clearance {
    /// Public — no clearance requirement. Equivalent to
    /// `Clearance(0)`.
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
/// Trust roots are themselves Ed25519 public keys; the validator
/// must be configured with a set of accepted trust roots per
/// channel (see
/// [`crate::validator::ChannelState::accepted_trust_roots`]).
///
/// The `signature` field covers everything else in this struct —
/// see `Attestation::signing_bytes` for the exact view. Revoking an
/// attestation produces a separate event; this struct is immutable
/// once minted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attestation {
    /// Unique id, set by the issuer at mint time. Referenced from
    /// `MemberRef::attestation` and `SignedOp::attestation_id`.
    pub id: AttestationId,
    /// Trust root that issued this attestation. The signature
    /// covers all other fields and must verify against this key.
    pub issued_by: Ed25519PublicKey,
    /// The identity this attestation makes claims about.
    pub subject: Ed25519PublicKey,
    /// One or more claims. Multiple claims can be packed into a
    /// single attestation to amortise signature cost (e.g. issue
    /// "EmployeeOf(ACME) + ClearedAt(Confidential)" in one signed
    /// envelope).
    pub claims: Vec<Claim>,
    /// Earliest moment this attestation is valid. `SystemTime` is
    /// the spec-mandated type; validators are responsible for
    /// dealing with clock skew between issuer and verifier (the
    /// recommendation is to allow up to 5 minutes of skew either
    /// side).
    pub valid_from: SystemTime,
    /// Last moment this attestation is valid. `valid_until > valid_from`
    /// is checked by the validator; a misconfigured window is
    /// treated as expired.
    pub valid_until: SystemTime,
    /// Detached Ed25519 signature by `issued_by` over the
    /// canonical-bytes view of this struct minus the signature
    /// field. See `Attestation::signing_bytes`.
    pub signature: Signature,
}

impl Attestation {
    /// Canonical bytes that the issuer signs to mint this
    /// attestation. The view omits the `signature` field itself so
    /// signing is well-defined; the wire bytes (with signature)
    /// flow through the regular `canonical_bytes` serialiser.
    pub fn signing_bytes(&self) -> Result<Vec<u8>, CanonicalError> {
        // Postcard is structural, so we build a struct-shaped tuple
        // that mirrors `Attestation`'s field order minus
        // `signature`. Using a named helper struct avoids drifting
        // from the wire encoding if a new field is added: just
        // mirror the addition here.
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
/// `Custom` exists so private deployments can attach extensions
/// (e.g. "BadgeNumber(...)") without forcing a spec amendment. The
/// `value` is opaque bytes; callers that want to interpret a custom
/// claim use the `kind` discriminator string to pick a decoder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Claim {
    /// "This identity is a member of the named space." Issued by
    /// the space's trust root at join time.
    MemberOf {
        /// Space id this membership covers.
        space: crate::space::SpaceId,
    },
    /// "This identity is cleared at the given level." Used as the
    /// gate for `Artifact::classification`. Issued by an org's
    /// trust root (or a delegated security officer).
    ClearedAt(Clearance),
    /// "This identity is an employee of the named org." The
    /// org-name string is human-meaningful and not parsed by the
    /// validator; it surfaces in UI ("Signed by Alice @ ACME").
    EmployeeOf(String),
    /// Extension point for deployment-specific claims. `kind` is a
    /// stable string (DNS-style: `com.example.badge-number`),
    /// `value` is opaque bytes the decoder for that `kind` knows
    /// how to interpret.
    Custom {
        /// Discriminator string. The validator does not interpret
        /// it; downstream code matches on `kind` to pick a decoder.
        kind: String,
        /// Opaque bytes. Decoded by whoever knows the `kind`.
        #[serde(with = "serde_bytes")]
        value: Vec<u8>,
    },
}

/// The full identity record cached on the local device.
///
/// `public_key` is the stable handle. `attestations` is an *observed*
/// cache, not an authoritative list — see the module-level note on
/// freshness.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    /// Stable Ed25519 public key for this identity. The "I am me"
    /// anchor used as the key in every member map and the author
    /// field on every signed op.
    pub public_key: Ed25519PublicKey,
    /// Human-meaningful label. Defaults to the OS username at first
    /// boot; the user can rename freely. Not unique; not used in
    /// any equality decision.
    pub label: String,
    /// Optional avatar image bytes. Raw PNG or JPEG, sized for the
    /// roster pill thumbnail (~128px). Stored inline so that
    /// receiving a member directory snapshot does not require a
    /// separate fetch round-trip. May be `None` if the user hasn't
    /// set an avatar.
    pub avatar: Option<AvatarBytes>,
    /// Cached attestations observed for this identity. May be stale
    /// — validators should re-fetch before making a security
    /// decision.
    pub attestations: Vec<Attestation>,
}

/// Avatar image bytes (PNG or JPEG). Wrapped in a newtype with a
/// `serde_bytes` adapter so postcard encodes it as length-prefixed
/// bytes (compact) rather than as a sequence of `u8`s
/// (1 varint per byte — wasteful).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AvatarBytes(#[serde(with = "serde_bytes")] pub Vec<u8>);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attestation_id_is_unique() {
        let a = AttestationId::new();
        let b = AttestationId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn clearance_ordering() {
        assert!(Clearance::PUBLIC < Clearance::RESTRICTED);
        assert!(Clearance::INTERNAL < Clearance::CONFIDENTIAL);
        assert_eq!(Clearance::PUBLIC, Clearance(0));
        assert_eq!(Clearance::RESTRICTED, Clearance(3));
    }

    #[test]
    fn claim_round_trip() {
        let c = Claim::EmployeeOf("ACME".into());
        let bytes = canonical_bytes(&c).unwrap();
        let back: Claim = crate::canonical::from_canonical_bytes(&bytes).unwrap();
        assert_eq!(c, back);
    }
}
