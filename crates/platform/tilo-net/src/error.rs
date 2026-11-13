//! The one error type of every network operation.

use crate::Unsupported;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum NetError {
    #[error(transparent)]
    Unsupported(#[from] Unsupported),
    #[error("invalid URL: {0}")]
    InvalidUrl(String),
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("timed out")]
    Timeout,
    /// The server was not reached, or the connection failed. A page reports a
    /// refused cross-origin response the same way.
    #[error("{}", connect_message(message, *cross_origin))]
    Connect { message: String, cross_origin: bool },
    #[error("the fetch proxy refused this destination: {0}")]
    Blocked(String),
    #[error("HTTP {0}")]
    Status(http::StatusCode),
    #[error("response body: {0}")]
    Body(String),
    #[error("the response is larger than {limit} bytes")]
    TooLarge { limit: usize },
    #[error("the server redirected and the request does not follow redirects")]
    Redirect,
    #[error("the connection is closed")]
    Closed,
    #[error("the connection closed ({code}): {reason}")]
    ClosedWith { code: u32, reason: String },
    #[error("{0}")]
    Protocol(String),
}

fn connect_message(message: &str, cross_origin: bool) -> String {
    if cross_origin {
        format!(
            "{message} (the site may not allow this page to read its responses; an offline network fails the same way)"
        )
    } else {
        message.to_owned()
    }
}

impl NetError {
    pub(crate) fn connect(message: impl Into<String>) -> Self {
        Self::Connect {
            message: message.into(),
            cross_origin: false,
        }
    }

    #[must_use]
    pub fn is_timeout(&self) -> bool {
        matches!(self, Self::Timeout)
    }

    #[must_use]
    pub fn is_connect(&self) -> bool {
        matches!(self, Self::Connect { .. })
    }

    #[must_use]
    pub fn may_be_cors(&self) -> bool {
        matches!(
            self,
            Self::Connect {
                cross_origin: true,
                ..
            }
        )
    }

    #[must_use]
    pub fn status(&self) -> Option<http::StatusCode> {
        match self {
            Self::Status(status) => Some(*status),
            _ => None,
        }
    }
}

#[cfg(feature = "webtransport")]
impl web_transport_trait::Error for NetError {
    fn session_error(&self) -> Option<(u32, String)> {
        match self {
            Self::ClosedWith { code, reason } => Some((*code, reason.clone())),
            _ => None,
        }
    }
}
