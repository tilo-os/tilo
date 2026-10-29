//! The host side of the collaboration wire (`runtime_collab::wire`): one
//! dispatcher over its requests, with the channels a connection opened kept
//! here under their handles and its subscriptions pumped onto the one
//! outgoing frame queue.

use std::cell::Cell;

use runtime_collab::wire::{
    Call, Event, FRAMING, Handle, HostFrame, Publication, Reply, Request, Topic,
};
use tokio::io::{AsyncRead, AsyncWrite};

use super::*;

/// A channel's presence as one admitted endpoint has it: what it publishes,
/// and the feed of everyone else's.
pub(super) struct PresenceSession {
    state: Arc<CapnpBrokerState>,
    channel_id: String,
    peer_id: u64,
    verified_identity: AdmittedJoinIdentity,
    /// Broker-owned monotonic sequence per invocation. Publishers describe
    /// phases and targets but cannot reorder or resurrect their visual state.
    interaction_sequences: Mutex<HashMap<tilo_actions::InvocationId, u64>>,
}

impl PresenceSession {
    pub(super) fn new(
        state: Arc<CapnpBrokerState>,
        channel_id: String,
        peer_id: u64,
        verified_identity: AdmittedJoinIdentity,
    ) -> Self {
        Self {
            state,
            channel_id,
            peer_id,
            verified_identity,
            interaction_sequences: Mutex::new(HashMap::new()),
        }
    }

    /// Widened to room scope — presence is room-wide, only document
    /// payloads are confined to a record.
    fn route(&self) -> CrdtRoute {
        self.full_route().to_cluster_scope()
    }

    /// Unwidened, so peer replication carries the full app/cluster route.
    fn full_route(&self) -> CrdtRoute {
        CrdtRoute::from_channel_key(&self.channel_id)
    }

