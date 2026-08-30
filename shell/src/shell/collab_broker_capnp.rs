// `Promise::from_future(async move {..})` refines capnp-rpc's declared
// `Promise<.., capnp::Error>` return type — the intended pattern, but it
// trips `refining_impl_trait`.
#![allow(refining_impl_trait)]

//! Native Cap'n Proto server for the qOS collaboration broker.
//!
//! Binds a second Unix socket alongside the legacy JSON one and answers the
//! `runtime-collab` schema; both share the fanout state in
//! [`crate::shell::collab_broker`], so a subscriber on either transport sees
//! every event regardless of which transport its author used.
//!
//! `join_channel` uses the caller's `channel_id` verbatim as the routing
//! key. `sendUnsigned*` share the JSON fanout but skip role enforcement —
//! use `send` with a signed op for that. `fetchScrollback*` answer from
//! the in-memory op log; an exhausted-but-empty batch triggers no
//! fallback push, callers repeat the fetch instead.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread::JoinHandle;
use std::time::Duration;

use capnp::capability::Promise;
use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
use tokio::sync::mpsc;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use collab_types::{
    ArtifactId, OpClass, RedactedMarker, SignedOp, ValidationError, validate_inbound_op,
};
use runtime_collab::raw::{
    OpKind, channel_handle, collab_service, context_stream, cursor_stream, presence_event,
    presence_handle, presence_payload, presence_stream, scrollback_batch, stream,
};
use ui::collaboration::{CollabEvent, PresencePayload};
use ui::presence::PresenceKey;

use crate::shell::collab_broker::{
    AuditEntry, BrokerChannels, CrdtRoute, LocalIdentity, PeerTable, apply_redaction_from_envelope,
};
use crate::shell::collab_outbound::{PeerPublisher, Transport};

/// Envelope pushed onto an [`OpStreamServer`]'s queue. `kind` is what
/// makes the queue readable: signed, delta and whole-document share
/// this type and are indistinguishable from the bytes alone.
#[derive(Clone, Debug)]
struct OpEnvelope {
    op_bytes: Vec<u8>,
    from_user_id: String,
    kind: OpKind,
}

/// Envelope pushed onto a [`PresenceStreamServer`]'s queue. Carries
/// every presence variant; the legacy [`CursorStreamServer`] filters
/// to Cursor only on read.
#[derive(Clone, Debug)]
struct PresenceEnvelope {
    user_id: String,
    key_bytes: Vec<u8>,
    payload: PresencePayload,
}

/// Envelope pushed onto a [`CursorStreamServer`]'s queue.
#[derive(Clone, Debug)]
struct CursorEnvelope {
    user_id: String,
    x: f32,
    y: f32,
}

/// Envelope pushed onto a [`ContextStreamServer`]'s queue.
#[derive(Clone, Debug)]
struct ContextEnvelope {
    collaborators: Vec<ui::collaboration::Collaborator>,
    permissions: Vec<String>,
}

/// One subscription onto the broker fanout, addressable by [`Self::send`]
/// regardless of which envelope type it carries — lets
/// [`CapnpBrokerState::fanout_to_route`] share one delivery loop
/// ([`retain_deliverable`]) across `op`/`presence`/`cursor`/`context`
/// instead of repeating the same retain-and-send closure four times.
trait Subscriber {
    type Msg: Clone;
    fn peer_id(&self) -> u64;
    fn send(&self, msg: Self::Msg) -> bool;
}

/// Drops every subscriber in `subs` whose receiver has gone away, and
/// sends `env` to the rest — except `except`, which is skipped (kept
/// subscribed) rather than sent to, so a peer never echoes its own op
/// back to itself.
fn retain_deliverable<S: Subscriber>(subs: &mut Vec<S>, except: u64, env: &S::Msg) {
    subs.retain(|s| {
        if s.peer_id() == except {
            return true;
        }
        s.send(env.clone())
    });
}

/// One subscription onto the broker fanout. Slot-indexed inside a
/// [`ChannelSubscribers`] table so it can be dropped en masse when
/// the connection tears down.
struct OpSub {
    peer_id: u64,
    tx: mpsc::UnboundedSender<OpEnvelope>,
}

impl Subscriber for OpSub {
    type Msg = OpEnvelope;
    fn peer_id(&self) -> u64 {
        self.peer_id
    }
    fn send(&self, msg: OpEnvelope) -> bool {
        self.tx.send(msg).is_ok()
    }
}

struct PresenceSub {
    peer_id: u64,
    tx: mpsc::UnboundedSender<PresenceEnvelope>,
}

impl Subscriber for PresenceSub {
    type Msg = PresenceEnvelope;
    fn peer_id(&self) -> u64 {
        self.peer_id
    }
    fn send(&self, msg: PresenceEnvelope) -> bool {
        self.tx.send(msg).is_ok()
    }
}

struct CursorSub {
    peer_id: u64,
    tx: mpsc::UnboundedSender<CursorEnvelope>,
}

impl Subscriber for CursorSub {
    type Msg = CursorEnvelope;
    fn peer_id(&self) -> u64 {
        self.peer_id
    }
    fn send(&self, msg: CursorEnvelope) -> bool {
        self.tx.send(msg).is_ok()
    }
}

struct ContextSub {
    peer_id: u64,
    tx: mpsc::UnboundedSender<ContextEnvelope>,
}

impl Subscriber for ContextSub {
    type Msg = ContextEnvelope;
    fn peer_id(&self) -> u64 {
        self.peer_id
    }
    fn send(&self, msg: ContextEnvelope) -> bool {
        self.tx.send(msg).is_ok()
    }
}

/// Per-channel subscriber registry, keyed by `join_channel(id)`. A
/// `channelId` carries the whole address ([`ui::collaboration::channel_key`]
/// encodes `(app, cluster, record)`), decoded via
/// [`CrdtRoute::from_channel_key`]; delivery is [`CrdtRoute::delivers_to`].
#[derive(Default)]
struct ChannelSubscribers {
    op: Vec<OpSub>,
    presence: Vec<PresenceSub>,
    cursor: Vec<CursorSub>,
    context: Vec<ContextSub>,
}

/// Shared capnp broker state. Attached to a [`PeerTable`] so the
/// legacy `fanout_to_route` / `send_to_peer` calls also reach capnp
/// subscribers.
pub struct CapnpBrokerState {
    /// Keyed by `join_channel`'s `channel_id`; lazily inserted on first join.
    channels: Mutex<HashMap<String, ChannelSubscribers>>,
    /// `Weak` so broker teardown isn't blocked by an outliving connection.
    peer_table: Weak<PeerTable>,
    /// Same instance the legacy path validates against, so a signed op
    /// arriving via capnp mutates the exact same `ChannelState`.
    broker_channels: Arc<Mutex<BrokerChannels>>,
    /// Same instance the legacy transport holds. A publisher with no
    /// network behind it ([`PeerPublisher::disabled`]) is legitimate
    /// (unit tests, a shell started without peer sync).
    publisher: Arc<PeerPublisher>,
    /// Same instance the JSON transport resolves through — an app must
    /// not become a different participant on a different socket.
    identity: Arc<LocalIdentity>,
}

impl CapnpBrokerState {
    /// So tests can register channel state before pushing signed
    /// envelopes through the capnp path.
    pub fn broker_channels(&self) -> Arc<Mutex<BrokerChannels>> {
        Arc::clone(&self.broker_channels)
    }

    fn new(
        peer_table: &Arc<PeerTable>,
        broker_channels: Arc<Mutex<BrokerChannels>>,
        publisher: Arc<PeerPublisher>,
        identity: Arc<LocalIdentity>,
    ) -> Arc<Self> {
        Arc::new(Self {
            channels: Mutex::new(HashMap::new()),
            peer_table: Arc::downgrade(peer_table),
            broker_channels,
            publisher,
            identity,
        })
    }

    /// `joinChannel(channelId, identity)` carries no announced handle,
    /// so the room lookup is the only answer for who to stamp traffic as.
    fn participant_id(&self, route: &CrdtRoute) -> String {
        self.identity.resolve(Some(route), None)
    }

    fn publisher(&self) -> &Arc<PeerPublisher> {
        &self.publisher
    }

    /// Converts once into the transport-neutral peer presence envelope.
    /// Peer replication does not embed either local RPC dialect.
    fn publish_presence(&self, route: &CrdtRoute, key: &PresenceKey, payload: &PresencePayload) {
        let Some(presence) = crate::shell::collab_presence::to_peer(key, payload) else {
            tracing::warn!(
                channel = %route.channel_key(),
                "capnp broker: presence would not encode; peers will \
                 not see this person move",
            );
            return;
        };
        self.publisher().presence(route, Transport::Capnp, presence);
    }

    /// Delegates to the shared [`PeerTable`] so ids never collide with
    /// the legacy side.
    fn next_peer_id(&self) -> u64 {
        // Peer table torn down: racing with shutdown. Hand out a
        // placeholder id; the caller drops it on the next tokio yield.
        self.peer_table
            .upgrade()
            .map_or(u64::MAX, |t| t.next_peer_id())
    }

