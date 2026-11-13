//! Errors of the PDF API.

#[cfg(unix)]
use crate::proto::FailureKind;

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
    #[error("the PDF engine timed out")]
    Timeout,
    #[error("{0}")]
    Format(String),
    #[error("the document is password protected")]
    Password,
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Limit(String),
    #[error("{0}")]
    Engine(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl Error {
    #[cfg(unix)]
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

    #[must_use]
    pub fn is_transient(&self) -> bool {
        matches!(self, Self::Crashed(_) | Self::Timeout)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(all(test, unix))]
mod tests {
    use super::*;

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
