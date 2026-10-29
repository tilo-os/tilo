//! The host side of the collaboration wire (`runtime_collab::wire`): one
//! dispatcher over its requests, with the channels a connection opened kept
//! here under their handles and its subscriptions pumped onto the one
//! outgoing frame queue.

use std::cell::Cell;

use runtime_collab::wire::{
    Call, ContextUpdate, Event, FRAMING, Handle, HostFrame, Publication, Read, Reply, Request,
    Topic,
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
    pub(super) fn subscribe(&self) -> Result<Feed<PresenceEnvelope>, String> {
        self.revalidate()?;
        Ok(Feed::new(
            HostWorkerReadGuard::for_channel(&self.state, self.peer_id, &self.verified_identity),
            self.state
                .subscribe_presence(&self.channel_id, self.peer_id),
        ))
    }
}

/// What a subscription receives from the broker's fanout. A host worker's
/// feed ends with an error once that worker is no longer current, and a
/// native window's once its room or document custody ended, checked on
/// both sides of every wait.
pub(super) struct Feed<T> {
    host_guard: Option<HostWorkerReadGuard>,
    native_guard: Option<NativeContextReadGuard>,
    rx: mpsc::UnboundedReceiver<T>,
}

impl<T> Feed<T> {
    pub(super) fn new(
        host_guard: Option<HostWorkerReadGuard>,
        rx: mpsc::UnboundedReceiver<T>,
    ) -> Self {
        Self {
            host_guard,
            native_guard: None,
            rx,
        }
    }

    pub(super) fn with_native_guard(mut self, guard: Option<NativeContextReadGuard>) -> Self {
        self.native_guard = guard;
        self
    }

    fn check(&self) -> Result<(), capnp::Error> {
        HostWorkerReadGuard::check(self.host_guard.as_ref())?;
        NativeContextReadGuard::check(self.native_guard.as_ref())
    }

    /// The next item, or `None` when the fanout closed.
    pub(super) async fn next(&mut self) -> Result<Option<T>, capnp::Error> {
        self.check()?;
        let next = self.rx.recv().await;
        self.check()?;
        Ok(next)
    }
}

/// What a connection opened under one handle: one admitted endpoint's hold
/// on a channel, and its presence there.
pub(super) struct ChannelSession {
    channel: Rc<CapnpChannelHandle>,
    presence: PresenceSession,
}

impl ChannelSession {
    pub(super) fn new(channel: Rc<CapnpChannelHandle>) -> Self {
        let presence = PresenceSession::new(
            channel.state.clone(),
            channel.channel_id.clone(),
            channel.connection_id,
            channel.verified_identity.clone(),
        );
        Self { channel, presence }
    }
}

impl std::ops::Deref for ChannelSession {
    type Target = CapnpChannelHandle;

    fn deref(&self) -> &CapnpChannelHandle {
        &self.channel
    }
}

impl CapnpChannelHandle {
    /// Admit, commit where the room's owner must hold it first, and fan out
    /// one signed document change.
    pub(super) async fn send(&self, envelope: SignedOp) -> Result<(), capnp::Error> {
        self.writable()?;
        let (state, route) = (&self.state, self.route());
        let current_identity = state.revalidate_admission(&route, &self.verified_identity)?;
        verify_session_author(&current_identity, envelope.author)
            .map_err(|error| capnp::Error::failed(format!("send: {error}")))?;
        state.authorize_certified_document(&route, &current_identity, &envelope)?;
        let owner_required =
            state.durable_owner_required || state.publisher.requires_owner_for(&route.cluster_id);
        let _owner_order = if owner_required {
            Some(state.owner_commit_order.lock().await)
        } else {
            None
        };
        if owner_required {
            state.revalidate_admission(&route, &self.verified_identity)?;
            state
                .commit_owner_request(
                    tilo_owner_commit::OwnerCommitRequest {
                        room: route.cluster_id.clone(),
                        operation: tilo_owner_commit::CommitOperation::Document {
                            app: route.app_id.clone(),
                            record: route.record_id.clone(),
                            envelope: Box::new(envelope.clone()),
                        },
                    },
                    self.record_kind.clone(),
                )
                .await?;
        }
        let current_identity = state.revalidate_admission(&route, &self.verified_identity)?;
        let authorized =
            state.authorize_certified_document(&route, &current_identity, &envelope)?;
        handle_authorized_signed_op_send(
            state,
            self.connection_id,
            &route,
            &current_identity,
            &authorized,
            envelope,
        );
        Ok(())
    }

    /// Every document change on this channel from now on, other endpoints'
    /// and peers'.
    pub(super) fn subscribe_ops(&self) -> Result<Feed<OpEnvelope>, capnp::Error> {
        self.state
            .revalidate_admission(&self.route(), &self.verified_identity)?;
        Ok(Feed::new(
            self.host_guard(),
            self.state
                .subscribe_ops(&self.channel_id, self.connection_id),
        ))
    }

