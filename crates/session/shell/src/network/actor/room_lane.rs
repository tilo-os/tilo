//! Each room's remote sends, on a lane of its own.
//!
//! The actor keeps every room's state and decides what goes where; the
//! sending itself (gossip broadcasts, joining gossip peers, subscribing to
//! the room's topic, writes on a direct connection) waits on other devices,
//! and one slow or unreachable device must hold up only its own room. So
//! each room's remote work runs in order on its own spawned task, and the
//! actor only queues it there and never waits for it. What the actor must
//! hear back (a subscription, how a direct send went) comes as an
//! [`InternalEvent`].
//!
//! A lane's queue is bounded: a room whose devices stall fills only its own
//! queue, and what no longer fits is shed there. Live traffic is for
//! whoever hears it now; retained state is still with the room's hosts, and
//! the room's peers catch up on it by sync.

use std::collections::HashMap;
use std::time::Duration;

use bytes::Bytes;
use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointAddr, EndpointId};
use iroh_gossip::api::GossipSender;
use iroh_gossip::net::Gossip;
use tokio::sync::mpsc;

use super::{InternalEvent, SURFACE_STREAM_ALPN, WorkspaceId, topic_id};
use crate::network::protocol::NeighbourMsg;

/// How much remote work one room may have waiting. Beyond it the room's
/// devices are not keeping up, and more waiting would only send stale
/// state late.
const LANE_DEPTH: usize = 256;

/// How long a direct connection may take to come up.
const DIRECT_DIAL: Duration = Duration::from_millis(300);

/// One unit of a room's remote work.
pub(super) enum Work {
    /// Subscribe to the room's gossip topic through `bootstrap`; the
    /// subscription comes back as [`InternalEvent::Subscribed`].
    Subscribe {
        serial: u64,
        bootstrap: Vec<EndpointId>,
    },
    /// Broadcast on the room's topic.
    Broadcast(Bytes),
    /// Broadcast to the room's direct gossip neighbours only.
    Neighbours(Bytes),
    /// Ask the room's topic to join these peers.
    JoinPeers(Vec<EndpointId>),
    /// Write one message to one device on the direct plane; how it went
    /// comes back as [`InternalEvent::DirectSent`].
    Direct(Direct),
}

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

/// Every room's lane.
pub(super) struct Lanes {
    gossip: Gossip,
    endpoint: Endpoint,
    internal_tx: mpsc::Sender<InternalEvent>,
    lanes: HashMap<WorkspaceId, Lane>,
}

struct Lane {
    queue: mpsc::Sender<Work>,
    task: tokio::task::JoinHandle<()>,
}

impl Lanes {
    pub(super) fn new(
        gossip: Gossip,
        endpoint: Endpoint,
        internal_tx: mpsc::Sender<InternalEvent>,
    ) -> Self {
        Self {
            gossip,
            endpoint,
            internal_tx,
            lanes: HashMap::new(),
        }
    }

    /// `ws`'s lane, started when the room has none yet.
    fn lane(&mut self, ws: WorkspaceId) -> &Lane {
        self.lanes.entry(ws).or_insert_with(|| {
            let (queue, rx) = mpsc::channel(LANE_DEPTH);
            let task = tokio::spawn(run(
                ws,
                rx,
                self.gossip.clone(),
                self.endpoint.clone(),
                self.internal_tx.clone(),
            ));
            Lane { queue, task }
        })
    }

    /// Queue `work` on `ws`'s lane; `false` when the room's lane is full.
    pub(super) fn queue(&mut self, ws: WorkspaceId, work: Work) -> bool {
        self.lane(ws).queue.try_send(work).is_ok()
    }

    /// Queue `work` on `ws`'s lane, or shed it when the room's lane is full:
    /// for gossip, which is best effort anyway.
    pub(super) fn queue_or_shed(&mut self, ws: WorkspaceId, work: Work) {
        if !self.queue(ws, work) {
            tracing::debug!(ws, "room's remote work is backed up; shed a gossip send");
        }
    }

    /// Queue `direct` on `ws`'s lane. Gives its message back when the
    /// room's lane is full, for the caller to send another way.
    pub(super) fn queue_direct(
        &mut self,
        ws: WorkspaceId,
        direct: Direct,
    ) -> Result<(), NeighbourMsg> {
        match self.lane(ws).queue.try_reserve() {
            Ok(permit) => {
                permit.send(Work::Direct(direct));
                Ok(())
            }
            Err(_) => Err(direct.msg),
        }
    }

    /// The room was left: what its lane still had to send goes with it.
    pub(super) fn close(&mut self, ws: WorkspaceId) {
        if let Some(lane) = self.lanes.remove(&ws) {
            lane.task.abort();
        }
    }
}

impl Drop for Lanes {
    fn drop(&mut self) {
        for lane in self.lanes.values() {
            lane.task.abort();
        }
    }
}

/// One room's lane: its remote work, in the order it was queued.
async fn run(
    ws: WorkspaceId,
    mut work: mpsc::Receiver<Work>,
    gossip: Gossip,
    endpoint: Endpoint,
    internal_tx: mpsc::Sender<InternalEvent>,
) {
    let mut sender: Option<GossipSender> = None;
    // Connections this lane brought up and the actor has not yet heard of.
    let mut connections: HashMap<EndpointId, Connection> = HashMap::new();
    while let Some(next) = work.recv().await {
        let event = match next {
            Work::Subscribe { serial, bootstrap } => {
                let receiver = match gossip.subscribe(topic_id(ws), bootstrap).await {
                    Ok(topic) => {
                        let (topic_sender, receiver) = topic.split();
                        sender = Some(topic_sender);
                        Some(receiver)
                    }
                    Err(error) => {
                        tracing::warn!(ws, %error, "could not join iroh workspace topic");
                        None
                    }
                };
                InternalEvent::Subscribed {
                    ws,
                    serial,
                    receiver: receiver.map(Box::new),
                }
            }
            Work::Broadcast(bytes) => {
                if let Some(sender) = &sender
                    && let Err(error) = sender.broadcast(bytes).await
                {
                    tracing::warn!(ws, %error, "iroh gossip publish failed");
                }
                continue;
            }
            Work::Neighbours(bytes) => {
                if let Some(sender) = &sender
                    && let Err(error) = sender.broadcast_neighbors(bytes).await
                {
                    tracing::warn!(ws, %error, "could not send to the room's gossip neighbours");
                }
                continue;
            }
            Work::JoinPeers(peers) => {
                if let Some(sender) = &sender
                    && let Err(error) = sender.join_peers(peers).await
                {
                    tracing::debug!(ws, %error, "gossip peer join will be retried");
                }
                continue;
            }
            Work::Direct(mut direct) => {
                let target = direct.addr.id;
                if let Some(connection) = connections.remove(&target) {
                    direct.cached.get_or_insert(connection);
                }
                let outcome = deliver(&endpoint, direct).await;
                if let Some(connection) = &outcome.connection {
                    connections.insert(target, connection.clone());
                }
                InternalEvent::DirectSent {
                    ws,
                    outcome: Box::new(outcome),
                }
            }
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
