//! Error types for `qos-channel-crypto`.
//!
//! OpenMLS itself defines a thicket of error enums parameterised over
//! the storage backend's error type. Propagating those verbatim
//! couples our public surface to openmls's internal type-level
//! plumbing; bumping openmls would then ripple into every downstream
//! caller. Instead we collapse openmls's errors into a single
//! [`Error::OpenMls`] variant that owns a human-readable string built
//! via `{:?}` of the underlying error. The original error chain is
//! lost; in exchange callers handle one shape and the broker can log
//! the message unchanged.
//!
//! Variants are split by *which step failed* (creation, join, etc.)
//! so triage can route on the variant alone — the inner string only
//! matters for the human reading the audit line.

use thiserror::Error;

/// All errors produced by this crate.
#[derive(Debug, Error)]
pub enum Error {
    /// The underlying openmls call failed. The string carries the
    /// `{:?}` form of the openmls error; full type chain is dropped
    /// to keep the public surface stable across openmls bumps.
    #[error("openmls error: {0}")]
    OpenMls(String),

    /// `OpenMlsRustCrypto::storage()` rejected a write of secret
    /// material (signature keypair, key package private state).
    /// Surfaced as a separate variant because storage failure has a
    /// different operational signal than a protocol error: it almost
    /// always means an out-of-memory or in-test fixture issue, not a
    /// malformed peer message.
    #[error("openmls storage error: {0}")]
    Storage(String),

    /// `tls_codec::Serialize`/`Deserialize` failed. The wire format
    /// for Welcome / application messages is TLS-CODEC; a malformed
    /// blob from a peer surfaces here. Always recoverable — drop the
    /// message, do not crash.
    #[error("tls_codec error: {0}")]
    TlsCodec(String),

    /// We received an `MlsMessageIn` whose body variant is not the
    /// one we expected (e.g. tried to read a Welcome but the bytes
    /// decoded to an application message, or vice versa). Cleanly
    /// distinct from `TlsCodec` because the bytes parsed — they just
    /// describe a different shape than the API call wanted.
    #[error("unexpected mls message variant: expected {expected}, got {actual}")]
    UnexpectedMessageKind {
        /// What the caller's API path was hoping to extract.
        expected: &'static str,
        /// What the bytes actually carried.
        actual: &'static str,
    },

    /// `MlsGroup::process_message` returned a non-application
    /// content variant (proposal, external join, commit). The API
    /// surface here only deals with application payloads; governance
    /// commits arrive via the broker's ChannelEvent log, not in-band.
    #[error("processed message was not an application message")]
    NotApplicationMessage,
}

impl Error {
    /// Wrap an openmls error type that implements `Debug`.
    ///
    /// We swallow the source-chain on purpose; see the module docs.
    pub(crate) fn openmls<E: std::fmt::Debug>(e: E) -> Self {
        Self::OpenMls(format!("{e:?}"))
    }

    /// Wrap a storage backend error.
    pub(crate) fn storage<E: std::fmt::Debug>(e: E) -> Self {
        Self::Storage(format!("{e:?}"))
    }

    /// Wrap a TLS-codec error.
    pub(crate) fn tls_codec<E: std::fmt::Debug>(e: E) -> Self {
        Self::TlsCodec(format!("{e:?}"))
    }
}

/// Crate-wide `Result` alias.
pub type Result<T> = std::result::Result<T, Error>;
