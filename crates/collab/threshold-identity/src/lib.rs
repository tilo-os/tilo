//! `threshold-identity` — FROST threshold-shared Ed25519 identity for Tilo.
//!
//! A single logical Ed25519 keypair representing a human whose secret
//! half is split across the user's devices via FROST(Ed25519, SHA-512)
//! (RFC 9591), so no single device holds the full key.
//!
//! **Headline property**: the aggregated signature from
//! [`signing::aggregate_signature`] verifies against `group_public_key`
//! via standard Ed25519 verification, so
//! `collab_types::verify_event(&value, &threshold_signature, &group_pk)`
//! works unchanged — verifiers don't need to know an op was threshold-signed.
//!
//! - [`identity::ThresholdIdentity`] — the per-device handle: secret
//!   share, threshold parameters, group public key, `sign_*` access.
//! - [`dkg`] — Distributed Key Generation, used to add a device to an
//!   existing identity (1→2, 2→3, …).
//! - [`signing`] — round-1 commit, round-2 partial-sign, aggregate, each
//!   a free function so a coordinator can drive multiple devices independently.
//! - [`shares::SecretShare`] — opaque secret-bearing share: no
//!   `Serialize`, redacted `Debug`, zeroized on drop.
//! - [`shares::RecoveryShare`] — paper-share format: 24 BIP39 words +
//!   non-secret metadata.
//! - [`errors::ThresholdError`] — the single fallible-result type.
//!
//! Non-goals: no device-enrolment UI (protocol primitives only; QR
//! pairing/confirmation-code UX is a `shell`+`ui` follow-up); no
//! persistence (the identity record is in-memory, the enrolment flow
//! writes it under `~/.config/tilo/identity/`); no proactive secret
//! sharing/refresh (each DKG run produces a fresh group public key); no
//! threshold *encryption* (this crate signs only).
//!
//! ## Wire-protocol stability
//!
//! [`dkg::EnrolmentRound1`], [`dkg::EnrolmentRound2`],
//! [`signing::SigningRound1Commitment`], and
//! [`signing::SigningRound2Partial`] derive `Serialize + Deserialize`,
//! wrapping opaque `frost-ed25519` round-package bytes inside a small
//! Tilo header (sender's share index) so the broker can route them
//! without parsing the FROST payload.

pub mod dkg;
pub mod errors;
pub mod identity;
mod rng;
pub mod shares;
pub mod signing;

// Convenience re-exports.
pub use dkg::{DkgOutcome, DkgSession, EnrolmentRound1, EnrolmentRound2};
pub use errors::ThresholdError;
pub use identity::ThresholdIdentity;
pub use shares::{RecoveryShare, SecretShare};
pub use signing::{
    SigningRound1, SigningRound1Commitment, SigningRound2Partial, SigningSessionNonces,
    aggregate_signature, sign_round1, sign_round2, verify_threshold_signature,
};
