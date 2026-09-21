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
    CertifiedEndpointIdentity, CollabEvent, CollabRequest, CollaborationManager,
    DocumentPublicationError, DocumentPublicationReceipt, RouteSpec,
};

/// One connection to the broker.
pub struct Link {
    manager: CollaborationManager,
    pending: VecDeque<CollabEvent>,
    last_publication: LastPublication,
}

enum LastPublication {
    None,
    Pending(DocumentPublicationReceipt),
    Failed(DocumentPublicationError),
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
            last_publication: LastPublication::None,
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
            last_publication: LastPublication::None,
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
            last_publication: LastPublication::None,
        })
    }

    /// Enqueue one request and record an ordered receipt for document publications.
    ///
    /// # Errors
    ///
    /// Returns an error if the request or its document completion marker cannot enqueue.
    pub fn send(&mut self, req: &CollabRequest) -> Result<()> {
        let queued = self.manager.send(req.clone());
        self.record_enqueue(req, queued)
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
    /// Returns an error if the request or its document completion marker cannot enqueue.
    pub fn send_to_route(&mut self, route: RouteSpec, req: &CollabRequest) -> Result<()> {
        let queued = self.manager.try_send_to_route(route, req.clone());
        self.record_enqueue(req, queued)
    }

    fn record_enqueue(&mut self, req: &CollabRequest, queued: bool) -> Result<()> {
        let document = matches!(
            req,
            CollabRequest::Document { .. } | CollabRequest::SignedOperation { .. }
        );
        if !queued {
            // Healthy presence shedding is an explicit transport contract.
            if matches!(req, CollabRequest::Presence { .. }) && self.manager.is_connected() {
                return Ok(());
            }
            let error =
                DocumentPublicationError::Failed("collaboration request enqueue rejected".into());
            if document {
                self.last_publication = LastPublication::Failed(error.clone());
            }
            return Err(error.into());
        }
        if document {
            if let LastPublication::Failed(error) = &self.last_publication {
                return Err(error.clone().into());
            }
            match self.manager.begin_document_publication_barrier() {
                Ok(receipt) => self.last_publication = LastPublication::Pending(receipt),
                Err(error) => {
                    // Invalidate the older prefix even if the caller ignores this error.
                    self.last_publication = LastPublication::Failed(error.clone());
                    return Err(error.into());
                }
            }
        }
        Ok(())
    }

    /// Complete the latest document prefix while retaining the inbound receiver.
    /// Events polled around the wait remain available to the normal session loop.
    ///
    /// # Errors
    /// Returns a publication/transport error or exhaustion of the supplied deadline.
    pub fn complete_publications(&mut self, deadline: Option<std::time::Instant>) -> Result<()> {
        let publication = std::mem::replace(&mut self.last_publication, LastPublication::None);
        let result = match publication {
            LastPublication::None => return Ok(()),
            LastPublication::Failed(error) => Err(error),
            LastPublication::Pending(receipt) => {
                self.pending.extend(self.manager.poll());
                let result = receipt.wait(deadline);
                self.pending.extend(self.manager.poll());
                result
            }
        };
        if let Err(error) = result {
            self.last_publication = LastPublication::Failed(error.clone());
            return Err(error.into());
        }
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

    struct AcceptedPeer;

    impl ui::collaboration::PeerTransport for AcceptedPeer {
        fn is_connected(&self) -> bool {
            true
        }
        fn send(&mut self, _: CollabRequest) -> std::result::Result<(), String> {
            Ok(())
        }
        fn poll(&mut self) -> Vec<CollabEvent> {
            Vec::new()
        }
    }

    fn peer_link() -> Link {
        Link {
            manager: CollaborationManager::with_peer_transport(AcceptedPeer),
            pending: VecDeque::new(),
            last_publication: LastPublication::None,
        }
    }

    #[test]
    fn idle_link_needs_no_completion_protocol_or_extra_deadline() {
        let mut link = peer_link();
        link.complete_publications(Some(std::time::Instant::now()))
            .unwrap();
    }

    #[test]
    fn accepted_document_without_completion_support_is_sticky_failure() {
        let mut link = peer_link();
        assert!(
            link.send(&CollabRequest::document_incremental(vec![1]))
                .is_err()
        );
        assert!(link.complete_publications(None).is_err());
        assert!(link.complete_publications(None).is_err());
    }

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
