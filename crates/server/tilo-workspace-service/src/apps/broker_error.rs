//! What the broker answers when it cannot do what an endpoint asked.

use std::fmt;

/// Why the broker refused a request or could not finish it. An endpoint is
/// told only the reason; the kind stays with the broker and its log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BrokerError {
    pub kind: BrokerErrorKind,
    /// What the endpoint is told.
    pub reason: String,
}

/// How a request failed, for the broker's own handling and its log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrokerErrorKind {
    /// The request was refused or went wrong.
    Failed,
    /// The other side of a connection or stream went away.
    Disconnected,
    /// This host does not offer what was asked.
    Unimplemented,
}

impl BrokerError {
    #[must_use]
    pub const fn failed(reason: String) -> Self {
        Self {
            kind: BrokerErrorKind::Failed,
            reason,
        }
    }

    #[must_use]
    pub const fn disconnected(reason: String) -> Self {
        Self {
            kind: BrokerErrorKind::Disconnected,
            reason,
        }
    }

    #[must_use]
    pub const fn unimplemented(reason: String) -> Self {
        Self {
            kind: BrokerErrorKind::Unimplemented,
            reason,
        }
    }
}

impl BrokerErrorKind {
    /// The kind as the broker's log names it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Failed => "failed",
            Self::Disconnected => "disconnected",
            Self::Unimplemented => "unimplemented",
        }
    }
}

impl fmt::Display for BrokerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.reason)
    }
}

impl std::error::Error for BrokerError {}

/// A socket that closed under the broker is a disconnection; any other I/O
/// failure is the request's.
impl From<std::io::Error> for BrokerError {
    fn from(error: std::io::Error) -> Self {
        use std::io::ErrorKind;
        let reason = error.to_string();
        match error.kind() {
            ErrorKind::BrokenPipe
            | ErrorKind::ConnectionAborted
            | ErrorKind::ConnectionRefused
            | ErrorKind::ConnectionReset
            | ErrorKind::NotConnected => Self::disconnected(reason),
            _ => Self::failed(reason),
        }
    }
}
