//! The broker's one error type.
//!
//! Every variant displays as the plain message the broker has always printed:
//! the shell shows a failed command's stderr to the user, so the category is
//! for callers that branch, never extra text in front of the message.

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_display_the_message_the_shell_shows() {
        assert_eq!(
            ProjectorError::Protocol("invalid --port".to_owned()).to_string(),
            "invalid --port"
        );
        assert_eq!(
            ProjectorError::missing("--address").to_string(),
            "missing --address"
        );
        assert_eq!(
            ProjectorError::Timeout("projection session did not become ready in time").to_string(),
            "projection session did not become ready in time"
        );
        let io = ProjectorError::io("cannot bind compositor video socket")(io::Error::from(
            io::ErrorKind::AddrInUse,
        ));
        assert!(matches!(io, ProjectorError::Io { .. }));
        assert!(
            io.to_string()
                .starts_with("cannot bind compositor video socket: "),
            "{io}"
        );
    }
}