    /// Drive an event onto the subscribed queues `route` addresses,
    /// skipping `except`. Called from [`PeerTable::fanout_to_route`]
    /// once it has written the JSON peers. Delegates to
    /// [`CrdtRoute::delivers_to`] rather than reimplementing the filter,
    /// so this transport's delivery rule cannot drift from the JSON side's.
    pub fn fanout_to_route(&self, event: &CollabEvent, except: u64, route: &CrdtRoute) {
        if route.cluster_id.is_empty() {
            return;
        }
        let Ok(mut all_channels) = self.channels.lock() else {
            return;
        };
        let channels = all_channels
            .iter_mut()
            .filter(|(k, _)| route.delivers_to(&CrdtRoute::from_channel_key(k)))
            .map(|(_, subs)| subs);
        match event {
            CollabEvent::SignedCRDTIncremental { .. }
            | CollabEvent::CRDTIncremental { .. }
            | CollabEvent::CRDTUpdate { .. } => {
                Self::fanout_op_event(channels, except, event);
            }
            CollabEvent::RemoteCursorMoved { user_id, x, y } => {
                let key_bytes = default_pointer_key_bytes();
                let presence_env = PresenceEnvelope {
                    user_id: user_id.clone(),
                    key_bytes,
                    payload: PresencePayload::Pointer { x: *x, y: *y },
                };
                let cursor_env = CursorEnvelope {
                    user_id: user_id.clone(),
                    x: *x,
                    y: *y,
                };
                for subs in channels {
                    retain_deliverable(&mut subs.presence, except, &presence_env);
                    retain_deliverable(&mut subs.cursor, except, &cursor_env);
                }
            }
            CollabEvent::RemotePresenceUpdate {
                user_id,
                key,
                payload,
            } => {
                let key_bytes = match postcard::to_stdvec(key) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(error = %e, "capnp fanout: postcard(PresenceKey) failed");
                        return;
                    }
                };
                let env = PresenceEnvelope {
                    user_id: user_id.clone(),
                    key_bytes,
                    payload: payload.clone(),
                };
                for subs in channels {
                    retain_deliverable(&mut subs.presence, except, &env);
                }
            }
            CollabEvent::Context {
                collaborators,
                permissions,
            } => {
                let env = ContextEnvelope {
                    collaborators: collaborators.clone(),
                    permissions: permissions.clone(),
                };
                for subs in channels {
                    retain_deliverable(&mut subs.context, except, &env);
                }
            }
            CollabEvent::ActionArrived { type_id, .. } => {
                // Unreachable today; said out loud, not silently dropped.
                tracing::warn!(
                    %type_id,
                    "capnp fanout: no action stream yet, action dropped"
                );
            }
            CollabEvent::ScrollbackBatch { .. } => {
                // Per-request in the capnp schema, not a stream; apps
                // wanting late history repeat `fetchScrollbackFor`.
            }
        }
    }

    /// The three [`CollabEvent`] variants [`Self::fanout_to_route`] fans
    /// out as an [`OpEnvelope`] on `subs.op`: a signed CRDT op
    /// (postcard-encoded from its envelope) or an unsigned
    /// incremental/whole-document update. No-op for any other variant.
    fn fanout_op_event(
        channels: impl Iterator<Item = &mut ChannelSubscribers>,
        except: u64,
        event: &CollabEvent,
    ) {
        let env = match event {
            CollabEvent::SignedCRDTIncremental {
                envelope,
                from_user_id,
            } => {
                let op_bytes = match postcard::to_stdvec(&**envelope) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(error = %e, "capnp fanout: postcard(SignedOp) failed");
                        return;
                    }
                };
                OpEnvelope {
                    op_bytes,
                    from_user_id: from_user_id.clone(),
                    kind: OpKind::Signed,
                }
            }
            CollabEvent::CRDTIncremental { data } | CollabEvent::CRDTUpdate { data } => {
                // Empty `from_user_id`, matching JSON; kind tags delta vs.
                // whole-document.
                let kind = match event {
                    CollabEvent::CRDTUpdate { .. } => OpKind::UnsignedSnapshot,
                    _ => OpKind::UnsignedIncremental,
                };
                OpEnvelope {
                    op_bytes: data.clone(),
                    from_user_id: String::new(),
                    kind,
                }
            }
            _ => return,
        };
        for subs in channels {
            retain_deliverable(&mut subs.op, except, &env);
        }
    }

    /// Capnp doesn't use per-peer targeted delivery (scrollback answers
    /// inline); exists so [`PeerTable::send_to_peer`] can call through
    /// unconditionally.
    pub fn send_to_peer(&self, _peer_id: u64, _event: &CollabEvent) -> bool {
        false
    }

    fn ensure_channel(&self, channel_id: &str) {
        if let Ok(mut m) = self.channels.lock() {
            m.entry(channel_id.to_owned())
                .or_insert_with(ChannelSubscribers::default);
        }
    }

    fn subscribe_ops(&self, channel_id: &str, peer_id: u64) -> mpsc::UnboundedReceiver<OpEnvelope> {
        let (tx, rx) = mpsc::unbounded_channel();
        if let Ok(mut m) = self.channels.lock() {
            m.entry(channel_id.to_owned())
                .or_insert_with(ChannelSubscribers::default)
                .op
                .push(OpSub { peer_id, tx });
        }
        rx
    }

    fn subscribe_presence(
        &self,
        channel_id: &str,
        peer_id: u64,
    ) -> mpsc::UnboundedReceiver<PresenceEnvelope> {
        let (tx, rx) = mpsc::unbounded_channel();
        if let Ok(mut m) = self.channels.lock() {
            m.entry(channel_id.to_owned())
                .or_insert_with(ChannelSubscribers::default)
                .presence
                .push(PresenceSub { peer_id, tx });
        }
        rx
    }

    fn subscribe_cursors(
        &self,
        channel_id: &str,
        peer_id: u64,
    ) -> mpsc::UnboundedReceiver<CursorEnvelope> {
        let (tx, rx) = mpsc::unbounded_channel();
        if let Ok(mut m) = self.channels.lock() {
            m.entry(channel_id.to_owned())
                .or_insert_with(ChannelSubscribers::default)
                .cursor
                .push(CursorSub { peer_id, tx });
        }
        rx
    }

    fn subscribe_context(
        &self,
        channel_id: &str,
        peer_id: u64,
    ) -> mpsc::UnboundedReceiver<ContextEnvelope> {
        let (tx, rx) = mpsc::unbounded_channel();
        if let Ok(mut m) = self.channels.lock() {
            m.entry(channel_id.to_owned())
                .or_insert_with(ChannelSubscribers::default)
                .context
                .push(ContextSub { peer_id, tx });
        }
        rx
    }
}

/// Postcard-encode [`PresenceKey::pointer_default`]; cheap enough not
/// to cache.
fn default_pointer_key_bytes() -> Vec<u8> {
    postcard::to_stdvec(&PresenceKey::pointer_default()).unwrap_or_default()
}

// Server implementations

/// Root capability server. One instance per connected RPC session.
struct CapnpCollabService {
    state: Arc<CapnpBrokerState>,
    /// Peer id assigned at accept time. Fanout uses it to skip the
    /// author when replaying its own frames back at itself.
    peer_id: u64,
}

impl collab_service::Server for CapnpCollabService {
    fn join_channel(
        self: Rc<Self>,
        params: collab_service::JoinChannelParams,
        mut results: collab_service::JoinChannelResults,
    ) -> Promise<(), capnp::Error> {
        let state = self.state.clone();
        let peer_id = self.peer_id;
        Promise::from_future(async move {
            let reader = params.get()?;
            let channel_id = reader
                .get_channel_id()?
                .to_str()
                .map_err(|e| capnp::Error::failed(format!("channelId not utf8: {e}")))?
                .to_owned();
            // The broker doesn't dispatch on identity today; bytes are read
            // only to enforce the field is present and decodable.
            let _identity = reader.get_identity()?;

            tracing::info!(
                target: "qos_collab_probe",
                channel = %channel_id,
                peer_id,
                "PROBE join_channel",
            );
            state.ensure_channel(&channel_id);

            let handle = CapnpChannelHandle {
                state: state.clone(),
                channel_id,
                peer_id,
            };
            let client: channel_handle::Client = capnp_rpc::new_client(handle);
            results.get().set_handle(client);
            Ok(())
        })
    }
}

/// Per-channel handle established by `join_channel`; every subsequent
/// method call inherits its (`channel_id`, `peer_id`, identity) context.
struct CapnpChannelHandle {
    state: Arc<CapnpBrokerState>,
    channel_id: String,
    peer_id: u64,
}

impl CapnpChannelHandle {
    /// A record-scoped connection names its record inside the channel id
    /// (the SDK joins `<cluster>#record:<id>`), parsed back out here.
    fn route(&self) -> CrdtRoute {
        CrdtRoute::from_channel_key(&self.channel_id)
    }
}

impl channel_handle::Server for CapnpChannelHandle {
    fn send(
        self: Rc<Self>,
        params: channel_handle::SendParams,
        _results: channel_handle::SendResults,
    ) -> Promise<(), capnp::Error> {
        let state = self.state.clone();
        let peer_id = self.peer_id;
        let route = self.route();
        Promise::from_future(async move {
            let op_bytes = params.get()?.get_op()?.to_vec();
            let envelope: SignedOp = postcard::from_bytes(&op_bytes).map_err(|e| {
                capnp::Error::failed(format!(
                    "capnp send: op is not a postcard-encoded SignedOp: {e}"
                ))
            })?;
            handle_signed_op_send(&state, peer_id, &route, envelope);
            Ok(())
        })
    }

    fn subscribe(
        self: Rc<Self>,
        _params: channel_handle::SubscribeParams,
        mut results: channel_handle::SubscribeResults,
    ) -> Promise<(), capnp::Error> {
        let rx = self.state.subscribe_ops(&self.channel_id, self.peer_id);
        let server = OpStreamServer {
            rx: Rc::new(RefCell::new(rx)),
        };
        let client: stream::Client = capnp_rpc::new_client(server);
        results.get().set_stream(client);
        Promise::ok(())
    }

    fn presence(
        self: Rc<Self>,
        _params: channel_handle::PresenceParams,
        mut results: channel_handle::PresenceResults,
    ) -> Promise<(), capnp::Error> {
        let handle = CapnpPresenceHandle {
            state: self.state.clone(),
            channel_id: self.channel_id.clone(),
            peer_id: self.peer_id,
        };
        let client: presence_handle::Client = capnp_rpc::new_client(handle);
        results.get().set_handle(client);
        Promise::ok(())
    }

    fn send_unsigned_snapshot(
        self: Rc<Self>,
        params: channel_handle::SendUnsignedSnapshotParams,
        _results: channel_handle::SendUnsignedSnapshotResults,
    ) -> Promise<(), capnp::Error> {
        let state = self.state.clone();
        let peer_id = self.peer_id;
        let route = self.route();
        Promise::from_future(async move {
            let data = params.get()?.get_data()?.to_vec();
            state
                .publisher()
                .document_snapshot(&route, Transport::Capnp, data.clone());
            if let Some(t) = state.peer_table.upgrade() {
                t.fanout_to_route(&CollabEvent::CRDTUpdate { data }, peer_id, &route);
            }
            Ok(())
        })
    }

