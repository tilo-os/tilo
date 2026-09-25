//! Blocking facade over the typed collaboration client.
//!
//! Production connects through the launcher-inherited `TILO_SVC_COLLAB_FD`
//! capability. Headless diagnostics use the same protocol through an
//! explicit Cap'n Proto endpoint.

use std::collections::VecDeque;
use std::os::fd::IntoRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use ui::collaboration::{
    CertifiedEndpointIdentity, CollabEvent, CollabRequest, CollaborationManager, RouteSpec,
};

/// One connection to the broker.
pub struct Link {
    manager: CollaborationManager,
    pending: VecDeque<CollabEvent>,
}

fn take_next_polled(
    pending: &mut VecDeque<CollabEvent>,
    polled: Vec<CollabEvent>,
) -> Option<CollabEvent> {
    pending.extend(polled);
    pending.pop_front()
}

impl Link {
    /// Connect to the typed service capability inherited from the launcher.
    ///
    /// # Errors
    ///
    /// Returns an error when `TILO_SVC_COLLAB_FD` is missing or the launcher's
    /// inherited capability cannot be turned into a typed collaboration
    /// client (wrong fd type, broker not listening, handshake rejected).
    pub fn connect(
        app_id: &str,
        cluster_id: uuid::Uuid,
        record: Option<String>,
        handle: Option<String>,
    ) -> Result<Self> {
        let agent_id = handle.ok_or_else(|| anyhow::anyhow!("agent connection needs an id"))?;
        let manager = CollaborationManager::for_agent_route(app_id, cluster_id, record, agent_id)
            .context("connect through TILO_SVC_COLLAB_FD")?;
        Ok(Self {
            manager,
            pending: VecDeque::new(),
        })
    }

    /// Connect to an explicit typed Cap'n Proto socket. This is for headless
    /// diagnostics that run outside the launcher but still exercise the
    /// production protocol.
    ///
    /// # Errors
    ///
    /// Returns an error when the Unix socket at `path` cannot be connected
    /// to, or when the typed collaboration client fails its handshake over
    /// that socket.
    pub fn connect_endpoint(
        path: &Path,
        app_id: &str,
        cluster_id: uuid::Uuid,
        record: Option<String>,
        handle: Option<String>,
        identity: Option<CertifiedEndpointIdentity>,
    ) -> Result<Self> {
        let stream = UnixStream::connect(path)
            .with_context(|| format!("connect to typed endpoint at {}", path.display()))?;
        let fd = stream.into_raw_fd();
        let agent_id = handle.ok_or_else(|| anyhow::anyhow!("agent connection needs an id"))?;
        let manager = match identity {
            Some(identity) => CollaborationManager::for_agent_route_fd_with_identity(
                fd, app_id, cluster_id, record, agent_id, identity,
            ),
            None => {
                CollaborationManager::for_agent_route_fd(fd, app_id, cluster_id, record, agent_id)
            }
        }
        .context("start typed collaboration client")?;
        Ok(Self {
            manager,
            pending: VecDeque::new(),
        })
    }

    /// Connect a certified human peer to an explicit diagnostic endpoint.
    ///
    /// The agent integration harness hosts real application peers alongside
    /// the agent. Keeping this constructor distinct prevents those app peers
    /// from being misrepresented as delegated agents during admission.
    #[doc(hidden)]
    pub fn connect_person_endpoint(
        path: &Path,
        app_id: &str,
        cluster_id: uuid::Uuid,
        record: Option<String>,
        handle: Option<String>,
        identity: CertifiedEndpointIdentity,
    ) -> Result<Self> {
        let stream = UnixStream::connect(path)
            .with_context(|| format!("connect to typed endpoint at {}", path.display()))?;
        let manager = CollaborationManager::for_route_fd_with_identity(
            stream.into_raw_fd(),
            app_id,
            cluster_id,
            record,
            handle,
            identity,
        )
        .context("start typed collaboration client")?;
        Ok(Self {
            manager,
            pending: VecDeque::new(),
        })
    }

    /// Write one request. Blocking: a full-document snapshot can be large
    /// and must not be truncated by a would-block.
    ///
    /// # Errors
    ///
    /// Reserved for a future fallible transport; currently always succeeds.
    pub fn send(&mut self, req: &CollabRequest) -> Result<()> {
        self.manager.send(req.clone());
        Ok(())
    }

    /// Wait until every document mutation sent so far has been handed to the
    /// broker. [`Self::send`] only queues: dropping the link with a mutation
    /// still queued discards it, so a session that is about to end calls this
    /// first.
    ///
    /// # Errors
    ///
    /// When one of those mutations was not published, or the transport
    /// stopped before answering.
    pub fn flush_documents(&mut self) -> Result<()> {
        self.manager
            .begin_document_publication_barrier()?
            .wait(None)?;
        Ok(())
    }

    /// Subscribe to another typed route. This is used for cluster-wide
    /// discovery; mutations still require [`Self::send_to_route`].
    pub fn join_route(&mut self, route: RouteSpec) {
        self.manager.join_route(route);
    }

    /// Publish on one explicit non-primary route. Naming the route at every
    /// call keeps a discovery subscription from becoming a mutation target.
    ///
    /// # Errors
    ///
    /// Reserved for a future fallible transport; currently always succeeds.
    pub fn send_to_route(&mut self, route: RouteSpec, req: &CollabRequest) -> Result<()> {
        self.manager.send_to_route(route, req.clone());
        Ok(())
    }

    /// Read the next event, waiting at most `timeout`. `Ok(None)` on
    /// timeout; `Err` once the broker has gone away.
    ///
    /// # Errors
    ///
    /// Reserved for a future fallible transport; currently always succeeds
    /// (a dead broker currently surfaces as an indefinite empty poll, not an
    /// `Err`).
    pub fn recv(&mut self, timeout: Duration) -> Result<Option<CollabEvent>> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(event) = take_next_polled(&mut self.pending, self.manager.poll()) {
                return Ok(Some(event));
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            std::thread::sleep((deadline - now).min(Duration::from_millis(5)));
        }
    }

    /// Put events consumed while waiting for a correlated broker reply back
    /// in front of the normal session loop, preserving their order.
    pub fn defer(&mut self, events: Vec<CollabEvent>) {
        for event in events.into_iter().rev() {
            self.pending.push_front(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_poll_batches_keep_every_event_in_order() {
        let mut pending = VecDeque::new();
        let first = take_next_polled(
            &mut pending,
            vec![
                CollabEvent::RemoteCursorMoved {
                    user_id: "one".into(),
                    x: 1.0,
                    y: 0.0,
                },
                CollabEvent::RemoteCursorMoved {
                    user_id: "two".into(),
                    x: 2.0,
                    y: 0.0,
                },
                CollabEvent::RemoteCursorMoved {
                    user_id: "three".into(),
                    x: 3.0,
                    y: 0.0,
                },
            ],
        );

        assert!(matches!(
            first,
            Some(CollabEvent::RemoteCursorMoved { x: 1.0, .. })
        ));
        assert!(matches!(
            take_next_polled(&mut pending, Vec::new()),
            Some(CollabEvent::RemoteCursorMoved { x: 2.0, .. })
        ));
        assert!(matches!(
            take_next_polled(&mut pending, Vec::new()),
            Some(CollabEvent::RemoteCursorMoved { x: 3.0, .. })
        ));
        assert!(take_next_polled(&mut pending, Vec::new()).is_none());
    }
}