    fn revalidate(&self) -> Result<(), String> {
        let route = if self.verified_identity.host_scope.is_some() {
            self.full_route()
        } else {
            self.route()
        };
        self.state
            .revalidate_admission(&route, &self.verified_identity)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// Publish a keyless pointer position: the cursor stream's event and
    /// the default pointer key's presence.
    pub(super) fn publish_cursor(&self, x: f32, y: f32) -> Result<(), String> {
        self.revalidate()?;
        let (route, full_route) = (self.route(), self.full_route());
        let event = CollabEvent::RemoteCursorMoved {
            user_id: CapnpBrokerState::participant_id(&self.verified_identity),
            x,
            y,
        };
        self.state.publish_presence(
            &full_route,
            &PresenceKey::pointer_default(),
            &PresencePayload::Pointer { x, y },
        );
        if let Some(fanout) = self.state.fanout.upgrade() {
            fanout.fanout_presence(&event, self.peer_id, &route, &full_route);
        }
        Ok(())
    }

    /// Publish one keyed presence update after the broker's own checks: a
    /// status is trimmed and bounded, an interaction is attributed to this
    /// endpoint and sequenced, and the desktop's own presence is refused.
    pub(super) fn publish(&self, key: PresenceKey, payload: PresencePayload) -> Result<(), String> {
        self.revalidate()?;
        let payload = match payload {
            PresencePayload::Status { text, automatic } => {
                let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
                if text.chars().count() > ui::MAX_STATUS_CHARS {
                    return Err("status exceeds local protocol bound".into());
                }
                PresencePayload::Status { text, automatic }
            }
            PresencePayload::Interaction(update) => {
                PresencePayload::Interaction(Box::new(self.attributed(*update)?))
            }
            PresencePayload::Availability { .. } => {
                return Err("desktop presence is the shell's, never an app's".into());
            }
            other => other,
        };
        let route = self.route();
        self.state
            .publish_presence(&self.full_route(), &key, &payload);
        let event = CollabEvent::RemotePresenceUpdate {
            user_id: CapnpBrokerState::participant_id(&self.verified_identity),
            key,
            payload,
        };
        if let Some(fanout) = self.state.fanout.upgrade() {
            fanout.fanout_presence(&event, self.peer_id, &route, &self.full_route());
        }
        Ok(())
    }

    /// `update` as this endpoint's: its actor is the admitted one, and its
    /// sequence the broker's next for the invocation.
    fn attributed(
        &self,
        update: tilo_actions::SemanticInteraction,
    ) -> Result<tilo_actions::SemanticInteraction, String> {
        let actor = self.verified_identity.broker_actor();
        let normalized_actor = tilo_actions::InteractionActor::new(
            CapnpBrokerState::participant_id(&self.verified_identity),
            if actor.is_agent {
                tilo_actions::ActorKind::Agent
            } else {
                tilo_actions::ActorKind::Person
            },
        );
        let sequence = {
            let mut sequences = self
                .interaction_sequences
                .lock()
                .map_err(|_| "interaction sequence registry unavailable".to_owned())?;
            let current = sequences.entry(update.invocation).or_insert(0);
            *current = current.saturating_add(1);
            *current
        };
        let update = update.normalized_by_broker(normalized_actor, sequence);
        update
            .validate()
            .map_err(|error| format!("invalid interaction: {error}"))?;
        Ok(update)
    }

    /// Everyone else's presence in this room from now on.
    pub(super) fn subscribe(&self) -> Result<PresenceFeed, String> {
        self.revalidate()?;
        Ok(PresenceFeed {
            host_guard: HostWorkerReadGuard::for_channel(
                &self.state,
                self.peer_id,
                &self.verified_identity,
            ),
            rx: self
                .state
                .subscribe_presence(&self.channel_id, self.peer_id),
        })
    }
}

/// The presence a [`PresenceSession`] subscribed to.
pub(super) struct PresenceFeed {
    host_guard: Option<HostWorkerReadGuard>,
    rx: mpsc::UnboundedReceiver<PresenceEnvelope>,
}

impl PresenceFeed {
    /// The next update, or `None` when the room's fanout closed. A host
    /// worker that is no longer current ends the feed with an error, checked
    /// on both sides of the wait.
    pub(super) async fn next(&mut self) -> Result<Option<PresenceEnvelope>, String> {
        HostWorkerReadGuard::check(self.host_guard.as_ref()).map_err(|error| error.to_string())?;
        let next = self.rx.recv().await;
        HostWorkerReadGuard::check(self.host_guard.as_ref()).map_err(|error| error.to_string())?;
        Ok(next)
    }
}

/// What a connection opened under one handle.
pub(super) struct ChannelSession {
    pub(super) presence: PresenceSession,
}

/// One endpoint's connection: the handles it holds, its live subscriptions,
/// and the queue every outgoing frame takes.
pub(crate) struct WireConnection {
    channels: RefCell<HashMap<Handle, Rc<ChannelSession>>>,
    next_handle: Cell<u64>,
    subscriptions: RefCell<HashMap<u64, (Handle, tokio::task::AbortHandle)>>,
    frames: mpsc::UnboundedSender<HostFrame>,
}

impl Drop for WireConnection {
    fn drop(&mut self) {
        for (_, (_, task)) in self.subscriptions.get_mut().drain() {
            task.abort();
        }
    }
}

impl WireConnection {
    /// A connection and the queue of the frames it sends.
    pub(crate) fn new() -> (Rc<Self>, mpsc::UnboundedReceiver<HostFrame>) {
        let (frames, outgoing) = mpsc::unbounded_channel();
        let connection = Rc::new(Self {
            channels: RefCell::default(),
            next_handle: Cell::new(1),
            subscriptions: RefCell::default(),
            frames,
        });
        (connection, outgoing)
    }

    /// Hold `session` for this connection under a fresh handle.
    pub(super) fn open(&self, session: ChannelSession) -> Handle {
        let handle = Handle(self.next_handle.get());
        self.next_handle.set(handle.0.wrapping_add(1));
        self.channels.borrow_mut().insert(handle, Rc::new(session));
        handle
    }

    fn channel(&self, handle: Handle) -> Result<Rc<ChannelSession>, String> {
        self.channels
            .borrow()
            .get(&handle)
            .cloned()
            .ok_or_else(|| format!("no channel {} on this connection", handle.0))
    }

