//! Errors of the PDF API.

use crate::proto::FailureKind;

/// Why a PDF operation failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The engine cannot run on this host: no pdfium library, no helper
    /// binary, or a kernel that cannot enforce the sandbox.
    #[error("PDF engine unavailable: {0}")]
    Unavailable(String),
    /// The engine process died while handling the request. The next request
    /// starts a new one.
    #[error("the PDF engine crashed: {0}")]
    Crashed(String),
    /// The engine did not answer in time and was stopped.
    #[error("the PDF engine timed out")]
    Timeout,
    /// The bytes are not a readable document.
    #[error("{0}")]
    Format(String),
    /// The document is encrypted and the password is missing or wrong.
    #[error("the document is password protected")]
    Password,
    /// A page, document or argument was out of range.
    #[error("{0}")]
    Invalid(String),
    /// The request exceeds a resource limit.
    #[error("{0}")]
    Limit(String),
    /// Any other engine failure.
    #[error("{0}")]
    Engine(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl Error {
    pub(crate) fn from_failure(kind: FailureKind, message: String) -> Self {
        match kind {
            FailureKind::Format => Self::Format(message),
            FailureKind::Password => Self::Password,
            FailureKind::Invalid => Self::Invalid(message),
            FailureKind::Limit => Self::Limit(message),
            FailureKind::Engine => Self::Engine(message),
        }
    }

    /// Whether retrying the same request may succeed (the engine restarted).
    #[must_use]
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Crashed(_) | Self::Timeout)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
