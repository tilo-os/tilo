//! The broker's one error type.

use std::io;

#[derive(Debug, thiserror::Error)]
pub(crate) enum ProjectorError {
    /// A local facility the projection needs is missing or would not start:
    /// the runtime directory, a discovery browser, the media pipeline or a
    /// sender helper.
    #[error("{0}")]
    Unavailable(String),
    /// The request was malformed, or names a transport the broker cannot run.
    #[error("{0}")]
    Protocol(String),
    /// The receiver, or the sender speaking to it, refused or ended the stream.
    #[error("{0}")]
    Receiver(String),
    /// A bounded wait ran out.
    #[error("{0}")]
    Timeout(&'static str),
    /// An operating-system call failed while the broker was doing `context`.
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: io::Error,
    },
}

impl ProjectorError {
    /// `map_err` adapter that names what the broker was doing when `source` failed.
    pub(crate) fn io(context: impl Into<String>) -> impl FnOnce(io::Error) -> Self {
        let context = context.into();
        move |source| Self::Io { context, source }
    }

    /// The error for an absent required command-line flag.
    pub(crate) fn missing(flag: &str) -> Self {
        Self::Protocol(format!("missing {flag}"))
    }
}
