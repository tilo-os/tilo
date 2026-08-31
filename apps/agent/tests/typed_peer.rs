//! Small app-side peer used by the agent's integration tests.
//!
//! The assertions in these tests predate the native collaboration service,
//! so the adapter deliberately retains their request/event-shaped driving
//! code while putting every byte on the production Cap'n Proto endpoint.

#![allow(
    dead_code,
    reason = "each integration binary uses a different adapter subset"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::io;
use std::path::Path;
use std::time::Duration;

use qos_agent::wire::Link;
use ui::collaboration::{CollabEvent, CollabRequest};

pub struct TypedPeer {
    link: Option<Link>,
    path: std::path::PathBuf,
    timeout: Duration,
}

impl TypedPeer {
    pub fn connect(path: &Path) -> Self {
        Self {
            link: None,
            path: path.to_path_buf(),
            timeout: Duration::from_millis(200),
        }
    }

    /// # Errors
    ///
    /// Returns an error when `request` is not `CollabRequest::Hello`, or when
    /// the typed collaboration client fails to connect to `path`.
    pub fn announce(&mut self, path: &Path, request: &CollabRequest) -> anyhow::Result<()> {
        let CollabRequest::Hello {
            app_id,
            cluster_id,
            record,
            handle,
        } = request
        else {
            anyhow::bail!("the first request must announce the peer route")
        };
        self.link = Some(Link::connect_endpoint(
            path,
            app_id,
            *cluster_id,
            record.clone(),
            handle.clone(),
        )?);
        Ok(())
    }

    /// # Errors
    ///
    /// Returns an error when the peer has not yet announced its route via
    /// [`Self::announce`], or when the underlying typed link fails to send.
    pub fn send(&mut self, request: &CollabRequest) -> anyhow::Result<()> {
        self.link
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("peer has not announced its route"))?
            .send(request)
    }

    /// # Errors
    ///
    /// Returns an error when the peer has not yet announced its route via
    /// [`Self::announce`], or when the underlying typed link fails to
    /// receive.
    pub fn recv(&mut self) -> anyhow::Result<Option<CollabEvent>> {
        self.link
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("peer has not announced its route"))?
            .recv(self.timeout)
    }

    pub fn set_read_timeout(&mut self, timeout: Option<Duration>) {
        self.timeout = timeout.unwrap_or(Duration::from_millis(200));
    }
}

/// # Errors
///
/// Returns an error when announcing the route fails (for a `Hello` request)
/// or when the peer's underlying send fails.
pub fn send(peer: &mut TypedPeer, request: &CollabRequest) -> io::Result<()> {
    let result = if matches!(request, CollabRequest::Hello { .. }) {
        let path = peer.path.clone();
        peer.announce(&path, request)
    } else {
        peer.send(request)
    };
    result.map_err(io::Error::other)
}

/// # Panics
///
/// Panics if the underlying typed link errors, which in these tests means
/// the collaboration broker has gone away.
pub fn recv(peer: &mut TypedPeer) -> Option<CollabEvent> {
    peer.recv()
        .expect("receive from typed collaboration service")
}

/// # Panics
///
/// Panics if the typed collaboration service cannot bind `path`.
pub fn spawn(path: &Path) -> shell::shell::collab_broker::BrokerHandle {
    shell::shell::collab_broker::spawn_native(
        path.to_path_buf(),
        &None,
        shell::shell::state::WorkspaceTree::default().shared(),
    )
    .expect("bind the typed collaboration service")
}