    fn host_guard(&self) -> Option<HostWorkerReadGuard> {
        HostWorkerReadGuard::for_channel(&self.state, self.peer_id, &self.verified_identity)
    }

    /// The host's record-content service, for a channel still admitted.
    fn record_content(
        &self,
    ) -> Result<(CrdtRoute, Arc<crate::record_content::RouteContent>), capnp::Error> {
        self.state
            .revalidate_admission(&self.route(), &self.verified_identity)?;
        let route = self.route();
        if route.app_id.is_empty() || route.cluster_id.is_empty() {
            return Err(capnp::Error::failed(
                "record content belongs to a certified app route".into(),
            ));
        }
        let content = self.state.services.record_content.clone().ok_or_else(|| {
            capnp::Error::unimplemented("this host keeps no record content".into())
        })?;
        Ok((route, content))
    }

    /// Keep content the record references, which the app stored.
    pub(super) fn offer_content(&self, hash: &str, bytes: &[u8]) -> Result<(), capnp::Error> {
        self.writable()?;
        let (route, content) = self.record_content()?;
        content
            .offer(&route, hash, bytes)
            .map_err(capnp::Error::failed)
    }

    /// Fetch content the record references and the app lacks; it arrives on
    /// the content feed.
    pub(super) fn want_content(&self, hash: &str) -> Result<(), capnp::Error> {
        let (route, content) = self.record_content()?;
        content.want(&route, hash);
        Ok(())
    }

    /// The channel's context from now on, starting with its current
    /// roster. A native private window's context also follows the
    /// continuation it shows.
    pub(super) fn subscribe_context(&self) -> Result<Feed<ContextUpdate>, capnp::Error> {
        let route = self.route();
        self.state
            .revalidate_admission(&route, &self.verified_identity)?;
        let native = self
            .state
            .native_roster_for_subscriber(&route, &self.verified_identity)
            .map_err(capnp::Error::failed)?;
        let instance = self
            .native_private_window_instance()
            .map_err(capnp::Error::failed)?;
        let document = match instance {
            Some(_) => Some(self.document_read_context().map_err(capnp::Error::failed)?),
            None => None,
        };
        let rx = self
            .state
            .subscribe_context(&self.channel_id, self.connection_id, native.clone());
        if let Some(instance) = instance {
            let alias = CrdtRoute::record(
                &route.app_id,
                &route.cluster_id,
                format!("continuation:{instance}"),
            )
            .channel_key();
            self.state
                .alias_private_context(&self.channel_id, &alias, self.connection_id);
        }
        let native_guard = native.map(|native| NativeContextReadGuard {
            state: self.state.clone(),
            route,
            identity: native.admitted,
            document,
        });
        Ok(Feed::new(self.host_guard(), rx).with_native_guard(native_guard))
    }

    /// Ask another endpoint holding this channel to publish the document's
    /// current state.
    pub(super) fn ask_for_document(&self) -> Result<(), capnp::Error> {
        self.state
            .revalidate_admission(&self.route(), &self.verified_identity)?;
        let _ = self
            .state
            .request_document_snapshot(&self.channel_id, self.connection_id);
        Ok(())
    }

    /// Up to `max` admitted changes to `artifact` before `before_seq`, and
    /// whether that is all there is. Where local history ends, the
    /// channel's other devices are asked for what lies before it; their
    /// answer lands in durable history, so the page is reported as not
    /// exhausted and the endpoint's next read picks it up.
    pub(super) fn history(
        &self,
        artifact: ArtifactId,
        before_seq: u64,
        max: u32,
    ) -> Result<(Vec<Vec<u8>>, bool), capnp::Error> {
        let route = self.route();
        self.state
            .revalidate_admission(&route, &self.verified_identity)?;
        let broker_channels = &self.state.broker_channels;
        let (ops, mut exhausted) = read_scrollback(broker_channels, artifact, before_seq, max);
        if exhausted {
            let peer_bound = ops
                .iter()
                .filter_map(|bytes| postcard::from_bytes::<SignedOp>(bytes).ok())
                .map(|op| op.seq)
                .min()
                .unwrap_or(before_seq);
            let ask = broker_channels
                .lock()
                .is_ok_and(|mut ch| ch.request_peer_history(artifact, peer_bound));
            if ask
                && self.state.publisher().scrollback_request(
                    &route,
                    artifact,
                    peer_bound,
                    u16::try_from(max).unwrap_or(u16::MAX),
                )
            {
                exhausted = false;
            }
        }
        Ok((ops, exhausted))
    }

