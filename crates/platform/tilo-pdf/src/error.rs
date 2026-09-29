//! Errors of the PDF API.

use crate::proto::FailureKind;

/// Why a PDF operation failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The engine cannot run on this host: no pdfium library or no helper
    /// binary.
    #[error("PDF engine unavailable: {0}")]
    Unavailable(String),
    /// The helper cannot enforce its sandbox on this kernel, so it refuses
    /// to parse anything; see
    /// [When the sandbox cannot be enforced](crate#when-the-sandbox-cannot-be-enforced).
    #[error("PDF engine unavailable: this system cannot confine the PDF helper: {0}")]
    Unconfined(String),
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
    #[cfg_attr(
        not(unix),
        allow(dead_code, reason = "pdfium failures exist on Unix only")
    )]
    pub(crate) fn from_failure(kind: FailureKind, message: String) -> Self {
        match kind {
            FailureKind::Format => Self::Format(message),
            FailureKind::Password => Self::Password,
            FailureKind::Invalid => Self::Invalid(message),
            FailureKind::Limit => Self::Limit(message),
            FailureKind::Engine => Self::Engine(message),
            FailureKind::Sandbox => Self::Unconfined(message),
        }
    }

    /// Whether retrying the same request may succeed (the engine restarted).
    #[must_use]
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Crashed(_) | Self::Timeout)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    /// A helper that cannot confine itself is its own error, not a generic
    /// engine failure: callers decide on it (refuse, or a development
    /// build's in-process fallback) without reading the message.
    #[test]
    fn a_helper_that_cannot_confine_itself_is_unconfined() {
        assert!(matches!(
            Error::from_failure(FailureKind::Sandbox, "landlock: not enforced".into()),
            Error::Unconfined(message) if message == "landlock: not enforced"
        ));
        assert!(matches!(
            Error::from_failure(FailureKind::Engine, "pdfium".into()),
            Error::Engine(_)
        ));
    }
}
