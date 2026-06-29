//! Error types for the threshold-identity crate.
//!
//! `ThresholdError` is the single error type returned from every
//! fallible operation on the public API. It is `pub` because the
//! crate is a foundation primitive that downstream code (the
//! enrolment-UI controller in `shell/`, the recovery-flow widget
//! in `ui/`) needs to pattern-match against to produce
//! human-actionable error messages.
//!
//! Each variant captures *what went wrong*, not *why* — the
//! interpretation is the caller's responsibility. For example
//! `DkgFailed` does not distinguish between "a peer sent an
//! invalid commitment" and "the threshold parameters don't add
//! up" because both are programmer errors at this layer; the
//! protocol coordinator is expected to verify peer messages
//! before handing them in.

use thiserror::Error;

use qos_collab_types::CanonicalError;

/// Failure modes for threshold-identity operations.
///
/// Variants are intentionally coarse — the granular FROST library
/// errors are wrapped via `FrostInternal`. This keeps the public
/// surface stable across `frost-ed25519` major bumps and lets
/// downstream callers pattern-match on the small, semantic set
/// they actually need to react to.
#[derive(Debug, Error)]
pub enum ThresholdError {
    /// The underlying FROST library reported a protocol-level
    /// error: a bad commitment, an unverifiable signature share,
    /// inconsistent round packages, etc. The inner string is the
    /// `frost-core::Error::Display` form for the audit log; do
    /// not parse it.
    #[error("FROST internal error: {0}")]
    FrostInternal(String),

    /// A share index outside the `1..=total_shares` range was
    /// passed. The 0 share index is reserved by FROST.
    #[error("invalid share index: {0} (must be 1..=total_shares)")]
    InvalidShareIndex(u16),

    /// A threshold / total combination that does not describe a
    /// valid t-of-n scheme. E.g. `threshold > total`, or
    /// `threshold == 0` (FROST requires at least 1 signer).
    #[error("invalid threshold parameters: {threshold} of {total}")]
    InvalidThresholdParameters {
        /// The supplied threshold.
        threshold: u16,
        /// The supplied total.
        total: u16,
    },

    /// The signing operation received fewer than `threshold` partial
    /// signatures. Distinguished from `FrostInternal` because the
    /// signing coordinator can do something about it (collect more
    /// shares) whereas internal protocol errors are programmer
    /// bugs.
    #[error("not enough partial signatures: have {have}, need {need}")]
    InsufficientPartials {
        /// Number of partials provided to `aggregate_signature`.
        have: usize,
        /// Minimum required (the threshold `t`).
        need: usize,
    },

    /// The DKG state machine was driven out of order: e.g. round 2
    /// called before round 1. Indicates a coordinator bug; recover
    /// by re-running DKG from the start.
    #[error("DKG state machine misuse: {0}")]
    DkgStateMisuse(&'static str),

    /// Canonical-bytes serialisation failed. Re-exported from
    /// `qos-collab-types` for symmetry with the other crates that
    /// produce signatures.
    #[error("canonical-bytes encoding failed: {0}")]
    Canonical(#[from] CanonicalError),

    /// A BIP39 mnemonic decode failed: malformed words, wrong
    /// length, or checksum mismatch. Returned by
    /// `ThresholdIdentity::import_recovery_share`.
    #[error("recovery-share mnemonic could not be decoded: {0}")]
    BadRecoveryMnemonic(String),

    /// The recovery-share metadata (share index, threshold,
    /// verifying share) is inconsistent with the surviving
    /// `ThresholdIdentity` being recovered into. Surfaces when
    /// the user tries to import a share that belongs to a
    /// different group.
    #[error("recovery-share metadata mismatch: {0}")]
    RecoveryMismatch(&'static str),
}

impl From<frost_ed25519::Error> for ThresholdError {
    /// Bridge FROST's typed errors into our coarse variant. We
    /// intentionally lose the specific frost-core variant — the
    /// crate-level audit log doesn't need it, and exposing it
    /// would couple our public surface to `frost-core`'s
    /// error-shape changes. `frost_ed25519::Error` is a type
    /// alias for `frost_core::Error<Ed25519Sha512>` so we
    /// pattern by the alias.
    fn from(err: frost_ed25519::Error) -> Self {
        ThresholdError::FrostInternal(err.to_string())
    }
}
