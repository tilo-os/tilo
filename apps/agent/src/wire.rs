//! Blocking facade over the typed collaboration client.
//!
//! Production connects through the launcher-inherited `QOS_SVC_COLLAB_FD`
//! capability. Headless diagnostics use the same protocol through an
//! explicit Cap'n Proto endpoint.

use std::collections::VecDeque;
use std::os::fd::IntoRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use ui::collaboration::{CollabEvent, CollabRequest, CollaborationManager, RouteSpec};

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
    /// Returns an error when `QOS_SVC_COLLAB_FD` is missing or the launcher's
    /// inherited capability cannot be turned into a typed collaboration
    /// client (wrong fd type, broker not listening, handshake rejected).
    pub fn connect(
        app_id: &str,
        cluster_id: uuid::Uuid,
        record: Option<String>,
        handle: Option<String>,
    ) -> Result<Self> {
        let manager = CollaborationManager::for_route(app_id, cluster_id, record, handle)
            .context("connect through QOS_SVC_COLLAB_FD")?;
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
    ) -> Result<Self> {
        let stream = UnixStream::connect(path)
            .with_context(|| format!("connect to typed endpoint at {}", path.display()))?;
        let manager = CollaborationManager::for_route_fd(
            stream.into_raw_fd(),
            app_id,
            cluster_id,
            record,
            handle,
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
                CollabEvent::CRDTIncremental { data: vec![1] },
                CollabEvent::CRDTUpdate { data: vec![2] },
                CollabEvent::CRDTIncremental { data: vec![3] },
            ],
        );

        assert!(matches!(
            first,
            Some(CollabEvent::CRDTIncremental { data }) if data == [1]
        ));
        assert!(matches!(
            take_next_polled(&mut pending, Vec::new()),
            Some(CollabEvent::CRDTUpdate { data }) if data == [2]
        ));
        assert!(matches!(
            take_next_polled(&mut pending, Vec::new()),
            Some(CollabEvent::CRDTIncremental { data }) if data == [3]
        ));
        assert!(take_next_polled(&mut pending, Vec::new()).is_none());
    }
}