    fn send_unsigned_incremental(
        self: Rc<Self>,
        params: channel_handle::SendUnsignedIncrementalParams,
        _results: channel_handle::SendUnsignedIncrementalResults,
    ) -> Promise<(), capnp::Error> {
        let state = self.state.clone();
        let peer_id = self.peer_id;
        let route = self.route();
        Promise::from_future(async move {
            let data = params.get()?.get_data()?.to_vec();
            state
                .publisher()
                .document_incremental(&route, Transport::Capnp, data.clone());
            if let Some(t) = state.peer_table.upgrade() {
                t.fanout_to_route(&CollabEvent::CRDTIncremental { data }, peer_id, &route);
            }
            Ok(())
        })
    }

    fn subscribe_context(
        self: Rc<Self>,
        _params: channel_handle::SubscribeContextParams,
        mut results: channel_handle::SubscribeContextResults,
    ) -> Promise<(), capnp::Error> {
        let rx = self.state.subscribe_context(&self.channel_id, self.peer_id);
        let server = ContextStreamServer {
            rx: Rc::new(RefCell::new(rx)),
        };
        let client: context_stream::Client = capnp_rpc::new_client(server);
        results.get().set_stream(client);
        Promise::ok(())
    }

    fn fetch_scrollback(
        self: Rc<Self>,
        params: channel_handle::FetchScrollbackParams,
        mut results: channel_handle::FetchScrollbackResults,
    ) -> Promise<(), capnp::Error> {
        // Legacy method: no artifact id, so nil-uuid stands in.
        let broker_channels = self.state.broker_channels.clone();
        Promise::from_future(async move {
            let p = params.get()?;
            let before_seq = p.get_before_seq();
            let max = p.get_max();
            let artifact = ArtifactId(uuid::Uuid::nil());
            let (ops, exhausted) = read_scrollback(&broker_channels, artifact, before_seq, max);
            let batch = results.get().init_batch();
            fill_scrollback_batch(batch, ops, exhausted);
            Ok(())
        })
    }

    fn fetch_scrollback_for(
        self: Rc<Self>,
        params: channel_handle::FetchScrollbackForParams,
        mut results: channel_handle::FetchScrollbackForResults,
    ) -> Promise<(), capnp::Error> {
        let broker_channels = self.state.broker_channels.clone();
        Promise::from_future(async move {
            let p = params.get()?;
            let artifact_id_bytes = p.get_artifact_id()?.to_vec();
            let before_seq = p.get_before_seq();
            let max = p.get_max();
            let uuid = uuid::Uuid::from_slice(&artifact_id_bytes).map_err(|e| {
                capnp::Error::failed(format!(
                    "capnp fetch_scrollback_for: artifactId must be 16 bytes: {e}"
                ))
            })?;
            let artifact = ArtifactId(uuid);
            let (ops, exhausted) = read_scrollback(&broker_channels, artifact, before_seq, max);
            let batch = results.get().init_batch();
            fill_scrollback_batch(batch, ops, exhausted);
            Ok(())
        })
    }
}

/// Apply a signed op arriving via capnp `send`: same validation /
/// audit / redaction / compaction pipeline the legacy path runs, then
/// fans out via the shared [`PeerTable`].
fn handle_signed_op_send(
    state: &Arc<CapnpBrokerState>,
    peer_id: u64,
    route: &CrdtRoute,
    envelope: SignedOp,
) {
    let artifact = envelope.artifact;
    let validation = {
        let Ok(mut ch) = state.broker_channels.lock() else {
            return;
        };
        let ch_state = ch.state_for(artifact);
        validate_inbound_op(&envelope, ch_state)
    };
    match validation {
        Ok(()) => {
            if let Ok(mut ch) = state.broker_channels.lock() {
                let ch_state = ch.state_for(artifact);
                ch_state.record_validated_op(envelope.clone());
                if envelope.op_class == OpClass::Compaction
                    && let Ok(cp) = collab_types::parse_compaction_payload(&envelope)
                {
                    let _ = ch_state.apply_compaction(&cp);
                }
            }
            if envelope.op_class == OpClass::Redact {
                apply_redaction_from_envelope(&envelope, artifact, &state.broker_channels);
            }
            // Republishing is not vouching.
            state
                .publisher()
                .signed_document(route, Transport::Capnp, envelope.clone());
            let event = CollabEvent::SignedCRDTIncremental {
                envelope: Box::new(envelope),
                from_user_id: state.participant_id(route),
            };
            if let Some(t) = state.peer_table.upgrade() {
                t.fanout_to_route(&event, peer_id, route);
            }
        }
        Err(ValidationError::OpAlreadyRedacted { target }) => {
            tracing::warn!(
                artifact = ?artifact,
                seq = envelope.seq,
                target = ?target,
                "capnp send: late-arriving op was already redacted; storing marker",
            );
            if let Ok(mut ch) = state.broker_channels.lock() {
                let ch_state = ch.state_for(artifact);
                let mut marker_envelope = envelope.clone();
                if let Ok(marker_bytes) = RedactedMarker::v1().canonical() {
                    marker_envelope.payload = marker_bytes;
                }
                ch_state.record_validated_op(marker_envelope);
                ch.audit.record(
                    artifact,
                    AuditEntry {
                        envelope: Box::new(envelope),
                        rejection: Some(ValidationError::OpAlreadyRedacted { target }),
                        at: std::time::SystemTime::now(),
                    },
                );
            }
        }
        Err(reason) => {
            tracing::warn!(
                artifact = ?artifact,
                author = ?envelope.author,
                seq = envelope.seq,
                "capnp send: dropping invalid signed op: {reason}",
            );
            if let Ok(mut ch) = state.broker_channels.lock() {
                ch.audit.record(
                    artifact,
                    AuditEntry {
                        envelope: Box::new(envelope),
                        rejection: Some(reason),
                        at: std::time::SystemTime::now(),
                    },
                );
            }
        }
    }
}

/// Read a page of scrollback ops for `artifact`, encoded as postcard
/// bytes since the capnp `ScrollbackBatch.ops` is `List(Data)`.
fn read_scrollback(
    broker_channels: &Arc<Mutex<BrokerChannels>>,
    artifact: ArtifactId,
    before_seq: u64,
    max: u32,
) -> (Vec<Vec<u8>>, bool) {
    let Ok(mut ch) = broker_channels.lock() else {
        return (Vec::new(), true);
    };
    let ch_state = ch.state_for(artifact);
    // A caller asking for more than a u16 can hold gets the max page
    // size instead of a wrapped, much-smaller one.
    let max = u16::try_from(max).unwrap_or(u16::MAX);
    let (ops, exhausted) = ch_state.scrollback_for(artifact, before_seq, max);
    let encoded: Vec<Vec<u8>> = ops
        .into_iter()
        .filter_map(|op| postcard::to_stdvec(&op).ok())
        .collect();
    (encoded, exhausted)
}

fn fill_scrollback_batch(mut b: scrollback_batch::Builder<'_>, ops: Vec<Vec<u8>>, exhausted: bool) {
    // capnp list indices are u32; `ops.len()` realistically never
    // approaches u32::MAX, but clamp (and cap the fill loop to match)
    // rather than let a truncating cast desync the list length from
    // how many ops actually get written into it.
    let n = u32::try_from(ops.len()).unwrap_or(u32::MAX);
    let mut list = b.reborrow().init_ops(n);
    for (i, op) in ops.iter().enumerate().take(n as usize) {
        let Ok(i) = u32::try_from(i) else {
            break;
        };
        list.set(i, op);
    }
    // Bracket seq isn't tracked; consumers needing bounds decode the ops.
    b.set_earliest_seq(0);
    b.set_latest_seq(0);
    b.set_done(exhausted);
}

// Presence handle + streams

struct CapnpPresenceHandle {
    state: Arc<CapnpBrokerState>,
    channel_id: String,
    peer_id: u64,
}

impl CapnpPresenceHandle {
    /// Widened to room scope — presence is room-wide, only document
    /// payloads are confined to a record.
    fn route(&self) -> CrdtRoute {
        self.full_route().to_cluster_scope()
    }

    /// Unwidened, so the peer publish carries the same `(app, cluster)`
    /// pair the JSON bridge stamps on presence.
    fn full_route(&self) -> CrdtRoute {
        CrdtRoute::from_channel_key(&self.channel_id)
    }
}

impl presence_handle::Server for CapnpPresenceHandle {
    fn publish_cursor(
        self: Rc<Self>,
        params: presence_handle::PublishCursorParams,
        _results: presence_handle::PublishCursorResults,
    ) -> Promise<(), capnp::Error> {
        let state = self.state.clone();
        let peer_id = self.peer_id;
        let route = self.route();
        let full_route = self.full_route();
        Promise::from_future(async move {
            let r = params.get()?;
            let (x, y) = (r.get_x(), r.get_y());
            // Legacy gets a keyless `RemoteCursorMoved`; capnp gets real
            // key bytes via `pointer_default()`.
            let event = CollabEvent::RemoteCursorMoved {
                user_id: state.participant_id(&route),
                x,
                y,
            };
            state.publish_presence(
                &full_route,
                &PresenceKey::pointer_default(),
                &PresencePayload::Pointer { x, y },
            );
            if let Some(t) = state.peer_table.upgrade() {
                t.fanout_to_route(&event, peer_id, &route);
            }
            Ok(())
        })
    }

    fn subscribe_remote_cursors(
        self: Rc<Self>,
        _params: presence_handle::SubscribeRemoteCursorsParams,
        mut results: presence_handle::SubscribeRemoteCursorsResults,
    ) -> Promise<(), capnp::Error> {
        let rx = self.state.subscribe_cursors(&self.channel_id, self.peer_id);
        let server = CursorStreamServer {
            rx: Rc::new(RefCell::new(rx)),
        };
        let client: cursor_stream::Client = capnp_rpc::new_client(server);
        results.get().set_stream(client);
        Promise::ok(())
    }

