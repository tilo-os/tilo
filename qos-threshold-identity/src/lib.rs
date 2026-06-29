//! `qos-threshold-identity` — FROST threshold-shared Ed25519
//! identity for qOS.
//!
//! This crate implements **Phase 8** of the
//! `2026-06-29-spaces-channels-and-cryptographic-roles.md` design
//! spec: a single logical Ed25519 keypair representing a human
//! whose secret half is split across the user's devices via
//! FROST(Ed25519, SHA-512) (RFC 9591) so that no single device
//! holds the full key.
//!
//! ## Headline property: byte-compatible with regular Ed25519
//!
//! The aggregated signature produced by [`signing::aggregate_signature`]
//! verifies against `group_public_key` via the standard Ed25519
//! verification routine. That means
//! `qos_collab_types::verify_event(&value, &threshold_signature, &group_pk)`
//! works unchanged — verifiers do not need to know an op was
//! signed via threshold.
//!
//! ## What is in this crate
//!
//! - [`identity::ThresholdIdentity`] — the per-device handle. Owns
//!   the device's secret share, threshold parameters, group public
//!   key, and provides single-method `sign_*` access.
//! - [`dkg`] — Distributed Key Generation state machine. Used to
//!   add a new device to an existing identity (1→2, 2→3, …).
//! - [`signing`] — Round-1 commit, round-2 partial-sign, aggregate.
//!   Each round is a free function so a coordinator can drive
//!   multiple devices independently.
//! - [`shares::SecretShare`] — opaque secret-bearing share. No
//!   `Serialize`, redacted `Debug`, zeroized on drop.
//! - [`shares::RecoveryShare`] — paper-share format: 24 BIP39
//!   words + non-secret metadata.
//! - [`errors::ThresholdError`] — the single fallible-result
//!   type.
//!
//! ## Non-goals (deliberate)
//!
//! - No device-enrolment UI. The protocol primitives are here;
//!   the QR pairing, 6-digit confirmation code, and ceremony UX
//!   are a `shell/` + `ui/` follow-up.
//! - No persistence. The identity record is in-memory; the
//!   enrolment flow is responsible for writing it under
//!   `~/.config/qos/identity/`.
//! - No proactive secret sharing / share refresh. Each DKG run
//!   produces a fresh group public key. The spec accepts that for
//!   Phase 8; refresh-based enrolment is a forward-compatible
//!   upgrade.
//! - No threshold *encryption* (MLS in Phase 7). This crate signs
//!   only.
//!
//! ## Wire-protocol stability
//!
//! [`dkg::EnrolmentRound1`], [`dkg::EnrolmentRound2`],
//! [`signing::SigningRound1Commitment`], and
//! [`signing::SigningRound2Partial`] derive `Serialize +
//! Deserialize`. They wrap opaque `frost-ed25519` round-package
//! bytes inside a small qOS header (sender's share index) so the
//! qOS broker can route them without parsing the FROST payload.

pub mod dkg;
pub mod errors;
pub mod identity;
pub mod shares;
pub mod signing;
pub mod transcript;

// Convenience re-exports.
pub use dkg::{DkgOutcome, DkgSession, EnrolmentRound1, EnrolmentRound2};
pub use errors::ThresholdError;
pub use identity::ThresholdIdentity;
pub use shares::{RecoveryShare, SecretShare};
pub use signing::{
    SigningRound1, SigningRound1Commitment, SigningRound2Partial, SigningSessionNonces,
    aggregate_signature, sign_round1, sign_round2, verify_threshold_signature,
};