    /// Answer one call; its reply, and any events it starts, take the
    /// outgoing queue in that order.
    pub(crate) fn call(self: &Rc<Self>, Call { id, request }: Call) {
        let result = self.dispatch(id, request);
        let _ = self.frames.send(HostFrame::Reply { id, result });
    }

    fn dispatch(self: &Rc<Self>, id: u64, request: Request) -> Result<Reply, String> {
        match request {
            Request::Publish {
                channel,
                publication: Publication::Presence(presence),
            } => {
                let (key, payload) = crate::collab_presence::from_peer(presence)
                    .ok_or_else(|| "presence does not decode".to_owned())?;
                self.channel(channel)?.presence.publish(key, payload)?;
                Ok(Reply::Done)
            }
            Request::Subscribe {
                channel,
                topic: Topic::Presence,
            } => {
                if self.subscriptions.borrow().contains_key(&id) {
                    return Err(format!("subscription {id} is already live"));
                }
                let feed = self.channel(channel)?.presence.subscribe()?;
                let task = tokio::task::spawn_local(pump_presence(feed, id, self.frames.clone()));
                self.subscriptions
                    .borrow_mut()
                    .insert(id, (channel, task.abort_handle()));
                Ok(Reply::Subscribed)
            }
            Request::Unsubscribe { subscription } => {
                let (_, task) = self
                    .subscriptions
                    .borrow_mut()
                    .remove(&subscription)
                    .ok_or_else(|| format!("no subscription {subscription}"))?;
                task.abort();
                Ok(Reply::Done)
            }
            Request::Close { handle } => {
                self.channels
                    .borrow_mut()
                    .remove(&handle)
                    .ok_or_else(|| format!("no channel {} on this connection", handle.0))?;
                self.subscriptions
                    .borrow_mut()
                    .retain(|_, (channel, task)| {
                        let keep = *channel != handle;
                        if !keep {
                            task.abort();
                        }
                        keep
                    });
                Ok(Reply::Done)
            }
        }
    }
}

async fn pump_presence(
    mut feed: PresenceFeed,
    subscription: u64,
    frames: mpsc::UnboundedSender<HostFrame>,
) {
    let reason = loop {
        match feed.next().await {
            Ok(Some(envelope)) => {
                let Some(payload) = crate::collab_presence::payload_to_peer(&envelope.payload)
                else {
                    continue;
                };
                let event = Event::Presence {
                    user_id: envelope.user_id,
                    presence: collab_types::PeerPresence {
                        key: envelope.key_bytes,
                        payload,
                    },
                };
                if frames
                    .send(HostFrame::Event {
                        subscription,
                        event,
                    })
                    .is_err()
                {
                    return;
                }
            }
            Ok(None) => break "presence fanout closed".to_owned(),
            Err(error) => break error,
        }
    };
    let _ = frames.send(HostFrame::Ended {
        subscription,
        reason,
    });
}

type NextCall<R> = (R, Result<Option<Call>, wire_frame::FrameError>);

async fn next_call<R: AsyncRead + Unpin>(mut reader: R) -> NextCall<R> {
    let call = FRAMING.read_value_async(&mut reader).await;
    (reader, call)
}

/// Serve `connection` until the endpoint hangs up. Must run on a
/// `LocalSet`; dropping the future ends the connection's subscriptions.
///
/// # Errors
///
/// A malformed frame or a failed write.
pub(crate) async fn serve<R, W>(
    connection: Rc<WireConnection>,
    mut outgoing: mpsc::UnboundedReceiver<HostFrame>,
    reader: R,
    mut writer: W,
) -> Result<(), wire_frame::FrameError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt as _;
    let mut next = Box::pin(next_call(reader));
    loop {
        tokio::select! {
            (reader, call) = &mut next => {
                let Some(call) = call? else {
                    return Ok(());
                };
                connection.call(call);
                next = Box::pin(next_call(reader));
            }
            Some(frame) = outgoing.recv() => {
                FRAMING.write_value_async(&mut writer, &frame).await?;
                writer.flush().await?;
            }
        }
    }
}

#[cfg(test)]
#[path = "wire_tests.rs"]
mod wire_tests;