    fn publish_caret(
        self: Rc<Self>,
        params: presence_handle::PublishCaretParams,
        _results: presence_handle::PublishCaretResults,
    ) -> Promise<(), capnp::Error> {
        let state = self.state.clone();
        let peer_id = self.peer_id;
        let route = self.route();
        let full_route = self.full_route();
        Promise::from_future(async move {
            let r = params.get()?;
            let key_bytes = r.get_key_bytes()?.to_vec();
            // A caret position wider than `usize` can't address anything
            // real anyway; clamp rather than let it wrap to a small,
            // wrong offset.
            let pos = usize::try_from(r.get_pos()).unwrap_or(usize::MAX);
            publish_keyed_presence(
                &state,
                peer_id,
                &route,
                &full_route,
                &key_bytes,
                PresencePayload::Caret { pos },
            );
            Ok(())
        })
    }

    fn publish_selection(
        self: Rc<Self>,
        params: presence_handle::PublishSelectionParams,
        _results: presence_handle::PublishSelectionResults,
    ) -> Promise<(), capnp::Error> {
        let state = self.state.clone();
        let peer_id = self.peer_id;
        let route = self.route();
        let full_route = self.full_route();
        Promise::from_future(async move {
            let r = params.get()?;
            let key_bytes = r.get_key_bytes()?.to_vec();
            // Same reasoning as `publish_caret`: clamp rather than wrap.
            let start = usize::try_from(r.get_start()).unwrap_or(usize::MAX);
            let end = usize::try_from(r.get_end()).unwrap_or(usize::MAX);
            publish_keyed_presence(
                &state,
                peer_id,
                &route,
                &full_route,
                &key_bytes,
                PresencePayload::Selection { start, end },
            );
            Ok(())
        })
    }

    fn publish_arrived(
        self: Rc<Self>,
        params: presence_handle::PublishArrivedParams,
        _results: presence_handle::PublishArrivedResults,
    ) -> Promise<(), capnp::Error> {
        let state = self.state.clone();
        let peer_id = self.peer_id;
        let route = self.route();
        let full_route = self.full_route();
        Promise::from_future(async move {
            let r = params.get()?;
            let key_bytes = r.get_key_bytes()?.to_vec();
            publish_keyed_presence(
                &state,
                peer_id,
                &route,
                &full_route,
                &key_bytes,
                PresencePayload::Arrived,
            );
            Ok(())
        })
    }

    fn publish_left(
        self: Rc<Self>,
        params: presence_handle::PublishLeftParams,
        _results: presence_handle::PublishLeftResults,
    ) -> Promise<(), capnp::Error> {
        let state = self.state.clone();
        let peer_id = self.peer_id;
        let route = self.route();
        let full_route = self.full_route();
        Promise::from_future(async move {
            let r = params.get()?;
            let key_bytes = r.get_key_bytes()?.to_vec();
            publish_keyed_presence(
                &state,
                peer_id,
                &route,
                &full_route,
                &key_bytes,
                PresencePayload::Left,
            );
            Ok(())
        })
    }

    fn subscribe_all(
        self: Rc<Self>,
        _params: presence_handle::SubscribeAllParams,
        mut results: presence_handle::SubscribeAllResults,
    ) -> Promise<(), capnp::Error> {
        let rx = self
            .state
            .subscribe_presence(&self.channel_id, self.peer_id);
        let server = PresenceStreamServer {
            rx: Rc::new(RefCell::new(rx)),
        };
        let client: presence_stream::Client = capnp_rpc::new_client(server);
        results.get().set_stream(client);
        Promise::ok(())
    }
}

/// Shared body of the four keyed presence publishers. Drops silently
/// on key-decode failure — a bad blob shouldn't crash the broker.
fn publish_keyed_presence(
    state: &Arc<CapnpBrokerState>,
    peer_id: u64,
    route: &CrdtRoute,
    full_route: &CrdtRoute,
    key_bytes: &[u8],
    payload: PresencePayload,
) {
    let Ok(key) = postcard::from_bytes::<PresenceKey>(key_bytes) else {
        tracing::debug!(
            len = key_bytes.len(),
            "capnp presence publish: key bytes not decodable — dropping",
        );
        return;
    };
    state.publish_presence(full_route, &key, &payload);
    let event = CollabEvent::RemotePresenceUpdate {
        user_id: state.participant_id(route),
        key,
        payload,
    };
    if let Some(t) = state.peer_table.upgrade() {
        t.fanout_to_route(&event, peer_id, route);
    }
}

// Stream servers

struct OpStreamServer {
    rx: Rc<RefCell<mpsc::UnboundedReceiver<OpEnvelope>>>,
}

impl stream::Server for OpStreamServer {
    fn read(
        self: Rc<Self>,
        _params: stream::ReadParams,
        mut results: stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let rx = self.rx.clone();
        Promise::from_future(async move {
            // `poll_fn` rather than `rx.borrow_mut().recv().await`: the
            // latter would hold the `RefMut` across the await point, so a
            // second `read()` call landing on this same `Rc<RefCell<_>>`
            // while this one is still pending would hit `.borrow_mut()`
            // on an already-borrowed cell and panic. Borrowing only for
            // the duration of each individual poll avoids that.
            let Some(env) = std::future::poll_fn(|cx| rx.borrow_mut().poll_recv(cx)).await else {
                return Err(capnp::Error::disconnected("op stream fanout closed".into()));
            };
            let mut r = results.get();
            r.set_op(&env.op_bytes);
            r.set_from_user_id(&env.from_user_id);
            r.set_kind(env.kind);
            Ok(())
        })
    }
}

struct PresenceStreamServer {
    rx: Rc<RefCell<mpsc::UnboundedReceiver<PresenceEnvelope>>>,
}

impl presence_stream::Server for PresenceStreamServer {
    fn read(
        self: Rc<Self>,
        _params: presence_stream::ReadParams,
        mut results: presence_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let rx = self.rx.clone();
        Promise::from_future(async move {
            // See `OpStreamServer::read`: borrowing only per-poll (not
            // across the whole await) keeps a second concurrent `read()`
            // on this same `Rc<RefCell<_>>` from panicking on an
            // already-borrowed cell.
            let Some(env) = std::future::poll_fn(|cx| rx.borrow_mut().poll_recv(cx)).await else {
                return Err(capnp::Error::disconnected(
                    "presence stream fanout closed".into(),
                ));
            };
            let mut ev: presence_event::Builder = results.get().init_event();
            ev.set_user_id(&env.user_id);
            ev.set_key_bytes(&env.key_bytes);
            let mut payload: presence_payload::Builder = ev.init_payload();
            match env.payload {
                PresencePayload::Pointer { x, y } => {
                    let mut c = payload.init_cursor();
                    c.set_x(x);
                    c.set_y(y);
                }
                PresencePayload::Caret { pos } => {
                    payload.init_caret().set_pos(pos as u64);
                }
                // `Typing` has no dedicated case in the capnp union yet,
                // so it rides along on `Selection`'s wire shape.
                PresencePayload::Selection { start, end }
                | PresencePayload::Typing { start, end } => {
                    let mut s = payload.init_selection();
                    s.set_start(start as u64);
                    s.set_end(end as u64);
                }
                PresencePayload::Arrived => payload.set_arrived(()),
                PresencePayload::Left => payload.set_left(()),
            }
            Ok(())
        })
    }
}

struct CursorStreamServer {
    rx: Rc<RefCell<mpsc::UnboundedReceiver<CursorEnvelope>>>,
}

impl cursor_stream::Server for CursorStreamServer {
    fn read(
        self: Rc<Self>,
        _params: cursor_stream::ReadParams,
        mut results: cursor_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let rx = self.rx.clone();
        Promise::from_future(async move {
            // See `OpStreamServer::read` for why this borrows per-poll
            // rather than holding the `RefCell` guard across the await.
            let Some(env) = std::future::poll_fn(|cx| rx.borrow_mut().poll_recv(cx)).await else {
                return Err(capnp::Error::disconnected(
                    "cursor stream fanout closed".into(),
                ));
            };
            let mut r = results.get();
            r.set_user_id(&env.user_id);
            r.set_x(env.x);
            r.set_y(env.y);
            Ok(())
        })
    }
}

struct ContextStreamServer {
    rx: Rc<RefCell<mpsc::UnboundedReceiver<ContextEnvelope>>>,
}

impl context_stream::Server for ContextStreamServer {
    fn read(
        self: Rc<Self>,
        _params: context_stream::ReadParams,
        mut results: context_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let rx = self.rx.clone();
        Promise::from_future(async move {
            // See `OpStreamServer::read` for why this borrows per-poll
            // rather than holding the `RefCell` guard across the await.
            let Some(env) = std::future::poll_fn(|cx| rx.borrow_mut().poll_recv(cx)).await else {
                return Err(capnp::Error::disconnected(
                    "context stream fanout closed".into(),
                ));
            };
            let mut ctx = results.get().init_ctx();
            {
                // capnp list lengths/indices are u32; these lists
                // realistically never approach u32::MAX, but clamp (and
                // cap the fill loop to match) rather than let a
                // truncating cast desync the list from what gets written.
                let n = u32::try_from(env.collaborators.len()).unwrap_or(u32::MAX);
                let mut collabs = ctx.reborrow().init_collaborators(n);
                for (i, c) in env.collaborators.iter().enumerate().take(n as usize) {
                    let Ok(i) = u32::try_from(i) else { break };
                    let mut slot = collabs.reborrow().get(i);
                    slot.set_user_id(&c.id);
                    slot.set_display_name(&c.name);
                    slot.set_color(pack_color(c.color));
                }
            }
            {
                let n = u32::try_from(env.permissions.len()).unwrap_or(u32::MAX);
                let mut perms = ctx.init_permissions(n);
                for (i, p) in env.permissions.iter().enumerate().take(n as usize) {
                    let Ok(i) = u32::try_from(i) else { break };
                    perms.set(i, p.as_str());
                }
            }
            Ok(())
        })
    }
}

