//! This device's own room host: the same host a relay runs
//! (`room_host::RoomHost`), in this process, serving the rooms placed
//! on this device.
//!
//! The actor's router is built before any room is hosted, so it carries the
//! host's protocols from the start as deferred handlers
//! (`room_host::deferred_protocols`): they refuse every connection
//! until a host is installed, then hand each to it. Nothing runs for them
//! before then. The host itself opens only once this device hosts a room
//! (`crate::shell::network::embedded_host`).
//!
//! The device's native owner is a member of every room it holds, these
//! included: for a room hosted here its session goes to this host over an
//! in-process carrier (`RoomHost::connect_local`), through the same
//! handshake and session code a remote member's takes over iroh. An
//! endpoint cannot dial itself, and a session that never leaves the
//! process keeps no relay up and arms no timer.
use std::sync::{Arc, Mutex, PoisonError};

/// The ALPN handlers a host serves, for the actor's router.
pub use room_host::Protocols;
use room_host::{HostSlot, RoomHost};
/// Where the room host lives: the one place its path is named.
pub(crate) use tilo_room_host::host as room_host;

/// This device's room host, once it is open: shared by the network (which
/// opens it) and the actor (which joins its rooms through it).
#[derive(Clone)]
pub struct LocalHost(Arc<Inner>);

struct Inner {
    slot: HostSlot,
    host: Mutex<Option<Arc<RoomHost>>>,
}

impl std::fmt::Debug for LocalHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalHost")
            .field("open", &self.get().is_some())
            .finish()
    }
}

impl LocalHost {
    /// A host not open yet, and the deferred handlers that serve it once it
    /// is, for the router.
    #[must_use]
    pub fn deferred() -> (Self, Protocols) {
        let (slot, protocols) = room_host::deferred_protocols();
        (
            Self(Arc::new(Inner {
                slot,
                host: Mutex::new(None),
            })),
            protocols,
        )
    }

    /// The open host, if this device hosts rooms.
    #[must_use]
    pub fn get(&self) -> Option<Arc<RoomHost>> {
        self.0
            .host
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The workspace-join exchange with this device's host, in process, as
    /// the device `caller` (this endpoint's own key): what enrolling into a
    /// room hosted here, or renewing membership of one, talks to. `None`
    /// while the host is not open.
    #[must_use]
    pub fn join_client(
        &self,
        caller: [u8; 32],
    ) -> Option<tilo_relay_session::workspace_join::WorkspaceJoinClient> {
        let host = self.get()?;
        Some(
            tilo_relay_session::workspace_join::WorkspaceJoinClient::local(
                host.join_service(),
                caller,
            ),
        )
    }

    /// Serve `host` from now on: its protocols answer on the router, and
    /// the native owner reaches its rooms through it. A host already
    /// installed stays; the one answered is the host in use.
    pub fn install(&self, host: Arc<RoomHost>) -> Arc<RoomHost> {
        let mut installed = self.0.host.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(current) = installed.as_ref() {
            return Arc::clone(current);
        }
        self.0.slot.install(Arc::clone(&host));
        *installed = Some(Arc::clone(&host));
        host
    }
}
