//! This device's own workspace host: the same host a server runs
//! (`workspace_service::WorkspaceService`), in this process, serving the workspaces placed
//! on this device.
//!
//! The actor's router is built before any workspace is hosted, so it carries the
//! host's protocols from the start as deferred handlers
//! (`workspace_service::deferred_protocols`): they refuse every connection
//! until a host is installed, then hand each to it. Nothing runs for them
//! before then. The host itself opens only once this device hosts a workspace
//! (the shell's `embedded_host`).
//!
//! The device's native owner is a member of every workspace it holds, these
//! included: for a workspace hosted here its session goes to this host over an
//! in-process carrier (`WorkspaceService::connect_local`), through the same
//! handshake and session code a remote member's takes over iroh. An
//! endpoint cannot dial itself, and a session that never leaves the
//! process keeps no server up and arms no timer.
use std::sync::{Arc, Mutex, PoisonError};

/// Where the workspace host lives: the one place its path is named.
pub use tilo_workspace_service::service as workspace_service;
/// The ALPN handlers a host serves, for the actor's router.
pub use workspace_service::Protocols;
use workspace_service::{ServiceSlot, WorkspaceService};

/// This device's workspace host, once it is open: shared by the network (which
/// opens it) and the actor (which joins its workspaces through it).
#[derive(Clone)]
pub struct OwnService(Arc<Inner>);

struct Inner {
    slot: ServiceSlot,
    host: Mutex<Option<Arc<WorkspaceService>>>,
}

impl std::fmt::Debug for OwnService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OwnService")
            .field("open", &self.get().is_some())
            .finish()
    }
}

impl OwnService {
    /// A host not open yet, and the deferred handlers that serve it once it
    /// is, for the router.
    #[must_use]
    pub fn deferred() -> (Self, Protocols) {
        let (slot, protocols) = workspace_service::deferred_protocols();
        (
            Self(Arc::new(Inner {
                slot,
                host: Mutex::new(None),
            })),
            protocols,
        )
    }

    /// The open host, if this device hosts workspaces.
    #[must_use]
    pub fn get(&self) -> Option<Arc<WorkspaceService>> {
        self.0
            .host
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The workspace-join exchange with this device's host, in process, as
    /// the device `caller` (this endpoint's own key): what enrolling into a
    /// workspace hosted here, or renewing membership of one, talks to. `None`
    /// while the host is not open.
    #[must_use]
    pub fn join_client(
        &self,
        caller: [u8; 32],
    ) -> Option<tilo_peer_state::join_transport::WorkspaceJoinClient> {
        let host = self.get()?;
        Some(tilo_peer_state::join_transport::WorkspaceJoinClient::local(
            host.join_service(),
            caller,
        ))
    }

    /// Serve `host` from now on: its protocols answer on the router, and
    /// the native owner reaches its workspaces through it. A host already
    /// installed stays; the one answered is the host in use.
    pub fn install(&self, host: Arc<WorkspaceService>) -> Arc<WorkspaceService> {
        let mut installed = self.0.host.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(current) = installed.as_ref() {
            return Arc::clone(current);
        }
        self.0.slot.install(Arc::clone(&host));
        *installed = Some(Arc::clone(&host));
        host
    }
}
