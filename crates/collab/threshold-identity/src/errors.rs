//! Error types for the threshold-identity crate.
//!
//! `ThresholdError` is the single error type from every fallible public
//! operation — `pub` so downstream code (the enrolment-UI controller in
//! `crates/session/shell/`, the recovery-flow widget in `crates/sdk/ui-core/`) can pattern-match for
//! human-actionable messages.
//!
//! Each variant captures *what* went wrong, not *why*: e.g. `DkgFailed`
//! doesn't distinguish an invalid peer commitment from mismatched
//! threshold parameters, since both are programmer errors at this
//! layer — the protocol coordinator is expected to verify peer messages
//! before handing them in.

use thiserror::Error;

use collab_types::CanonicalError;

/// Failure modes for threshold-identity operations. Intentionally
/// coarse — granular FROST library errors wrap via `FrostInternal`,
/// keeping the public surface stable across `frost-ed25519` major bumps.
#[derive(Debug, Error)]
pub enum ThresholdError {
    /// The underlying FROST library reported a protocol-level error (bad
    /// commitment, unverifiable signature share, inconsistent round
    /// packages, …). The inner string is `frost-core::Error::Display`, for
    /// the audit log — do not parse it.
    #[error("FROST internal error: {0}")]
    FrostInternal(String),

    /// A share index outside `1..=total_shares` — 0 is reserved by FROST.
    #[error("invalid share index: {0} (must be 1..=total_shares)")]
    InvalidShareIndex(u16),

    /// A threshold/total combination that isn't a valid t-of-n scheme (e.g.
    /// `threshold > total`, or `threshold == 0`).
    #[error("invalid threshold parameters: {threshold} of {total}")]
    InvalidThresholdParameters {
        threshold: u16,
        /// The supplied total.
        total: u16,
    },

    /// Fewer than `threshold` partial signatures were supplied. Distinct
    /// from `FrostInternal` because the coordinator can react (collect more
    /// shares), unlike an internal protocol error (a programmer bug).
    #[error("not enough partial signatures: have {have}, need {need}")]
    InsufficientPartials {
        /// Number of partials provided to `aggregate_signature`.
        have: usize,
        /// Minimum required (the threshold `t`).
        need: usize,
    },

    /// The DKG state machine was driven out of order (e.g. round 2 before
    /// round 1) — a coordinator bug; recover by re-running DKG from the start.
    #[error("DKG state machine misuse: {0}")]
    DkgStateMisuse(&'static str),

    /// Canonical-bytes serialisation failed. Re-exported from
    /// `collab-types` for symmetry with other signature-producing crates.
    #[error("canonical-bytes encoding failed: {0}")]
    Canonical(#[from] CanonicalError),

    /// A BIP39 mnemonic decode failed (malformed words, wrong length,
    /// checksum mismatch) — returned by `import_recovery_share`.
    #[error("recovery-share mnemonic could not be decoded: {0}")]
    BadRecoveryMnemonic(String),

    /// Recovery-share metadata (index, threshold, verifying share)
    /// disagrees with the `ThresholdIdentity` being recovered into — the
    /// share belongs to a different group.
    #[error("recovery-share metadata mismatch: {0}")]
    RecoveryMismatch(&'static str),
}

impl From<frost_ed25519::Error> for ThresholdError {
    /// Bridge FROST's typed errors into our coarse variant, deliberately
    /// losing the specific frost-core variant — the audit log doesn't need
    /// it, and exposing it would couple our surface to frost-core's
    /// error-shape changes. `frost_ed25519::Error` is a type alias for
    /// `frost_core::Error<Ed25519Sha512>`, matched by that alias.
    fn from(err: frost_ed25519::Error) -> Self {
        ThresholdError::FrostInternal(err.to_string())
    }
}
