//! The one error type of every network operation.

use crate::Unsupported;

/// Why a network operation failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum NetError {
    /// This target or host cannot do it.
    #[error(transparent)]
    Unsupported(#[from] Unsupported),
    /// The URL did not parse, or names a scheme the operation does not speak.
    #[error("invalid URL: {0}")]
    InvalidUrl(String),
    /// A header, method or body the request cannot carry.
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    /// The deadline passed before the operation finished.
    #[error("timed out")]
    Timeout,
    /// The server was not reached, or the connection failed.
    ///
    /// In a page the browser does not say why: a cross-origin request whose
    /// response the site did not allow (CORS) fails the same way as an
    /// unreachable host. `cross_origin` is then true, and the host's
    /// [`proxy`](crate::proxy) may help.
    #[error("{}", connect_message(message, *cross_origin))]
    Connect { message: String, cross_origin: bool },
    /// A destination the host's fetch proxy refuses to reach, such as an
    /// address on the host's own network.
    #[error("the fetch proxy refused this destination: {0}")]
    Blocked(String),
    /// The server answered with an error status, from
    /// [`Response::error_for_status`](crate::Response::error_for_status).
    #[error("HTTP {0}")]
    Status(http::StatusCode),
    /// The body failed while streaming.
    #[error("response body: {0}")]
    Body(String),
    /// A body was longer than the caller's limit.
    #[error("the response is larger than {limit} bytes")]
    TooLarge { limit: usize },
    /// A redirect the request was asked not to follow, in a page, where the
    /// browser hides the redirect response.
    #[error("the server redirected and the request does not follow redirects")]
    Redirect,
    /// The connection or stream has ended.
    #[error("the connection is closed")]
    Closed,
    /// The peer broke a protocol (WebSocket, WebTransport, TLS, JSON).
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

    /// Whether the operation ran out of time.
    #[must_use]
    pub fn is_timeout(&self) -> bool {
        matches!(self, Self::Timeout)
    }

    /// Whether the server was not reached.
    #[must_use]
    pub fn is_connect(&self) -> bool {
        matches!(self, Self::Connect { .. })
    }

    /// Whether this may be the browser refusing a cross-origin response,
    /// which the host's fetch proxy can relay.
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

    /// The error status, for [`NetError::Status`].
    #[must_use]
    pub fn status(&self) -> Option<http::StatusCode> {
        match self {
            Self::Status(status) => Some(*status),
            _ => None,
        }
    }
}
