//! The resources a session's services publish, as its apps read them.
//!
//! A session's services — a catalogue of resources, each read and invoked —
//! live in the session's process, on the session's own thread, which is the
//! only thread that may touch them. An app is another process on the other
//! side of the collaboration wire. This trait is the seam between them:
//! the broker answers an app's `Snapshot`, `Watch` and `Invoke` by asking the
//! provider, and the provider answers on the session's thread.
//!
//! Nothing here decides what an app may reach: the catalogue the provider
//! publishes is the authority, and a resource it does not publish is refused.

use std::fmt;

use runtime_collab::wire::{Event, Receipt};
use tokio::sync::{mpsc, oneshot};

/// Who a resource is read, watched or invoked for: the app the launcher
/// named on the connection, and the workspace it was started in. Per-app
/// grants key on it where a session has them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caller {
    pub app_id: String,
    pub workspace: String,
}

/// A resource's reading as the wire carries it: the services' revision it was
/// read at, and the reading as JSON; `None` when no service publishes it.
pub type Reading = (u64, Option<Vec<u8>>);

/// One watch a connection asked for, as [`PublishedResources::watch`] minted
/// it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ResourceWatch(u64);

impl ResourceWatch {
    /// The watch numbered `id`, as a provider mints them.
    #[must_use]
    pub const fn new(id: u64) -> Self {
        Self(id)
    }

    /// The number the provider minted it under.
    #[must_use]
    pub const fn id(self) -> u64 {
        self.0
    }
}

impl fmt::Display for ResourceWatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "resource watch {}", self.0)
    }
}

/// What a session's services publish, asked by the connections it admits.
///
/// Every call is answered on the session's own thread, so each returns at
/// once with the channel its answer arrives on: a resource is read and
/// invoked by the thread that owns the device, and the watch delivers each
/// changed reading to `sink` as [`Event::Reading`], `None` once no service
/// publishes the resource any more.
pub trait PublishedResources: Send + Sync {
    /// `resource` as it reads now, answered on the session's next pass.
    fn snapshot(&self, caller: &Caller, resource: &str) -> oneshot::Receiver<Reading>;

    /// `resource`'s reading now and each time it changes, delivered to
    /// `sink` until [`Self::unwatch`] or until `sink` closes.
    fn watch(
        &self,
        caller: &Caller,
        resource: &str,
        sink: mpsc::UnboundedSender<Event>,
    ) -> ResourceWatch;

    /// Stop serving `watch`.
    fn unwatch(&self, watch: ResourceWatch);

    /// Invoke `action` of `resource` with `arguments` (a JSON object), answered
    /// on the session's next pass.
    fn invoke(
        &self,
        caller: &Caller,
        resource: &str,
        action: &str,
        arguments: Vec<u8>,
    ) -> oneshot::Receiver<Receipt>;
}
