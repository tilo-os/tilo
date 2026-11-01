//! Each workspace's direct sends, on a lane of its own.
//!
//! The actor keeps every workspace's state and decides what goes where; the
//! sending itself (a write to one device on its direct connection) waits on
//! other devices, and one slow or unreachable device must hold up only its
//! own workspace. So each workspace's direct sends run in order on its own spawned
//! task, and the actor only queues them there and never waits for them. How
//! a send went comes back as an [`InternalEvent`].
//!
//! A lane's queue is bounded: a workspace whose devices stall fills only its own
//! queue, and what no longer fits goes another way or not at all.

use std::collections::HashMap;
use std::time::Duration;

use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointAddr, EndpointId};
use tokio::sync::mpsc;

use super::{InternalEvent, SURFACE_STREAM_ALPN, WorkspaceId};
use neighbour_wire::NeighbourMsg;

/// How many direct sends one workspace may have waiting. Beyond it the workspace's
/// devices are not keeping up, and more waiting would only send stale
/// state late.
const LANE_DEPTH: usize = 256;

/// How long a direct connection may take to come up.
const DIRECT_DIAL: Duration = Duration::from_millis(300);

/// A message for exactly one device on the authenticated direct plane.
pub(super) struct Direct {
    pub(super) addr: EndpointAddr,
    /// The connection to the device the actor already holds.
    pub(super) cached: Option<Connection>,
    pub(super) msg: NeighbourMsg,
    pub(super) bytes: Vec<u8>,
    /// How long the write may take before it no longer helps.
    pub(super) budget: Duration,
}

/// How a [`Direct`] went, for the actor.
pub(super) struct DirectOutcome {
    pub(super) target: EndpointId,
    pub(super) msg: NeighbourMsg,
    /// The connection to keep for the device: `None` once it failed.
    pub(super) connection: Option<Connection>,
    pub(super) delivered: bool,
}

/// Every workspace's lane.
pub(super) struct Lanes {
    endpoint: Endpoint,
    internal_tx: mpsc::Sender<InternalEvent>,
    workspaces: HashMap<WorkspaceId, Lane>,
}

struct Lane {
    queue: mpsc::Sender<Direct>,
    task: tokio::task::JoinHandle<()>,
}

impl Lanes {
    pub(super) fn new(endpoint: Endpoint, internal_tx: mpsc::Sender<InternalEvent>) -> Self {
        Self {
            endpoint,
            internal_tx,
            workspaces: HashMap::new(),
        }
    }

    /// `ws`'s lane, started when the workspace has none yet.
    fn lane(&mut self, ws: WorkspaceId) -> &Lane {
        self.workspaces.entry(ws).or_insert_with(|| {
            let (queue, rx) = mpsc::channel(LANE_DEPTH);
            let task = tokio::spawn(run(ws, rx, self.endpoint.clone(), self.internal_tx.clone()));
            Lane { queue, task }
        })
    }

    /// Queue `direct` on `ws`'s lane. Gives its message back when the
    /// workspace's lane is full, for the caller to send another way.
    pub(super) fn queue_direct(
        &mut self,
        ws: WorkspaceId,
        direct: Direct,
    ) -> Result<(), Box<NeighbourMsg>> {
        match self.lane(ws).queue.try_reserve() {
            Ok(permit) => {
                permit.send(direct);
                Ok(())
            }
            Err(_) => Err(Box::new(direct.msg)),
        }
    }

    /// The workspace was left: what its lane still had to send goes with it.
    pub(super) fn close(&mut self, ws: WorkspaceId) {
        if let Some(lane) = self.workspaces.remove(&ws) {
            lane.task.abort();
        }
    }
}

impl Drop for Lanes {
    fn drop(&mut self) {
        for lane in self.workspaces.values() {
            lane.task.abort();
        }
    }
}

/// One workspace's lane: its direct sends, in the order they were queued.
async fn run(
    ws: WorkspaceId,
    mut work: mpsc::Receiver<Direct>,
    endpoint: Endpoint,
    internal_tx: mpsc::Sender<InternalEvent>,
) {
    // Connections this lane brought up and the actor has not yet heard of.
    let mut connections: HashMap<EndpointId, Connection> = HashMap::new();
    while let Some(mut direct) = work.recv().await {
        let target = direct.addr.id;
        if let Some(connection) = connections.remove(&target) {
            direct.cached.get_or_insert(connection);
        }
        let outcome = deliver(&endpoint, direct).await;
        if let Some(connection) = &outcome.connection {
            connections.insert(target, connection.clone());
        }
        let event = InternalEvent::DirectSent {
            ws,
            outcome: Box::new(outcome),
        };
        if internal_tx.send(event).await.is_err() {
            return;
        }
    }
}

/// Write `direct` on a connection to its device, bringing one up if none is
/// held or the held one closed.
async fn deliver(endpoint: &Endpoint, direct: Direct) -> DirectOutcome {
    let Direct {
        addr,
        cached,
        msg,
        bytes,
        budget,
    } = direct;
    let target = addr.id;
    let cached = cached.filter(|connection| connection.close_reason().is_none());
    let connection = match cached {
        Some(connection) => Some(connection),
        None => tokio::time::timeout(DIRECT_DIAL, endpoint.connect(addr, SURFACE_STREAM_ALPN))
            .await
            .ok()
            .and_then(Result::ok),
    };
    let Some(connection) = connection else {
        tracing::warn!(peer = %target, "direct connection failed");
        return DirectOutcome {
            target,
            msg,
            connection: None,
            delivered: false,
        };
    };
    let send = async {
        let mut stream = connection.open_uni().await.map_err(|_| ())?;
        stream.write_all(&bytes).await.map_err(|_| ())?;
        stream.finish().map_err(|_| ())?;
        Ok::<(), ()>(())
    };
    let delivered = tokio::select! {
        biased;
        _ = connection.closed() => false,
        result = tokio::time::timeout(budget, send) => matches!(result, Ok(Ok(()))),
    };
    if !delivered {
        tracing::warn!(peer = %target, "direct publish failed");
    }
    DirectOutcome {
        target,
        msg,
        connection: delivered.then_some(connection),
        delivered,
    }
}
