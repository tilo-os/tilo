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
    service: ServiceSlot,
    installed: Mutex<Option<Arc<WorkspaceService>>>,
    /// The service did not open, and will not in this session.
    failed: std::sync::atomic::AtomicBool,
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
        let (service, protocols) = workspace_service::deferred_protocols();
        (
            Self(Arc::new(Inner {
                service,
                installed: Mutex::new(None),
                failed: std::sync::atomic::AtomicBool::new(false),
            })),
            protocols,
        )
    }

    /// [`Self::deferred`] over a [`workspace_service::ServiceSlot`] the caller
    /// already holds: the service this serves is found there too, without the
    /// caller having to reach this handle's thread.
    #[must_use]
    pub fn deferred_in(service: workspace_service::ServiceSlot) -> (Self, Protocols) {
        let protocols = workspace_service::deferred_protocols_in(&service);
        (
            Self(Arc::new(Inner {
                service,
                installed: Mutex::new(None),
                failed: std::sync::atomic::AtomicBool::new(false),
            })),
            protocols,
        )
    }

    /// The open host, if this device hosts workspaces.
    #[must_use]
    pub fn get(&self) -> Option<Arc<WorkspaceService>> {
        self.0
            .installed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The service did not open: the workspaces placed on this device cannot
    /// be served here in this session.
    pub fn failed_to_open(&self) {
        self.0
            .failed
            .store(true, std::sync::atomic::Ordering::Release);
    }

    /// Whether the service did not open ([`Self::failed_to_open`]); while it is
    /// still opening, it is on its way.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.get().is_none() && self.0.failed.load(std::sync::atomic::Ordering::Acquire)
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
    pub fn install(&self, service: Arc<WorkspaceService>) -> Arc<WorkspaceService> {
        let mut installed = self
            .0
            .installed
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(current) = installed.as_ref() {
            return Arc::clone(current);
        }
        self.0.service.install(Arc::clone(&service));
        *installed = Some(Arc::clone(&service));
        service
    }
}