/// Convert `[f32; 3]` RGB in 0..=1 to the wire's `0x00RRGGBB`.
fn pack_color(rgb: [f32; 3]) -> u32 {
    // Clamped to [0.0, 1.0] before scaling to [0.0, 255.0] and rounding,
    // so this cast is always in u32's range.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to [0.0, 255.0] above"
    )]
    let clamp = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u32;
    let r = clamp(rgb[0]);
    let g = clamp(rgb[1]);
    let b = clamp(rgb[2]);
    (r << 16) | (g << 8) | b
}

// Listener wiring

/// Bind `socket_path` and spawn the capnp accept loop. Attaches the
/// created [`CapnpBrokerState`] to `peer_table` so every event flowing
/// through the legacy fanout also lands on capnp subscribers.
///
/// `publisher` is the other direction — attaching to the peer table lets
/// a capnp app *receive*, `publisher` lets it *send*, same instance
/// [`crate::shell::collab_broker`] hands the JSON dialect.
///
/// The accept thread runs a `current_thread` tokio runtime + `LocalSet`
/// (capnp-rpc futures are `!Send`), polling `quit` on a 100 ms cadence.
///
/// # Errors
///
/// Returns the underlying I/O error if `socket_path` can't be bound
/// (e.g. already in use, or the parent directory doesn't exist).
pub fn spawn(
    socket_path: PathBuf,
    peer_table: Arc<PeerTable>,
    broker_channels: Arc<Mutex<BrokerChannels>>,
    publisher: Arc<PeerPublisher>,
    identity: Arc<LocalIdentity>,
    quit: Arc<AtomicBool>,
) -> std::io::Result<JoinHandle<()>> {
    let _ = std::fs::remove_file(&socket_path);
    let wired = publisher.is_wired();
    let state = CapnpBrokerState::new(&peer_table, broker_channels, publisher, identity);
    peer_table.attach_capnp(Arc::clone(&state));

    // Bind on the caller's thread: from the worker thread, a bind
    // failure surfaces only via a log line.
    let std_listener = std::os::unix::net::UnixListener::bind(&socket_path)?;
    std_listener.set_nonblocking(true)?; // tokio's reactor requires this for `from_std`.
    tracing::info!(
        peer_sync = wired,
        "capnp broker: bound {} for native SDK access ({})",
        socket_path.display(),
        if wired {
            "edits from apps on this socket will reach the room"
        } else {
            "LOCAL ONLY — no peer network, so edits from apps on this \
             socket stay on this machine"
        },
    );

    let thread = std::thread::Builder::new()
        .name("qos-collab-broker-capnp".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::warn!("capnp broker: tokio runtime init failed: {e}");
                    return;
                }
            };
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                let listener = match tokio::net::UnixListener::from_std(std_listener) {
                    Ok(l) => l,
                    Err(e) => {
                        tracing::warn!(
                            "capnp broker: adopting {} into the reactor failed: {e}",
                            socket_path.display()
                        );
                        return;
                    }
                };
                loop {
                    if quit.load(Ordering::Relaxed) {
                        break;
                    }
                    let stream = tokio::select! {
                        biased;
                        () = tokio::time::sleep(Duration::from_millis(100)) => continue,
                        conn = listener.accept() => conn,
                    };
                    let (stream, _) = match stream {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!("capnp broker: accept failed: {e}; retrying");
                            continue;
                        }
                    };
                    let state = Arc::clone(&state);
                    let peer_id = state.next_peer_id();
                    tokio::task::spawn_local(async move {
                        if let Err(e) = drive_connection(stream, state, peer_id).await {
                            tracing::warn!("capnp broker: peer-{peer_id} disconnected: {e}");
                        }
                    });
                }
            });
        })?;

    Ok(thread)
}

/// Dial the capnp socket at `path` for tests. Production apps go through
/// `runtime_collab::try_connect` (launcher-passed fd). Public so
/// out-of-tree integration tests can reach it too.
///
/// # Errors
///
/// Returns an I/O error if the socket still hasn't appeared after
/// retrying with backoff for ~2s, or if connecting to it fails outright.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc's object model is Rc/RefCell-based and its futures are \
              inherently !Send; this always runs on the connection's own LocalSet, \
              same as the production accept loop in spawn()"
)]
pub async fn dial_capnp_for_test(
    path: PathBuf,
) -> Result<
    (
        runtime_collab::raw::collab_service::Client,
        tokio::task::JoinHandle<()>,
    ),
    std::io::Error,
> {
    // Retry-with-backoff ~2s: the accept loop may not have bound yet.
    let stream = {
        let mut last_err: Option<std::io::Error> = None;
        let mut stream = None;
        for _ in 0..50 {
            match tokio::net::UnixStream::connect(&path).await {
                Ok(s) => {
                    stream = Some(s);
                    break;
                }
                Err(e) => {
                    last_err = Some(e);
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
        let Some(stream) = stream else {
            return Err(last_err.unwrap_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "capnp socket did not appear within 2s",
                )
            }));
        };
        stream
    };
    let (r, w) = stream.into_split();
    let network = Box::new(twoparty::VatNetwork::new(
        futures::io::BufReader::new(r.compat()),
        futures::io::BufWriter::new(w.compat_write()),
        rpc_twoparty_capnp::Side::Client,
        capnp::message::ReaderOptions::default(),
    ));
    let mut rpc = RpcSystem::new(network, None);
    let client: runtime_collab::raw::collab_service::Client =
        rpc.bootstrap(rpc_twoparty_capnp::Side::Server);
    let driver = tokio::task::spawn_local(async move {
        let _ = rpc.await;
    });
    Ok((client, driver))
}

