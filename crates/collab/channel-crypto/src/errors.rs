//! Error types for `channel-crypto`.
//!
//! `OpenMLS` defines a thicket of error enums parameterised over the
//! storage backend's error type; propagating those verbatim would
//! couple our surface to openmls's internal plumbing, so they collapse
//! into a single [`Error::OpenMls`] carrying a `{:?}`-rendered string.
//! The error chain is lost; in exchange callers handle one shape and the
//! broker logs the message unchanged.
//!
//! Variants split by *which step failed* so triage routes on the
//! variant alone — the inner string is only for the human reading the
//! audit line.

use thiserror::Error;

/// All errors produced by this crate.
#[derive(Debug, Error)]
pub enum Error {
    /// The underlying openmls call failed; the string is the `{:?}` form,
    /// full type chain dropped to keep the surface stable across openmls bumps.
    #[error("openmls error: {0}")]
    OpenMls(String),

    /// `OpenMlsRustCrypto::storage()` rejected a write of secret material.
    /// Separate variant because storage failure has a different operational
    /// signal than a protocol error — almost always OOM or a test-fixture
    /// issue, not a malformed peer message.
    #[error("openmls storage error: {0}")]
    Storage(String),

    /// `tls_codec::Serialize`/`Deserialize` failed — the Welcome/application
    /// wire format is TLS-CODEC; a malformed peer blob surfaces here.
    /// Always recoverable: drop the message, don't crash.
    #[error("tls_codec error: {0}")]
    TlsCodec(String),

    /// An `MlsMessageIn`'s body variant wasn't the one expected (e.g. a
    /// Welcome read as an application message). Distinct from `TlsCodec` —
    /// the bytes parsed, they just describe a different shape.
    #[error("unexpected mls message variant: expected {expected}, got {actual}")]
    UnexpectedMessageKind {
        /// What the caller's API path was hoping to extract.
        expected: &'static str,
        /// What the bytes actually carried.
        actual: &'static str,
    },

    /// `MlsGroup::process_message` returned a non-application variant
    /// (proposal, external join, commit) — this surface only handles
    /// application payloads; governance commits arrive via the broker's
    /// `ChannelEvent` log, not in-band.
    #[error("processed message was not an application message")]
    NotApplicationMessage,
}

impl Error {
    /// Wrap an openmls error type that implements `Debug`. Swallows the
    /// source chain on purpose; see the module docs.
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