    /// News of the record's content from now on.
    pub(super) fn subscribe_content(
        &self,
    ) -> Result<Feed<crate::record_content::ContentNews>, capnp::Error> {
        let (route, content) = self.record_content()?;
        Ok(Feed::new(self.host_guard(), content.subscribe(&route)))
    }
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

    /// Answer one call. A reply, and the events a subscription it starts
    /// delivers, take the outgoing queue in that order; a call that waits
    /// (a document change the room's owner commits first) answers when done.
    pub(crate) fn call(self: &Rc<Self>, Call { id, request }: Call) {
        let result = match request {
            Request::Publish {
                channel,
                publication: Publication::Document(envelope),
            } => match self.channel(channel) {
                Ok(channel) => {
                    let frames = self.frames.clone();
                    tokio::task::spawn_local(async move {
                        let result = channel
                            .send(*envelope)
                            .await
                            .map(|()| Reply::Done)
                            .map_err(|error| error.to_string());
                        let _ = frames.send(HostFrame::Reply { id, result });
                    });
                    return;
                }
                Err(error) => Err(error),
            },
            request => self.dispatch(id, request),
        };
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
            Request::Publish {
                publication: Publication::Document(_),
                ..
            } => Err("a document change is answered when committed".into()),
            Request::Publish {
                channel,
                publication: Publication::Content { hash, bytes },
            } => {
                self.channel(channel)?
                    .offer_content(&hash, &bytes)
                    .map_err(|error| error.to_string())?;
                Ok(Reply::Done)
            }
            Request::Read {
                channel,
                read:
                    Read::History {
                        artifact,
                        before_seq,
                        max,
                    },
            } => {
                let (ops, exhausted) = self
                    .channel(channel)?
                    .history(artifact, before_seq, max)
                    .map_err(|error| error.to_string())?;
                Ok(Reply::History { ops, exhausted })
            }
            Request::Read {
                channel,
                read: Read::Document,
            } => {
                self.channel(channel)?
                    .ask_for_document()
                    .map_err(|error| error.to_string())?;
                Ok(Reply::Done)
            }
            Request::Read {
                channel,
                read: Read::Content { hash },
            } => {
                self.channel(channel)?
                    .want_content(&hash)
                    .map_err(|error| error.to_string())?;
                Ok(Reply::Done)
            }
            Request::Subscribe { channel, topic } => {
                if self.subscriptions.borrow().contains_key(&id) {
                    return Err(format!("subscription {id} is already live"));
                }
                let session = self.channel(channel)?;
                let frames = self.frames.clone();
                let failed = |error: capnp::Error| error.to_string();
                let task = match topic {
                    Topic::Presence => {
                        let feed = session.presence.subscribe()?;
                        tokio::task::spawn_local(pump(feed, id, frames, presence_event))
                    }
                    Topic::Ops => {
                        let feed = session.subscribe_ops().map_err(failed)?;
                        tokio::task::spawn_local(pump(feed, id, frames, |envelope: OpEnvelope| {
                            Some(Event::Op {
                                op: envelope.op_bytes,
                                from_user_id: envelope.from_user_id,
                                admission: envelope.admission,
                            })
                        }))
                    }
                    Topic::Context => {
                        let feed = session.subscribe_context().map_err(failed)?;
                        tokio::task::spawn_local(pump(feed, id, frames, |update| {
                            Some(Event::Context(update))
                        }))
                    }
                    Topic::Content => {
                        let feed = session.subscribe_content().map_err(failed)?;
                        tokio::task::spawn_local(pump(
                            feed,
                            id,
                            frames,
                            |news: crate::record_content::ContentNews| {
                                Some(Event::Content {
                                    hash: news.hash,
                                    state: news.state,
                                    bytes: news.bytes,
                                })
                            },
                        ))
                    }
                };
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

/// Another participant's presence as the wire carries it; `None` for what
/// no app receives or what does not fit the wire.
fn presence_event(envelope: PresenceEnvelope) -> Option<Event> {
    if !envelope.payload.reaches_apps() {
        return None;
    }
    Some(Event::Presence {
        user_id: envelope.user_id,
        presence: collab_types::PeerPresence {
            key: envelope.key_bytes,
            payload: crate::collab_presence::payload_to_peer(&envelope.payload)?,
        },
    })
}

/// Deliver `feed` as the subscription `subscription`'s events until it
/// ends, then say why.
async fn pump<T>(
    mut feed: Feed<T>,
    subscription: u64,
    frames: mpsc::UnboundedSender<HostFrame>,
    event: impl Fn(T) -> Option<Event>,
) {
    let reason = loop {
        match feed.next().await {
            Ok(Some(item)) => {
                let Some(event) = event(item) else {
                    continue;
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
            Ok(None) => break "the channel's fanout closed".to_owned(),
            Err(error) => break error.to_string(),
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
