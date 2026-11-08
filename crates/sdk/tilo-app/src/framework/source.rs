//! Where a framework app's state comes from and where its actions go: one
//! path for both kinds of host (`docs/designs/app-language/one-app-model.md`
//! §4).
//!
//! A runner needs two things of storage, and never a third:
//!
//! - **the state of its resource**, as its own model type reads it, so a
//!   view draws what is there;
//! - **a way to submit an action** on it, with the arguments a control
//!   supplies.
//!
//! [`Source`] is that pair. It has two implementations, and the app's code
//! is the same on both:
//!
//! - [`Source::Document`] is the app's own record, opened by its own host:
//!   the device a runtime is compiled into (`one-app-model.md` §3), a
//!   capture, a test. Views sync from the document the host lent the pass
//!   ([`TypedRoot::sync`](super::root::TypedRoot)) and edits are the typed
//!   actions the host's door applies.
//! - [`Source::Connection`] is the session's document host, reached over the
//!   launcher-made connection ([`Connection`]). The app opens no record and
//!   holds no key: views take the state from the reading the session
//!   publishes, and every edit is an [`Ask`] the session's door decides and
//!   commits as the person's gesture.
//!
//! The root and the dispatcher keep one `Source` and ask it, so a view syncs
//! and submits the same way whichever it is; only [`Source`] tells.

use super::hosted::{Ask, Connection};

/// Where a root's state comes from, and where its actions go.
#[derive(Clone)]
pub(crate) enum Source {
    /// The app's own record, lent to each pass by the host that runs it.
    Document,
    /// The session's document host, over the app's one connection.
    ///
    /// The resource address is the one the record is published at: what the
    /// session's reading answers and what an edit addresses.
    Connection {
        connection: Connection,
        resource: String,
    },
}

impl Source {
    /// The source over `connection`, the record published at `resource`.
    pub(crate) fn over(connection: Connection, resource: impl Into<String>) -> Self {
        Self::Connection {
            connection,
            resource: resource.into(),
        }
    }

    /// Whether the state comes over a connection rather than the pass's
    /// document.
    pub(crate) fn is_connection(&self) -> bool {
        matches!(self, Self::Connection { .. })
    }

    /// The connection the record reaches, when the session keeps it.
    pub(crate) fn connection(&self) -> Option<&Connection> {
        match self {
            Self::Document => None,
            Self::Connection { connection, .. } => Some(connection),
        }
    }

    /// The resource address the session publishes, when there is one.
    pub(crate) fn remote_resource(&self) -> Option<&str> {
        match self {
            Self::Document => None,
            Self::Connection { resource, .. } => Some(resource),
        }
    }

    /// Submit `ask` through the connection that keeps the record; `Document`
    /// has no separate door (`None`), because its door is the document the
    /// pass lent.
    pub(crate) fn submit(&self, ask: &Ask) -> Option<Result<(), String>> {
        match self {
            Self::Document => None,
            Self::Connection { connection, .. } => Some(connection.submit(ask)),
        }
    }
}

impl std::fmt::Debug for Source {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Document => formatter.write_str("Source::Document"),
            Self::Connection { resource, .. } => formatter
                .debug_struct("Source::Connection")
                .field("resource", resource)
                .finish_non_exhaustive(),
        }
    }
}