async fn drive_connection(
    stream: tokio::net::UnixStream,
    state: Arc<CapnpBrokerState>,
    peer_id: u64,
) -> Result<(), capnp::Error> {
    let (reader, writer) = stream.into_split();
    let reader = reader.compat();
    let writer = writer.compat_write();
    let network = Box::new(twoparty::VatNetwork::new(
        futures::io::BufReader::new(reader),
        futures::io::BufWriter::new(writer),
        rpc_twoparty_capnp::Side::Server,
        capnp::message::ReaderOptions::default(),
    ));
    let service = CapnpCollabService { state, peer_id };
    let client: collab_service::Client = capnp_rpc::new_client(service);
    let rpc = RpcSystem::new(network, Some(client.client));
    rpc.await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream as StdUnixStream;
    use std::time::{Duration, Instant, SystemTime};

    use collab_types::{
        ArtifactKind, Attestation, AttestationId, ChannelId, ChannelState, Ed25519PublicKey, Role,
        RoleAttestation, Signature, generate_keypair, sign_event, sign_op,
    };
    use ed25519_dalek::SigningKey;
    use ui::collaboration::CollabRequest;

    use crate::shell::collab_broker::{
        BrokerHandle, spawn_full, spawn_native, spawn_with_publisher,
    };
    use crate::shell::collab_outbound::recording::{RecordingSink, Sent};

    fn unique_paths(label: &str) -> (PathBuf, PathBuf) {
        let pid = std::process::id();
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let json = std::env::temp_dir().join(format!("qos-broker-{label}-{pid}-{nanos}.json.sock"));
        let capnp =
            std::env::temp_dir().join(format!("qos-broker-{label}-{pid}-{nanos}.capnp.sock"));
        (json, capnp)
    }

    fn wait_for_json_broker(socket: &PathBuf) -> StdUnixStream {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match StdUnixStream::connect(socket) {
                Ok(s) => return s,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("could not connect to json broker: {e}"),
            }
        }
    }

    fn write_json_frame<T: serde::Serialize>(stream: &mut StdUnixStream, v: &T) {
        let bytes = serde_json::to_vec(v).expect("serialize");
        let len = (bytes.len() as u32).to_le_bytes();
        stream.write_all(&len).expect("write len");
        stream.write_all(&bytes).expect("write body");
    }

    fn read_json_event(
        stream: &mut StdUnixStream,
        timeout: Duration,
    ) -> Option<ui::collaboration::CollabEvent> {
        stream.set_read_timeout(Some(timeout)).expect("set timeout");
        let mut len_buf = [0u8; 4];
        stream.read_exact(&mut len_buf).ok()?;
        let len = u32::from_le_bytes(len_buf) as usize;
        let mut body = vec![0u8; len];
        stream.read_exact(&mut body).ok()?;
        serde_json::from_slice(&body).ok()
    }

    /// A `ChannelState` authorising one Editor; returns the artifact +
    /// editor keys + attestation id.
    fn setup_role_table(
        handle: &BrokerHandle,
    ) -> (ArtifactId, SigningKey, Ed25519PublicKey, AttestationId) {
        let (owner_sk, owner_pk) = generate_keypair();
        let (editor_sk, editor_pk) = generate_keypair();
        let channel = ChannelId::new();
        let artifact = ArtifactId::new();

        let now = SystemTime::now();
        let mut att = Attestation {
            id: AttestationId::new(),
            issued_by: owner_pk,
            subject: editor_pk,
            claims: vec![],
            valid_from: now - Duration::from_secs(365 * 24 * 3600),
            valid_until: now + Duration::from_secs(10 * 365 * 24 * 3600),
            signature: Signature::from_bytes([0u8; 64]),
        };
        let sb = att.signing_bytes().expect("attestation signing_bytes");
        att.signature = sign_event(&sb, &owner_sk).expect("sign attestation");
        let ra = RoleAttestation {
            channel,
            role: Role::Editor,
            attestation: att.clone(),
        };
        let editor_att_id = att.id;

        let mut state = ChannelState::default();
        state.authorised_signers.push(owner_pk);
        state.artifacts.insert(artifact, ArtifactKind::Note);
        state.register_attestation(ra);
        handle.register_channel_state(artifact, state);

        (artifact, editor_sk, editor_pk, editor_att_id)
    }

    fn build_signed_op(
        author_sk: &SigningKey,
        author_pk: Ed25519PublicKey,
        artifact: ArtifactId,
        attestation_id: AttestationId,
        seq: u64,
        payload: &[u8],
    ) -> SignedOp {
        let mut op = SignedOp::new_unsigned(
            payload.to_vec(),
            artifact,
            author_pk,
            attestation_id,
            OpClass::Splice,
            SystemTime::now(),
            seq,
        );
        sign_op(&mut op, author_sk).expect("sign op");
        op
    }

    #[test]
    fn legacy_signed_op_reaches_capnp_subscriber() {
        let (json_path, capnp_path) = unique_paths("legacy2capnp");
        let handle =
            spawn_full(json_path.clone(), Some(capnp_path.clone()), None).expect("spawn broker");
        let (artifact, editor_sk, editor_pk, editor_att_id) = setup_role_table(&handle);

        let cluster = uuid::Uuid::new_v4();
        let channel_id = cluster.to_string();
        let mut legacy = wait_for_json_broker(&json_path);
        write_json_frame(
            &mut legacy,
            &CollabRequest::Hello {
                app_id: "legacy-editor".into(),
                cluster_id: cluster,
                record: None,
                handle: None,
            },
        );

        let (op_tx, op_rx) = std::sync::mpsc::channel::<(Vec<u8>, String)>();
        let capnp_path_clone = capnp_path.clone();
        let channel_id = channel_id.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build runtime");
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                let (client, _driver) = dial_capnp_for_test(capnp_path_clone)
                    .await
                    .expect("dial capnp");
                let sdk = runtime_collab::CollabClient::from_raw(client);
                let channel = sdk
                    .join_channel(&channel_id, b"identity-A")
                    .await
                    .expect("join channel");
                let stream = channel.subscribe().await.expect("subscribe");
                loop {
                    match stream.read().await {
                        Ok(ev) => {
                            if op_tx.send((ev.op, ev.from_user_id)).is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            });
        });

        // No public accessor for capnp subscriber count; use JSON
        // `peer_count` as a proxy plus a grace sleep.
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let has_sub = { handle.peer_count() >= 1 };
            if has_sub {
                break;
            }
            if Instant::now() > deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(200));

        let op = build_signed_op(
            &editor_sk,
            editor_pk,
            artifact,
            editor_att_id,
            1,
            b"hi-capnp",
        );
        write_json_frame(
            &mut legacy,
            &CollabRequest::SignedSyncIncremental {
                envelope: Box::new(op.clone()),
            },
        );

        let (op_bytes, from) = op_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("capnp subscriber should receive the op");
        let received: SignedOp =
            postcard::from_bytes(&op_bytes).expect("received bytes are a SignedOp postcard");
        assert_eq!(received.artifact, artifact);
        assert_eq!(received.seq, 1);
        assert_eq!(received.payload, b"hi-capnp");
        assert!(
            !from.starts_with("peer-") && !from.starts_with("peer@"),
            "a transport id reached a subscriber: {from}",
        );
        assert_eq!(from, crate::shell::state::local_user_id());

        drop(legacy);
        drop(handle);
    }

    #[test]
    fn capnp_signed_op_reaches_legacy_subscriber() {
        let (json_path, capnp_path) = unique_paths("capnp2legacy");
        let handle =
            spawn_full(json_path.clone(), Some(capnp_path.clone()), None).expect("spawn broker");
        let (artifact, editor_sk, editor_pk, editor_att_id) = setup_role_table(&handle);

        let cluster = uuid::Uuid::new_v4();
        let channel_id = cluster.to_string();
        let mut legacy = wait_for_json_broker(&json_path);
        write_json_frame(
            &mut legacy,
            &CollabRequest::Hello {
                app_id: "legacy-observer".into(),
                cluster_id: cluster,
                record: None,
                handle: None,
            },
        );

        let op = build_signed_op(
            &editor_sk,
            editor_pk,
            artifact,
            editor_att_id,
            1,
            b"hi-legacy",
        );
        let op_bytes = postcard::to_stdvec(&op).expect("postcard signed op");
        let capnp_path_clone = capnp_path.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build runtime");
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                let (client, _driver) = dial_capnp_for_test(capnp_path_clone)
                    .await
                    .expect("dial capnp");
                let sdk = runtime_collab::CollabClient::from_raw(client);
                let channel = sdk
                    .join_channel(&channel_id, b"identity-B")
                    .await
                    .expect("join channel");
                channel.send(&op_bytes).await.expect("send signed op");
                tokio::time::sleep(Duration::from_millis(500)).await;
            });
        });

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut seen = false;
        while Instant::now() < deadline {
            match read_json_event(&mut legacy, Duration::from_millis(200)) {
                Some(ui::collaboration::CollabEvent::SignedCRDTIncremental {
                    envelope, ..
                }) => {
                    assert_eq!(envelope.artifact, artifact);
                    assert_eq!(envelope.seq, 1);
                    assert_eq!(envelope.payload, b"hi-legacy");
                    seen = true;
                    break;
                }
                Some(_) => continue,
                None => continue,
            }
        }
        assert!(seen, "legacy peer should have received the signed op");
        drop(legacy);
        drop(handle);
    }

    #[test]
    fn capnp_cursor_publish_reaches_legacy_and_capnp() {
        let (json_path, capnp_path) = unique_paths("cursor-fan");
        let handle =
            spawn_full(json_path.clone(), Some(capnp_path.clone()), None).expect("spawn broker");

        let cluster = uuid::Uuid::new_v4();
        let channel_id = cluster.to_string();
        let mut legacy = wait_for_json_broker(&json_path);
        write_json_frame(
            &mut legacy,
            &CollabRequest::Hello {
                app_id: "legacy-cursor-observer".into(),
                cluster_id: cluster,
                record: None,
                handle: None,
            },
        );

        let (presence_tx, presence_rx) = std::sync::mpsc::channel::<(String, Vec<u8>, f32, f32)>();
        let capnp_path_sub = capnp_path.clone();
        let channel_id_sub = channel_id.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                let (client, _driver) = dial_capnp_for_test(capnp_path_sub)
                    .await
                    .expect("dial capnp (sub)");
                let sdk = runtime_collab::CollabClient::from_raw(client);
                let channel = sdk
                    .join_channel(&channel_id_sub, b"identity-sub")
                    .await
                    .expect("join channel");
                let ph = channel.presence().await.expect("presence");
                let ps = ph.subscribe_all().await.expect("subscribe_all");
                while let Ok(evt) = ps.read().await {
                    let (x, y) = match evt.payload {
                        runtime_collab::PresencePayload::Cursor { x, y } => (x, y),
                        _ => continue,
                    };
                    if presence_tx
                        .send((evt.user_id, evt.key_bytes, x, y))
                        .is_err()
                    {
                        break;
                    }
                }
            });
        });

        std::thread::sleep(Duration::from_millis(400));

        let capnp_path_pub = capnp_path.clone();
        let channel_id_pub = channel_id.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                let (client, _driver) = dial_capnp_for_test(capnp_path_pub)
                    .await
                    .expect("dial capnp (pub)");
                let sdk = runtime_collab::CollabClient::from_raw(client);
                let channel = sdk
                    .join_channel(&channel_id_pub, b"identity-pub")
                    .await
                    .expect("join channel");
                let ph = channel.presence().await.expect("presence");
                ph.publish_cursor(11.0, 22.0).await.expect("publish cursor");
                tokio::time::sleep(Duration::from_millis(500)).await;
            });
        });

        let deadline = Instant::now() + Duration::from_secs(3);
        let mut legacy_seen = false;
        while Instant::now() < deadline {
            match read_json_event(&mut legacy, Duration::from_millis(200)) {
                Some(ui::collaboration::CollabEvent::RemoteCursorMoved { x, y, .. }) => {
                    assert!((x - 11.0).abs() < 0.001 && (y - 22.0).abs() < 0.001);
                    legacy_seen = true;
                    break;
                }
                Some(_) => continue,
                None => continue,
            }
        }
        assert!(
            legacy_seen,
            "legacy peer should have seen RemoteCursorMoved"
        );

        let (_user, key_bytes, x, y) = presence_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("capnp presence sub should receive cursor");
        assert!((x - 11.0).abs() < 0.001 && (y - 22.0).abs() < 0.001);
        let key: PresenceKey =
            postcard::from_bytes(&key_bytes).expect("key_bytes decode as PresenceKey");
        assert_eq!(
            key,
            PresenceKey::pointer_default(),
            "capnp cursor fanout must carry canonical pointer_default key bytes"
        );

        drop(legacy);
        drop(handle);
    }

    #[test]
    fn capnp_fetch_scrollback_for_reads_legacy_written_ops() {
        let (json_path, capnp_path) = unique_paths("scrollback");
        let handle =
            spawn_full(json_path.clone(), Some(capnp_path.clone()), None).expect("spawn broker");
        let (artifact, editor_sk, editor_pk, editor_att_id) = setup_role_table(&handle);

        let mut legacy = wait_for_json_broker(&json_path);
        write_json_frame(
            &mut legacy,
            &CollabRequest::Hello {
                app_id: "writer".into(),
                cluster_id: uuid::Uuid::new_v4(),
                record: None,
                handle: None,
            },
        );
        for seq in 1..=3 {
            let op = build_signed_op(
                &editor_sk,
                editor_pk,
                artifact,
                editor_att_id,
                seq,
                format!("op-{seq}").as_bytes(),
            );
            write_json_frame(
                &mut legacy,
                &CollabRequest::SignedSyncIncremental {
                    envelope: Box::new(op),
                },
            );
        }
        std::thread::sleep(Duration::from_millis(200));

        let artifact_bytes = artifact.0.as_bytes().to_vec();
        let capnp_path_clone = capnp_path.clone();
        let (batch_tx, batch_rx) = std::sync::mpsc::channel::<runtime_collab::ScrollbackBatch>();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                let (client, _driver) = dial_capnp_for_test(capnp_path_clone)
                    .await
                    .expect("dial capnp");
                let sdk = runtime_collab::CollabClient::from_raw(client);
                let channel = sdk.join_channel("sb-ch", b"identity").await.expect("join");
                let batch = channel
                    .fetch_scrollback_for(&artifact_bytes, u64::MAX, 100)
                    .await
                    .expect("fetch_scrollback_for");
                let _ = batch_tx.send(batch);
            });
        });

        let batch = batch_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("scrollback reply");
        assert_eq!(batch.ops.len(), 3, "should read all three ops");
        for (i, op_bytes) in batch.ops.iter().enumerate() {
            let decoded: SignedOp =
                postcard::from_bytes(op_bytes).expect("scrollback op is a valid postcard");
            assert_eq!(decoded.artifact, artifact);
            assert_eq!(decoded.seq, (i + 1) as u64);
        }

        drop(legacy);
        drop(handle);
    }

    // Outbound: these drive the real capnp server over a real socket and
    // read back what the broker handed the network — local fanout alone
    // would pass even with no publisher wired.

    fn with_capnp_client<F>(capnp_path: PathBuf, body: F)
    where
        F: FnOnce(
                runtime_collab::CollabClient,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()>>>
            + Send
            + 'static,
    {
        let done = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build runtime");
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                let (client, _driver) = dial_capnp_for_test(capnp_path).await.expect("dial capnp");
                body(runtime_collab::CollabClient::from_raw(client)).await;
            });
        });
        done.join().expect("capnp client thread");
    }

    fn wait_for_publish(sink: &RecordingSink, within: Duration) -> Vec<Sent> {
        let deadline = Instant::now() + within;
        loop {
            let seen = sink.snapshot();
            if !seen.is_empty() || Instant::now() > deadline {
                return seen;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn capnp_unsigned_incremental_reaches_the_peer_network() {
        let (json_path, capnp_path) = unique_paths("capnp-outbound");
        let sink = Arc::new(RecordingSink::default());
        let publisher = crate::shell::collab_outbound::PeerPublisher::new(
            sink.clone() as Arc<dyn crate::shell::collab_outbound::PeerPublishSink>
        );
        let handle = spawn_with_publisher(json_path, Some(capnp_path.clone()), publisher)
            .expect("spawn broker");

        let cluster = uuid::Uuid::new_v4().to_string();
        let channel_id = cluster.clone();
        with_capnp_client(capnp_path, move |sdk| {
            Box::pin(async move {
                let channel = sdk
                    .join_channel(&channel_id, b"identity-A")
                    .await
                    .expect("join channel");
                channel
                    .send_unsigned_incremental(b"typed-in-the-sandbox")
                    .await
                    .expect("send unsigned incremental");
            })
        });

        assert_eq!(
            wait_for_publish(&sink, Duration::from_secs(3)),
            vec![Sent::Incremental {
                app_id: String::new(),
                cluster_id: cluster,
                record_id: String::new(),
                data: b"typed-in-the-sandbox".to_vec(),
            }],
            "an edit made over the capnp transport must be published to the \
             room; a broker that only fans out locally leaves a sandboxed \
             app collaborating with itself",
        );
        drop(handle);
    }

    #[test]
    fn capnp_record_channel_keeps_its_record_on_the_wire() {
        // If the record were dropped on the way out, a peer editing a
        // different file in the same room would get this file's deltas.
        let (json_path, capnp_path) = unique_paths("capnp-outbound-record");
        let sink = Arc::new(RecordingSink::default());
        let publisher = crate::shell::collab_outbound::PeerPublisher::new(
            sink.clone() as Arc<dyn crate::shell::collab_outbound::PeerPublishSink>
        );
        let handle = spawn_with_publisher(json_path, Some(capnp_path.clone()), publisher)
            .expect("spawn broker");

        let cluster = uuid::Uuid::new_v4().to_string();
        let channel_id = format!("{cluster}#record:report.md");
        with_capnp_client(capnp_path, move |sdk| {
            Box::pin(async move {
                let channel = sdk
                    .join_channel(&channel_id, b"identity-A")
                    .await
                    .expect("join channel");
                channel
                    .send_unsigned_incremental(b"delta")
                    .await
                    .expect("send unsigned incremental");
            })
        });

        assert_eq!(
            wait_for_publish(&sink, Duration::from_secs(3)),
            vec![Sent::Incremental {
                app_id: String::new(),
                cluster_id: cluster,
                record_id: "report.md".to_string(),
                data: b"delta".to_vec(),
            }],
        );
        drop(handle);
    }

    #[test]
    fn capnp_signed_op_reaches_the_peer_network() {
        let (json_path, capnp_path) = unique_paths("capnp-outbound-signed");
        let sink = Arc::new(RecordingSink::default());
        let publisher = crate::shell::collab_outbound::PeerPublisher::new(
            sink.clone() as Arc<dyn crate::shell::collab_outbound::PeerPublishSink>
        );
        let handle = spawn_with_publisher(json_path, Some(capnp_path.clone()), publisher)
            .expect("spawn broker");
        let (artifact, editor_sk, editor_pk, editor_att_id) = setup_role_table(&handle);
        let op = build_signed_op(
            &editor_sk,
            editor_pk,
            artifact,
            editor_att_id,
            1,
            b"signed-in-the-sandbox",
        );
        let op_bytes = postcard::to_stdvec(&op).expect("encode op");

        let cluster = uuid::Uuid::new_v4().to_string();
        let channel_id = cluster.clone();
        with_capnp_client(capnp_path, move |sdk| {
            Box::pin(async move {
                let channel = sdk
                    .join_channel(&channel_id, b"identity-A")
                    .await
                    .expect("join channel");
                channel.send(&op_bytes).await.expect("send signed op");
            })
        });

        assert_eq!(
            wait_for_publish(&sink, Duration::from_secs(3)),
            vec![Sent::Signed {
                app_id: String::new(),
                cluster_id: cluster,
                record_id: String::new(),
                payload: b"signed-in-the-sandbox".to_vec(),
            }],
            "a validated signed op must reach the room too; the inbound \
             half has always known how to receive one",
        );
        drop(handle);
    }

    #[test]
    fn a_channel_that_names_no_room_publishes_nothing() {
        // Fail closed, same as local fanout: the nil uuid is what a
        // workspace announces before its room identity is minted.
        let (json_path, capnp_path) = unique_paths("capnp-outbound-unrouted");
        let sink = Arc::new(RecordingSink::default());
        let publisher = crate::shell::collab_outbound::PeerPublisher::new(
            sink.clone() as Arc<dyn crate::shell::collab_outbound::PeerPublishSink>
        );
        let handle = spawn_with_publisher(json_path, Some(capnp_path.clone()), publisher)
            .expect("spawn broker");

        with_capnp_client(capnp_path, move |sdk| {
            Box::pin(async move {
                for channel_id in ["", "00000000-0000-0000-0000-000000000000"] {
                    let channel = sdk
                        .join_channel(channel_id, b"identity-A")
                        .await
                        .expect("join channel");
                    channel
                        .send_unsigned_incremental(b"nowhere")
                        .await
                        .expect("send unsigned incremental");
                }
            })
        });

        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(
            sink.snapshot(),
            Vec::new(),
            "an unaddressable channel must not put frames on the wire",
        );
        drop(handle);
    }

    // The same question, on the real network handle: stand up
    // `shell_network_for_test` and assert on the `NeighbourMsg`.

    #[test]
    fn a_capnp_apps_whole_document_is_published_to_the_room() {
        let (json_path, capnp_path) = unique_paths("capnp-snapshot-outbound");
        let tree = crate::shell::state::WorkspaceTree::bootstrap_demo().shared();
        let (net, mut cmd_rx) = crate::shell::network::shell_network_for_test(tree, 0u64);
        let net = Arc::new(net);
        let net_for_arrival = Arc::clone(&net);
        let handle = spawn_full(json_path.clone(), Some(capnp_path.clone()), Some(net))
            .expect("spawn broker");

        let cluster = uuid::Uuid::new_v4();
        let channel_id =
            CrdtRoute::record("dev.qos.words", cluster.to_string(), "/notes/a.md").channel_key();

        net_for_arrival.note_snapshot_request(&cluster.to_string());

        let capnp_path_clone = capnp_path.clone();
        let joined = channel_id.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                let (client, _driver) = dial_capnp_for_test(capnp_path_clone)
                    .await
                    .expect("dial capnp");
                let sdk = runtime_collab::CollabClient::from_raw(client);
                let channel = sdk.join_channel(&joined, b"identity").await.expect("join");
                channel
                    .send_unsigned_snapshot(b"a-whole-document")
                    .await
                    .expect("send unsigned snapshot");
                tokio::time::sleep(Duration::from_millis(500)).await;
            });
        });

        let deadline = Instant::now() + Duration::from_secs(5);
        let published = loop {
            match cmd_rx.try_recv() {
                Ok(crate::network::actor::NetCmd::Publish { msg, .. }) => break Some(msg),
                Ok(_) => continue,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(_) => break None,
            }
        };

        match published {
            Some(crate::network::protocol::NeighbourMsg::AppCrdtSnapshot {
                app_id,
                cluster_id,
                record_id,
                data,
                ..
            }) => {
                assert_eq!(app_id, "dev.qos.words");
                assert_eq!(cluster_id, cluster.to_string());
                assert_eq!(record_id, "/notes/a.md");
                assert_eq!(data, b"a-whole-document");
            }
            Some(other) => panic!("expected AppCrdtSnapshot, got {other:?}"),
            None => panic!("a capnp snapshot must be published to the room"),
        }

        drop(handle);
    }

    #[test]
    fn a_capnp_apps_delta_is_published_to_the_room() {
        let (json_path, capnp_path) = unique_paths("capnp-outbound");
        let tree = crate::shell::state::WorkspaceTree::bootstrap_demo().shared();
        let (net, mut cmd_rx) = crate::shell::network::shell_network_for_test(tree, 0u64);
        let handle = spawn_full(
            json_path.clone(),
            Some(capnp_path.clone()),
            Some(Arc::new(net)),
        )
        .expect("spawn broker");

        let cluster = uuid::Uuid::new_v4();
        let channel_id =
            CrdtRoute::record("dev.qos.words", cluster.to_string(), "/notes/a.md").channel_key();

        let capnp_path_clone = capnp_path.clone();
        let joined = channel_id.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                let (client, _driver) = dial_capnp_for_test(capnp_path_clone)
                    .await
                    .expect("dial capnp");
                let sdk = runtime_collab::CollabClient::from_raw(client);
                let channel = sdk.join_channel(&joined, b"identity").await.expect("join");
                channel
                    .send_unsigned_incremental(b"a-change-blob")
                    .await
                    .expect("send unsigned incremental");
                tokio::time::sleep(Duration::from_millis(500)).await;
            });
        });

        let deadline = Instant::now() + Duration::from_secs(5);
        let published = loop {
            match cmd_rx.try_recv() {
                Ok(crate::network::actor::NetCmd::Publish { msg, .. }) => break Some(msg),
                Ok(_) => continue,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(_) => break None,
            }
        };

        match published {
            Some(crate::network::protocol::NeighbourMsg::AppCrdtIncremental {
                app_id,
                cluster_id,
                record_id,
                data,
                ..
            }) => {
                assert_eq!(
                    data,
                    b"a-change-blob".to_vec(),
                    "the payload crosses verbatim"
                );
                assert_eq!(
                    cluster_id,
                    cluster.to_string(),
                    "the room is what the far side gates admission on; \
                     a delta published without it is refused there",
                );
                assert_eq!(
                    app_id, "dev.qos.words",
                    "an empty app id means cluster-scoped, which would hand \
                     one app's document to every app in the room",
                );
                assert_eq!(
                    record_id, "/notes/a.md",
                    "the record is which document this change belongs to",
                );
            }
            Some(other) => panic!("expected AppCrdtIncremental, got {other:?}"),
            None => panic!(
                "a capnp app sent a document delta and nothing was published; \
                 the outbound half of the bridge is not connected",
            ),
        }

        drop(handle);
    }

    #[test]
    fn a_channel_naming_no_room_publishes_nothing() {
        let (json_path, capnp_path) = unique_paths("capnp-unaddressed");
        let tree = crate::shell::state::WorkspaceTree::bootstrap_demo().shared();
        let (net, mut cmd_rx) = crate::shell::network::shell_network_for_test(tree, 0u64);
        let handle = spawn_full(
            json_path.clone(),
            Some(capnp_path.clone()),
            Some(Arc::new(net)),
        )
        .expect("spawn broker");

        let channel_id = CrdtRoute::record(
            "dev.qos.words",
            uuid::Uuid::nil().to_string(),
            "/notes/a.md",
        )
        .channel_key();

        let capnp_path_clone = capnp_path.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                let (client, _driver) = dial_capnp_for_test(capnp_path_clone)
                    .await
                    .expect("dial capnp");
                let sdk = runtime_collab::CollabClient::from_raw(client);
                let channel = sdk
                    .join_channel(&channel_id, b"identity")
                    .await
                    .expect("join");
                channel
                    .send_unsigned_incremental(b"a-change-blob")
                    .await
                    .expect("send unsigned incremental");
                tokio::time::sleep(Duration::from_millis(500)).await;
            });
        });

        std::thread::sleep(Duration::from_millis(1200));
        assert!(
            cmd_rx.try_recv().is_err(),
            "a delta whose channel names no room must not reach the network",
        );
        drop(handle);
    }
    /// Three kinds of frame share the op stream; a reader can only tell
    /// them apart by the `kind` tag, not the bytes alone.
    #[test]
    fn an_unsigned_frame_reaches_a_subscriber_tagged_with_its_kind() {
        let (json_path, capnp_path) = unique_paths("capnp-unsigned-kind");
        let handle = spawn_native(capnp_path.clone(), None).expect("spawn typed broker");
        assert!(
            !json_path.exists(),
            "the production typed service must not create a legacy socket"
        );

        let channel_id = CrdtRoute::record(
            "dev.qos.words",
            uuid::Uuid::new_v4().to_string(),
            "/notes/a.md",
        )
        .channel_key();

        let (tx, rx) = std::sync::mpsc::channel();
        let capnp_path_clone = capnp_path.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                // The reader is a second connection — a client talking
                // to itself would prove nothing.
                let (rc, _rd) = dial_capnp_for_test(capnp_path_clone.clone())
                    .await
                    .expect("dial reader");
                let reader = runtime_collab::CollabClient::from_raw(rc);
                let rch = reader
                    .join_channel(&channel_id, b"reader")
                    .await
                    .expect("join reader");
                let sub = rch.subscribe().await.expect("subscribe");

                let (wc, _wd) = dial_capnp_for_test(capnp_path_clone)
                    .await
                    .expect("dial writer");
                let writer = runtime_collab::CollabClient::from_raw(wc);
                let wch = writer
                    .join_channel(&channel_id, b"writer")
                    .await
                    .expect("join writer");

                wch.send_unsigned_incremental(b"a-change-blob")
                    .await
                    .expect("send delta");
                let delta = sub.read().await.expect("read delta");
                wch.send_unsigned_snapshot(b"a-whole-document")
                    .await
                    .expect("send snapshot");
                let snapshot = sub.read().await.expect("read snapshot");
                let _ = tx.send((delta, snapshot));
            });
        });

        let (delta, snapshot) = rx
            .recv_timeout(Duration::from_secs(20))
            .expect("the subscriber received both frames");

        assert_eq!(
            delta.kind,
            runtime_collab::OpKind::UnsignedIncremental,
            "a delta must arrive tagged as a delta; tagged as an envelope it is decoded \
             as one, fails to decode, and is dropped without a word",
        );
        assert_eq!(
            delta.op,
            b"a-change-blob".to_vec(),
            "the delta's bytes cross unchanged",
        );
        assert_eq!(
            snapshot.kind,
            runtime_collab::OpKind::UnsignedSnapshot,
            "a whole document must arrive tagged as one: it is merged into the reader's \
             document, and applying it as a delta does nothing at all",
        );
        assert_eq!(
            snapshot.op,
            b"a-whole-document".to_vec(),
            "the document's bytes cross unchanged",
        );

        drop(handle);
    }

    /// One room, two apps: a delta reaches the app it belongs to and not
    /// the app beside it (see [`CapnpBrokerState::fanout_to_route`]).
    /// Driven with three live connections; the third (bare cluster)
    /// proves the fanout isn't narrowed so far that nothing crosses.
    #[test]
    fn a_documents_delta_reaches_its_own_app_and_not_the_app_beside_it() {
        let (json_path, capnp_path) = unique_paths("capnp-app-scoped");
        let handle =
            spawn_full(json_path.clone(), Some(capnp_path.clone()), None).expect("spawn broker");

        let cluster = uuid::Uuid::new_v4().to_string();
        let words = CrdtRoute::app("dev.qos.word-processor", cluster.clone()).channel_key();
        let files = CrdtRoute::app("dev.qos.file-explorer", cluster.clone()).channel_key();
        let room = CrdtRoute::cluster(cluster.clone()).channel_key();

        let (tx, rx) = std::sync::mpsc::channel();
        let capnp_path_clone = capnp_path.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let local = tokio::task::LocalSet::new();
            local.block_on(&rt, async move {
                // Audience: same app, different app, room itself — each
                // its own connection.
                let (wc, _wd) = dial_capnp_for_test(capnp_path_clone.clone())
                    .await
                    .expect("dial word-processor reader");
                let words_reader = runtime_collab::CollabClient::from_raw(wc);
                let words_chan = words_reader
                    .join_channel(&words, b"words-reader")
                    .await
                    .expect("join word-processor");
                let words_sub = words_chan
                    .subscribe()
                    .await
                    .expect("subscribe word-processor");

                let (fc, _fd) = dial_capnp_for_test(capnp_path_clone.clone())
                    .await
                    .expect("dial file-explorer reader");
                let files_reader = runtime_collab::CollabClient::from_raw(fc);
                let files_chan = files_reader
                    .join_channel(&files, b"files-reader")
                    .await
                    .expect("join file-explorer");
                let files_sub = files_chan
                    .subscribe()
                    .await
                    .expect("subscribe file-explorer");

                let (rc, _rd) = dial_capnp_for_test(capnp_path_clone.clone())
                    .await
                    .expect("dial room reader");
                let room_reader = runtime_collab::CollabClient::from_raw(rc);
                let room_chan = room_reader
                    .join_channel(&room, b"room-reader")
                    .await
                    .expect("join room");
                let room_sub = room_chan.subscribe().await.expect("subscribe room");

                let (sc, _sd) = dial_capnp_for_test(capnp_path_clone)
                    .await
                    .expect("dial word-processor writer");
                let writer = runtime_collab::CollabClient::from_raw(sc);
                let write_chan = writer
                    .join_channel(&words, b"words-writer")
                    .await
                    .expect("join word-processor writer");
                write_chan
                    .send_unsigned_incremental(b"ada-typed-this")
                    .await
                    .expect("send the edit");

                let to_its_app = words_sub.read().await.map(|e| e.op).ok();
                let to_the_room = room_sub.read().await.map(|e| e.op).ok();
                // A neighbour delivery is already queued by the time
                // either read above returns; this wait is slack, not a race.
                let to_the_neighbour = matches!(
                    tokio::time::timeout(Duration::from_secs(2), files_sub.read()).await,
                    Ok(Ok(_)),
                );
                let _ = tx.send((to_its_app, to_the_room, to_the_neighbour));
            });
        });

        let (to_its_app, to_the_room, to_the_neighbour) = rx
            .recv_timeout(Duration::from_secs(30))
            .expect("the capnp thread reported");

        assert_eq!(
            to_its_app.as_deref(),
            Some(&b"ada-typed-this"[..]),
            "an edit must reach the other window of the app that made it — \
             this is what one shared document means",
        );
        assert!(
            !to_the_neighbour,
            "the word processor's document reached the file manager: two apps in one \
             room are not one document, and a reader asking either of them what it holds \
             is answered by whichever replies first",
        );
        assert_eq!(
            to_the_room.as_deref(),
            Some(&b"ada-typed-this"[..]),
            "a connection addressing the bare room still hears the room's traffic",
        );

        drop(handle);
    }
}
