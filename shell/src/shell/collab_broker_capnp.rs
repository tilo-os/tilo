// Silences `refining_impl_trait` warnings that surface at every capnp-rpc
// server-method impl — capnp-rpc's declared return type is
// `Promise<..., capnp::Error>` and using `Promise::from_future(async move {..})`
// refines its inner future type, which is the intended pattern. Same pin as
// the collab-bridge crate uses.
#![allow(refining_impl_trait)]

//! Native Cap'n Proto server for the qOS collaboration broker.
//!
//! The shell binds a second Unix socket alongside the legacy JSON one
//! and answers the `runtime-collab` schema on it. Both transports
//! share the same fanout state (see [`crate::shell::collab_broker`]):
//! a subscriber on either side observes every event that flows
//! through the broker regardless of which transport its author used.
//!
//! ## Wiring
//!
//! - [`spawn`] binds `socket_path` and returns the accept thread's
//!   join handle. The thread stands up a `current_thread` tokio
//!   runtime + `LocalSet` (capnp-rpc futures are `!Send`) and spawns
//!   one RPC system per accepted connection.
//! - [`CapnpBrokerState`] holds the per-connection subscriber
//!   registry. When the legacy [`PeerTable`] fans an event out, it
//!   calls into [`CapnpBrokerState::fanout_except`] which pushes the
//!   event onto every subscribed stream's tokio mpsc queue.
//! - Server structs ([`CapnpCollabService`], [`CapnpChannelHandle`],
//!   [`CapnpPresenceHandle`], the three per-stream servers) live for
//!   the lifetime of one RPC session and hold Rc'd handles into the
//!   shared broker state.
//!
//! ## Semantic policy
//!
//! - `join_channel(channel_id, identity)` uses the caller's
//!   `channel_id` verbatim as the channel routing key. No UUID
//!   derivation, no synthesis.
//! - `publishCursor(x, y)` fans out as a `Cursor` payload keyed on
//!   [`PresenceKey::pointer_default`] (postcard-encoded). Legacy
//!   subscribers see it as `CollabEvent::RemoteCursorMoved` (schema
//!   v1 didn't carry a key on that variant); capnp subscribers get
//!   the real keyBytes on `subscribeAll` streams.
//! - `sendUnsignedSnapshot` / `sendUnsignedIncremental` route through
//!   the same fanout as their legacy JSON counterparts. Callers that
//!   want role enforcement must use `send` with a signed op.
//! - `fetchScrollback` / `fetchScrollbackFor` answer directly from
//!   the broker's in-memory op log; cooperative-scrollback follow-ups
//!   still fan out via the drain thread and reach capnp subscribers
//!   on their op stream (unsigned `CRDTIncremental` fallback for a
//!   scrollback batch is intentionally NOT emitted — apps repeat the
//!   fetch if the first answer was exhausted-but-empty).

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
    channel_handle, collab_service, context_stream, cursor_stream, presence_event, presence_handle,
    presence_payload, presence_stream, scrollback_batch, stream,
};
use ui::collaboration::{CollabEvent, PresencePayload};
use ui::presence::PresenceKey;

use crate::shell::collab_broker::{
    AuditEntry, BrokerChannels, PeerTable, apply_redaction_from_envelope,
};

/// Envelope pushed onto an [`OpStreamServer`]'s queue.
#[derive(Clone, Debug)]
struct OpEnvelope {
    op_bytes: Vec<u8>,
    from_user_id: String,
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

/// One subscription onto the broker fanout. Slot-indexed inside a
/// [`ChannelSubscribers`] table so it can be dropped en masse when
/// the connection tears down.
struct OpSub {
    peer_id: u64,
    tx: mpsc::UnboundedSender<OpEnvelope>,
}
struct PresenceSub {
    peer_id: u64,
    tx: mpsc::UnboundedSender<PresenceEnvelope>,
}
struct CursorSub {
    peer_id: u64,
    tx: mpsc::UnboundedSender<CursorEnvelope>,
}
struct ContextSub {
    peer_id: u64,
    tx: mpsc::UnboundedSender<ContextEnvelope>,
}

/// Per-channel subscriber registry.
///
/// Every capnp connection that `join_channel(id)`s lands here under
/// its `id` string. The broker's legacy fanout paths (translate,
/// drain thread, `BrokerHandle::fanout_event`) enumerate every
/// channel's subscribers and push events onto the matching queues.
///
/// Global-fanout policy: every op / presence / context event is
/// pushed onto every channel's subscribers regardless of which
/// channel-id the client registered under. This mirrors the legacy
/// broker's global-broadcast semantic (the JSON broker has no
/// per-channel routing at the peer level). Apps that want per-channel
/// isolation filter on their own artifact id.
#[derive(Default)]
struct ChannelSubscribers {
    op: Vec<OpSub>,
    presence: Vec<PresenceSub>,
    cursor: Vec<CursorSub>,
    context: Vec<ContextSub>,
}

/// Shared capnp broker state. Attached to a [`PeerTable`] so the
/// legacy `fanout_except` / `send_to_peer` calls also reach capnp
/// subscribers.
pub struct CapnpBrokerState {
    /// Per-channel-id subscription rows. Key is the caller-supplied
    /// `channel_id` from `join_channel`. Rows are lazily inserted on
    /// first `join_channel(id)`.
    channels: Mutex<HashMap<String, ChannelSubscribers>>,
    /// Weak back-reference to the shared [`PeerTable`] so
    /// [`CapnpChannelHandle::send`] can drive the legacy fanout
    /// through the same code path a JSON peer would trigger. `Weak`
    /// so the broker teardown is not blocked by a capnp connection
    /// that outlives the handle.
    peer_table: Weak<PeerTable>,
    /// Shared per-artifact channel state + audit log. Same instance
    /// the legacy path validates against; a signed op that comes in
    /// via capnp mutates the exact same `ChannelState`.
    broker_channels: Arc<Mutex<BrokerChannels>>,
}

impl CapnpBrokerState {
    /// Public read-only handle to the underlying [`BrokerChannels`]
    /// so tests can register channel state before pushing signed
    /// envelopes through the capnp path.
    pub fn broker_channels(&self) -> Arc<Mutex<BrokerChannels>> {
        Arc::clone(&self.broker_channels)
    }

    fn new(peer_table: &Arc<PeerTable>, broker_channels: Arc<Mutex<BrokerChannels>>) -> Arc<Self> {
        Arc::new(Self {
            channels: Mutex::new(HashMap::new()),
            peer_table: Arc::downgrade(peer_table),
            broker_channels,
        })
    }

    /// Mint the next unified peer id. Delegates to the shared
    /// [`PeerTable`] so ids never collide with the legacy side.
    fn next_peer_id(&self) -> u64 {
        // If the peer table has been torn down we're racing with
        // shutdown; hand out a placeholder id (any capnp handle
        // holding it will drop on the next tokio yield).
        self.peer_table
            .upgrade()
            .map(|t| t.next_peer_id())
            .unwrap_or(u64::MAX)
    }

    /// Drive an event onto every subscribed queue whose peer id is
    /// not `except`. Called from the legacy `PeerTable::fanout_except`
    /// after it has finished writing to the JSON peers.
    pub fn fanout_except(&self, event: &CollabEvent, except: u64) {
        let Ok(mut channels) = self.channels.lock() else {
            return;
        };
        // Materialise each event into the transport-native envelope
        // once, then push a clone to every subscriber. Cheaper than
        // re-encoding per subscriber, and the envelopes are shallow
        // (Vec<u8> + strings) so `Clone` is a memcpy.
        match event {
            CollabEvent::SignedCRDTIncremental { envelope, from_user_id } => {
                let op_bytes = match postcard::to_stdvec(&**envelope) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(error = %e, "capnp fanout: postcard(SignedOp) failed");
                        return;
                    }
                };
                let env = OpEnvelope {
                    op_bytes,
                    from_user_id: from_user_id.clone(),
                };
                for subs in channels.values_mut() {
                    subs.op.retain(|s| {
                        if s.peer_id == except {
                            return true;
                        }
                        s.tx.send(env.clone()).is_ok()
                    });
                }
            }
            CollabEvent::CRDTIncremental { data } | CollabEvent::CRDTUpdate { data } => {
                // Legacy unsigned frames get an empty `from_user_id`
                // — the broker never learned who wrote them (the
                // JSON dialect stripped attribution on those two
                // variants long before this transport arrived).
                // Explicit empty string documented here so a reader
                // does not mistake it for a bug.
                let env = OpEnvelope {
                    op_bytes: data.clone(),
                    from_user_id: String::new(),
                };
                for subs in channels.values_mut() {
                    subs.op.retain(|s| {
                        if s.peer_id == except {
                            return true;
                        }
                        s.tx.send(env.clone()).is_ok()
                    });
                }
            }
            CollabEvent::RemoteCursorMoved { user_id, x, y } => {
                // Fan onto both the presence-all and the cursor-only
                // streams. On presence-all we emit real key bytes
                // corresponding to `PresenceKey::pointer_default()`
                // so downstream apps that route by key see a stable,
                // parseable tag (not the empty blob the bridge used
                // to fabricate).
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
                for subs in channels.values_mut() {
                    subs.presence.retain(|s| {
                        if s.peer_id == except {
                            return true;
                        }
                        s.tx.send(presence_env.clone()).is_ok()
                    });
                    subs.cursor.retain(|s| {
                        if s.peer_id == except {
                            return true;
                        }
                        s.tx.send(cursor_env.clone()).is_ok()
                    });
                }
            }
            CollabEvent::RemotePresenceUpdate { user_id, key, payload } => {
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
                for subs in channels.values_mut() {
                    subs.presence.retain(|s| {
                        if s.peer_id == except {
                            return true;
                        }
                        s.tx.send(env.clone()).is_ok()
                    });
                }
            }
            CollabEvent::Context { collaborators, permissions } => {
                let env = ContextEnvelope {
                    collaborators: collaborators.clone(),
                    permissions: permissions.clone(),
                };
                for subs in channels.values_mut() {
                    subs.context.retain(|s| {
                        if s.peer_id == except {
                            return true;
                        }
                        s.tx.send(env.clone()).is_ok()
                    });
                }
            }
            CollabEvent::ScrollbackBatch { .. } => {
                // Scrollback replies are per-request in the capnp
                // schema — they don't ride a stream. The legacy path
                // still fans a ScrollbackBatch through here when a
                // gossipsub cooperative response arrives (the
                // originating request has no memory of "who asked");
                // we drop it silently on the capnp side. Apps that
                // want late-arriving history repeat their
                // `fetchScrollbackFor` call.
            }
        }
    }

    /// Legacy-side `send_to_peer` fallback. Capnp doesn't use
    /// per-peer targeted delivery (scrollback answers inline) — this
    /// exists so [`PeerTable::send_to_peer`] can call through
    /// unconditionally without needing a runtime check on transport.
    pub fn send_to_peer(&self, _peer_id: u64, _event: &CollabEvent) -> bool {
        false
    }

    // -- subscription bookkeeping --------------------------------------

    fn ensure_channel(&self, channel_id: &str) {
        if let Ok(mut m) = self.channels.lock() {
            m.entry(channel_id.to_owned())
                .or_insert_with(ChannelSubscribers::default);
        }
    }

    fn subscribe_ops(
        &self,
        channel_id: &str,
        peer_id: u64,
    ) -> mpsc::UnboundedReceiver<OpEnvelope> {
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

/// Postcard-encode [`PresenceKey::pointer_default`]. Materialised
/// once for every cursor fanout — cheap enough not to cache.
fn default_pointer_key_bytes() -> Vec<u8> {
    postcard::to_stdvec(&PresenceKey::pointer_default()).unwrap_or_default()
}

// -----------------------------------------------------------------------------
// Server implementations
// -----------------------------------------------------------------------------

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
            // `identity` bytes are captured on the handle so tests /
            // future auditing can inspect them; the broker doesn't
            // dispatch on identity today (validation happens per-op
            // against `authorised_signers`). Retained rather than
            // dropped so a future extension can lift them into the
            // per-channel context snapshot.
            let identity = reader.get_identity()?.to_vec();

            state.ensure_channel(&channel_id);

            let handle = CapnpChannelHandle {
                state: state.clone(),
                channel_id: channel_id.clone(),
                peer_id,
                identity,
            };
            let client: channel_handle::Client = capnp_rpc::new_client(handle);
            results.get().set_handle(client);
            Ok(())
        })
    }
}

/// Per-channel handle. Holds the (channel_id, peer_id, identity)
/// tuple established by `join_channel`; every subsequent method call
/// (`send`, `subscribe*`, `presence`, `fetchScrollback*`) inherits
/// that context.
struct CapnpChannelHandle {
    state: Arc<CapnpBrokerState>,
    channel_id: String,
    peer_id: u64,
    /// The identity bytes the caller presented on join. Currently
    /// only retained for observability; role enforcement runs against
    /// `SignedOp.author` on the fanout hot path (same as the legacy
    /// broker). Marked `allow(dead_code)` while the field waits for
    /// its consumer.
    #[allow(dead_code)]
    identity: Vec<u8>,
}

impl channel_handle::Server for CapnpChannelHandle {
    fn send(
        self: Rc<Self>,
        params: channel_handle::SendParams,
        _results: channel_handle::SendResults,
    ) -> Promise<(), capnp::Error> {
        let state = self.state.clone();
        let peer_id = self.peer_id;
        Promise::from_future(async move {
            let op_bytes = params.get()?.get_op()?.to_vec();
            let envelope: SignedOp = postcard::from_bytes(&op_bytes).map_err(|e| {
                capnp::Error::failed(format!(
                    "capnp send: op is not a postcard-encoded SignedOp: {e}"
                ))
            })?;
            handle_signed_op_send(&state, peer_id, envelope);
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
        Promise::from_future(async move {
            let data = params.get()?.get_data()?.to_vec();
            // Route through the same fanout the legacy `SyncCRDT`
            // request would trigger. Preserves the "unsigned frames
            // reach every peer with empty attribution" behaviour the
            // pre-capnp bridge relied on.
            if let Some(t) = state.peer_table.upgrade() {
                t.fanout_except(&CollabEvent::CRDTUpdate { data }, peer_id);
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
        Promise::from_future(async move {
            let data = params.get()?.get_data()?.to_vec();
            if let Some(t) = state.peer_table.upgrade() {
                t.fanout_except(&CollabEvent::CRDTIncremental { data }, peer_id);
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
        // Legacy method: no artifact id. Answer with the nil-uuid
        // artifact so callers that never migrated get a consistent
        // (empty) reply rather than a random UUID that never matches.
        let broker_channels = self.state.broker_channels.clone();
        Promise::from_future(async move {
            let p = params.get()?;
            let before_seq = p.get_before_seq();
            let max = p.get_max();
            let artifact = ArtifactId(uuid::Uuid::nil());
            let (ops, exhausted, earliest_seq, latest_seq) =
                read_scrollback(&broker_channels, artifact, before_seq, max);
            let batch = results.get().init_batch();
            fill_scrollback_batch(batch, ops, exhausted, earliest_seq, latest_seq);
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
            let (ops, exhausted, earliest_seq, latest_seq) =
                read_scrollback(&broker_channels, artifact, before_seq, max);
            let batch = results.get().init_batch();
            fill_scrollback_batch(batch, ops, exhausted, earliest_seq, latest_seq);
            Ok(())
        })
    }
}

/// Apply a signed op arriving via the capnp `send` method. Runs the
/// same validation / audit / redaction / compaction pipeline the
/// legacy path does, then fans out via the shared [`PeerTable`].
///
/// Hoisted to a free function so the capnp send-handler and the
/// broker's fanout unification code test are both concise.
fn handle_signed_op_send(state: &Arc<CapnpBrokerState>, peer_id: u64, envelope: SignedOp) {
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
                if envelope.op_class == OpClass::Compaction {
                    if let Ok(cp) = collab_types::parse_compaction_payload(&envelope) {
                        let _ = ch_state.apply_compaction(&cp);
                    }
                }
            }
            if envelope.op_class == OpClass::Redact {
                apply_redaction_from_envelope(&envelope, artifact, &state.broker_channels);
            }
            let event = CollabEvent::SignedCRDTIncremental {
                envelope: Box::new(envelope),
                from_user_id: format!("peer-{peer_id}"),
            };
            if let Some(t) = state.peer_table.upgrade() {
                t.fanout_except(&event, peer_id);
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

/// Read a page of scrollback ops for `artifact` from the broker's
/// in-memory op log. Encodes each op as postcard bytes because the
/// capnp `ScrollbackBatch.ops` is `List(Data)`.
///
/// Returns `(encoded ops, exhausted, earliest_seq, latest_seq)`.
/// `ChannelState::scrollback_for` returns ops sorted ascending by seq,
/// so `earliest = ops.first().seq` and `latest = ops.last().seq`. Both
/// bracket values are `0` when the batch is empty.
fn read_scrollback(
    broker_channels: &Arc<Mutex<BrokerChannels>>,
    artifact: ArtifactId,
    before_seq: u64,
    max: u32,
) -> (Vec<Vec<u8>>, bool, u64, u64) {
    let Ok(mut ch) = broker_channels.lock() else {
        return (Vec::new(), true, 0, 0);
    };
    let ch_state = ch.state_for(artifact);
    // The channel-state's `scrollback_for` returns `Vec<SignedOp>` +
    // `exhausted: bool`; encode inline so callers don't need to
    // re-borrow the mutex.
    let (ops, exhausted) = ch_state.scrollback_for(artifact, before_seq, max as u16);
    let earliest_seq = ops.first().map(|op| op.seq).unwrap_or(0);
    let latest_seq = ops.last().map(|op| op.seq).unwrap_or(0);
    let encoded: Vec<Vec<u8>> = ops
        .into_iter()
        .filter_map(|op| postcard::to_stdvec(&op).ok())
        .collect();
    (encoded, exhausted, earliest_seq, latest_seq)
}

fn fill_scrollback_batch(
    mut b: scrollback_batch::Builder<'_>,
    ops: Vec<Vec<u8>>,
    exhausted: bool,
    earliest_seq: u64,
    latest_seq: u64,
) {
    let n = ops.len() as u32;
    let mut list = b.reborrow().init_ops(n);
    for (i, op) in ops.iter().enumerate() {
        list.set(i as u32, op);
    }
    // Bracket seqs come straight from the returned batch — the ops are
    // already sorted ascending by seq (see `ChannelState::scrollback_for`),
    // so first/last give the true bounds without a second decode.
    b.set_earliest_seq(earliest_seq);
    b.set_latest_seq(latest_seq);
    b.set_done(exhausted);
}

// -----------------------------------------------------------------------------
// Presence handle + streams
// -----------------------------------------------------------------------------

struct CapnpPresenceHandle {
    state: Arc<CapnpBrokerState>,
    channel_id: String,
    peer_id: u64,
}

impl presence_handle::Server for CapnpPresenceHandle {
    fn publish_cursor(
        self: Rc<Self>,
        params: presence_handle::PublishCursorParams,
        _results: presence_handle::PublishCursorResults,
    ) -> Promise<(), capnp::Error> {
        let state = self.state.clone();
        let peer_id = self.peer_id;
        Promise::from_future(async move {
            let r = params.get()?;
            let (x, y) = (r.get_x(), r.get_y());
            // Legacy JSON side gets a keyless `RemoteCursorMoved`
            // — that's the schema-v1 shape and the ui-side apps
            // already treat it as pointer-only. The capnp side sees
            // the real key bytes because our own `fanout_except`
            // synthesises them from `PresenceKey::pointer_default()`
            // when it processes this event.
            let event = CollabEvent::RemoteCursorMoved {
                user_id: format!("peer-{peer_id}"),
                x,
                y,
            };
            if let Some(t) = state.peer_table.upgrade() {
                t.fanout_except(&event, peer_id);
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
        Promise::from_future(async move {
            let r = params.get()?;
            let key_bytes = r.get_key_bytes()?.to_vec();
            let pos = r.get_pos();
            publish_keyed_presence(
                &state,
                peer_id,
                key_bytes,
                PresencePayload::Caret { pos: pos as usize },
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
        Promise::from_future(async move {
            let r = params.get()?;
            let key_bytes = r.get_key_bytes()?.to_vec();
            let start = r.get_start();
            let end = r.get_end();
            publish_keyed_presence(
                &state,
                peer_id,
                key_bytes,
                PresencePayload::Selection {
                    start: start as usize,
                    end: end as usize,
                },
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
        Promise::from_future(async move {
            let r = params.get()?;
            let key_bytes = r.get_key_bytes()?.to_vec();
            publish_keyed_presence(&state, peer_id, key_bytes, PresencePayload::Arrived);
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
        Promise::from_future(async move {
            let r = params.get()?;
            let key_bytes = r.get_key_bytes()?.to_vec();
            publish_keyed_presence(&state, peer_id, key_bytes, PresencePayload::Left);
            Ok(())
        })
    }

    fn subscribe_all(
        self: Rc<Self>,
        _params: presence_handle::SubscribeAllParams,
        mut results: presence_handle::SubscribeAllResults,
    ) -> Promise<(), capnp::Error> {
        let rx = self.state.subscribe_presence(&self.channel_id, self.peer_id);
        let server = PresenceStreamServer {
            rx: Rc::new(RefCell::new(rx)),
        };
        let client: presence_stream::Client = capnp_rpc::new_client(server);
        results.get().set_stream(client);
        Promise::ok(())
    }
}

/// Shared body of the four keyed presence publishers. Decodes the
/// postcard-encoded key so the legacy `RemotePresenceUpdate` frame
/// carries a typed `PresenceKey`; drops silently on decode failure
/// (a bad blob shouldn't crash the broker, and the ui-side stub
/// widget handles missing presence gracefully).
fn publish_keyed_presence(
    state: &Arc<CapnpBrokerState>,
    peer_id: u64,
    key_bytes: Vec<u8>,
    payload: PresencePayload,
) {
    let Ok(key) = postcard::from_bytes::<PresenceKey>(&key_bytes) else {
        tracing::debug!(
            len = key_bytes.len(),
            "capnp presence publish: key bytes not decodable — dropping",
        );
        return;
    };
    let event = CollabEvent::RemotePresenceUpdate {
        user_id: format!("peer-{peer_id}"),
        key,
        payload,
    };
    if let Some(t) = state.peer_table.upgrade() {
        t.fanout_except(&event, peer_id);
    }
}

// -----------------------------------------------------------------------------
// Stream servers
// -----------------------------------------------------------------------------

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
            let env = match rx.borrow_mut().recv().await {
                Some(e) => e,
                None => {
                    return Err(capnp::Error::disconnected(
                        "op stream fanout closed".into(),
                    ));
                }
            };
            let mut r = results.get();
            r.set_op(&env.op_bytes);
            r.set_from_user_id(&env.from_user_id);
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
            let env = match rx.borrow_mut().recv().await {
                Some(e) => e,
                None => {
                    return Err(capnp::Error::disconnected(
                        "presence stream fanout closed".into(),
                    ));
                }
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
                PresencePayload::Selection { start, end } => {
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
            let env = match rx.borrow_mut().recv().await {
                Some(e) => e,
                None => {
                    return Err(capnp::Error::disconnected(
                        "cursor stream fanout closed".into(),
                    ));
                }
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
            let env = match rx.borrow_mut().recv().await {
                Some(e) => e,
                None => {
                    return Err(capnp::Error::disconnected(
                        "context stream fanout closed".into(),
                    ));
                }
            };
            let mut ctx = results.get().init_ctx();
            {
                let mut collabs = ctx
                    .reborrow()
                    .init_collaborators(env.collaborators.len() as u32);
                for (i, c) in env.collaborators.iter().enumerate() {
                    let mut slot = collabs.reborrow().get(i as u32);
                    slot.set_user_id(&c.id);
                    slot.set_display_name(&c.name);
                    slot.set_color(pack_color(c.color));
                }
            }
            {
                let mut perms = ctx.init_permissions(env.permissions.len() as u32);
                for (i, p) in env.permissions.iter().enumerate() {
                    perms.set(i as u32, p.as_str());
                }
            }
            Ok(())
        })
    }
}

/// Convert `[f32; 3]` RGB in 0..=1 to the wire's `0x00RRGGBB`.
fn pack_color(rgb: [f32; 3]) -> u32 {
    let clamp = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u32;
    let r = clamp(rgb[0]);
    let g = clamp(rgb[1]);
    let b = clamp(rgb[2]);
    (r << 16) | (g << 8) | b
}

// -----------------------------------------------------------------------------
// Listener wiring
// -----------------------------------------------------------------------------

/// Bind `socket_path` and spawn the capnp accept loop. Attaches the
/// created [`CapnpBrokerState`] to `peer_table` so every event that
/// flows through the legacy fanout ALSO lands on capnp subscribers.
///
/// The accept thread runs a `current_thread` tokio runtime + a
/// `LocalSet` (capnp-rpc futures are `!Send`). It polls `quit` on a
/// 100 ms cadence between blocking accepts so a broker shutdown is
/// bounded.
pub fn spawn(
    socket_path: PathBuf,
    peer_table: Arc<PeerTable>,
    broker_channels: Arc<Mutex<BrokerChannels>>,
    quit: Arc<AtomicBool>,
) -> std::io::Result<JoinHandle<()>> {
    let _ = std::fs::remove_file(&socket_path);
    let state = CapnpBrokerState::new(&peer_table, broker_channels);
    peer_table.attach_capnp(Arc::clone(&state));

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
                let listener = match tokio::net::UnixListener::bind(&socket_path) {
                    Ok(l) => l,
                    Err(e) => {
                        tracing::warn!(
                            "capnp broker: bind {} failed: {e}",
                            socket_path.display()
                        );
                        return;
                    }
                };
                tracing::info!(
                    "capnp broker: bound {} for native SDK access",
                    socket_path.display()
                );
                loop {
                    if quit.load(Ordering::Relaxed) {
                        break;
                    }
                    let stream = tokio::select! {
                        biased;
                        _ = tokio::time::sleep(Duration::from_millis(100)) => continue,
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
                            tracing::warn!(
                                "capnp broker: peer-{peer_id} disconnected: {e}"
                            );
                        }
                    });
                }
            });
        })?;

    Ok(thread)
}

/// Convenience used by tests: dial the capnp socket at `path` on
/// the current tokio LocalSet, returning a bootstrap
/// `CollabServiceClient` capability. Not meant for production apps —
/// production apps go through `runtime_collab::try_connect` which
/// uses the launcher-passed fd instead of an explicit path.
///
/// Public rather than `#[cfg(test)]` so out-of-tree integration
/// tests (e.g. `shell/tests/*`) can reach it too.
pub async fn dial_capnp_for_test(
    path: PathBuf,
) -> Result<
    (
        runtime_collab::raw::collab_service::Client,
        tokio::task::JoinHandle<()>,
    ),
    std::io::Error,
> {
    // Retry-with-backoff up to ~2 s: broker spawn is asynchronous and
    // the capnp accept loop may not have finished binding when the
    // caller races in. Bounded so a genuinely-missing socket fails
    // fast; ~50 attempts × 50 ms.
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
        match stream {
            Some(s) => s,
            None => {
                return Err(last_err.unwrap_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "capnp socket did not appear within 2s",
                    )
                }));
            }
        }
    };
    let (r, w) = stream.into_split();
    let network = Box::new(twoparty::VatNetwork::new(
        futures::io::BufReader::new(r.compat()),
        futures::io::BufWriter::new(w.compat_write()),
        rpc_twoparty_capnp::Side::Client,
        Default::default(),
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
        Default::default(),
    ));
    let service = CapnpCollabService {
        state,
        peer_id,
    };
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

    use crate::shell::collab_broker::{BrokerHandle, spawn_full};

    fn unique_paths(label: &str) -> (PathBuf, PathBuf) {
        let pid = std::process::id();
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let json = std::env::temp_dir()
            .join(format!("qos-broker-{label}-{pid}-{nanos}.json.sock"));
        let capnp = std::env::temp_dir()
            .join(format!("qos-broker-{label}-{pid}-{nanos}.capnp.sock"));
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

    /// Set up a `ChannelState` that authorises one Editor. Returns
    /// the artifact + editor keys + the built attestation id.
    fn setup_role_table(
        handle: &BrokerHandle,
    ) -> (
        ArtifactId,
        SigningKey,
        Ed25519PublicKey,
        AttestationId,
    ) {
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

    /// A legacy JSON client sends a signed op; a capnp subscriber
    /// on the same broker sees it. Proves the shared fanout path.
    #[test]
    fn legacy_signed_op_reaches_capnp_subscriber() {
        let (json_path, capnp_path) = unique_paths("legacy2capnp");
        let handle = spawn_full(json_path.clone(), Some(capnp_path.clone()), None)
            .expect("spawn broker");
        let (artifact, editor_sk, editor_pk, editor_att_id) = setup_role_table(&handle);

        // Connect the legacy JSON peer + say hello (so the broker
        // sees a subscriber to fan out to).
        let mut legacy = wait_for_json_broker(&json_path);
        write_json_frame(
            &mut legacy,
            &CollabRequest::Hello {
                app_id: "legacy-editor".into(),
                cluster_id: uuid::Uuid::new_v4(),
            },
        );

        // Capnp-side setup on a dedicated thread with its own tokio
        // LocalSet. It joins a channel + subscribes to ops, then
        // pushes every op it receives onto a std mpsc back to the
        // test thread.
        let (op_tx, op_rx) = std::sync::mpsc::channel::<(Vec<u8>, String)>();
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
                    .join_channel("channel-A", b"identity-A")
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

        // Let the capnp subscriber register before the legacy peer
        // sends. The capnp side registers its op-tx into
        // `CapnpBrokerState.channels["channel-A"].op` synchronously
        // inside `subscribe()`; poll on the broker's state until the
        // slot is populated.
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let has_sub = {
                // Peek into the broker via public accessors. We
                // don't have one for capnp subscriber count, so use
                // the JSON `peer_count` as a proxy for "broker is
                // up" + a tiny sleep for capnp connect.
                handle.peer_count() >= 1
            };
            if has_sub {
                break;
            }
            if Instant::now() > deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        // Extra grace for the capnp thread to complete join+subscribe.
        std::thread::sleep(Duration::from_millis(200));

        // Legacy peer publishes a signed op.
        let op = build_signed_op(&editor_sk, editor_pk, artifact, editor_att_id, 1, b"hi-capnp");
        write_json_frame(
            &mut legacy,
            &CollabRequest::SignedSyncIncremental {
                envelope: Box::new(op.clone()),
            },
        );

        // Capnp subscriber should receive the op.
        let (op_bytes, from) = op_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("capnp subscriber should receive the op");
        let received: SignedOp =
            postcard::from_bytes(&op_bytes).expect("received bytes are a SignedOp postcard");
        assert_eq!(received.artifact, artifact);
        assert_eq!(received.seq, 1);
        assert_eq!(received.payload, b"hi-capnp");
        assert!(from.starts_with("peer-"), "attribution shape: {from}");

        drop(legacy);
        drop(handle);
    }

    /// A capnp client sends a signed op; a legacy JSON subscriber
    /// on the same broker sees it as `SignedCRDTIncremental`.
    #[test]
    fn capnp_signed_op_reaches_legacy_subscriber() {
        let (json_path, capnp_path) = unique_paths("capnp2legacy");
        let handle = spawn_full(json_path.clone(), Some(capnp_path.clone()), None)
            .expect("spawn broker");
        let (artifact, editor_sk, editor_pk, editor_att_id) = setup_role_table(&handle);

        // Legacy peer connects + says hello so the broker will
        // fan events out to it.
        let mut legacy = wait_for_json_broker(&json_path);
        write_json_frame(
            &mut legacy,
            &CollabRequest::Hello {
                app_id: "legacy-observer".into(),
                cluster_id: uuid::Uuid::new_v4(),
            },
        );

        // Capnp thread: join channel + send the signed op.
        let op = build_signed_op(&editor_sk, editor_pk, artifact, editor_att_id, 1, b"hi-legacy");
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
                    .join_channel("channel-B", b"identity-B")
                    .await
                    .expect("join channel");
                channel.send(&op_bytes).await.expect("send signed op");
                // Keep the connection alive briefly so the RPC
                // driver has a chance to actually push the frame.
                tokio::time::sleep(Duration::from_millis(500)).await;
            });
        });

        // Legacy peer should receive it. Give the capnp thread + rpc
        // driver a moment to fully push.
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut seen = false;
        while Instant::now() < deadline {
            match read_json_event(&mut legacy, Duration::from_millis(200)) {
                Some(ui::collaboration::CollabEvent::SignedCRDTIncremental { envelope, .. }) => {
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

    /// Capnp `publishCursor(x, y)` fans out to legacy peers as
    /// `RemoteCursorMoved`; the pointer_default key bytes route on
    /// the capnp presence-all stream.
    #[test]
    fn capnp_cursor_publish_reaches_legacy_and_capnp() {
        let (json_path, capnp_path) = unique_paths("cursor-fan");
        let handle = spawn_full(json_path.clone(), Some(capnp_path.clone()), None)
            .expect("spawn broker");

        // Legacy observer.
        let mut legacy = wait_for_json_broker(&json_path);
        write_json_frame(
            &mut legacy,
            &CollabRequest::Hello {
                app_id: "legacy-cursor-observer".into(),
                cluster_id: uuid::Uuid::new_v4(),
            },
        );

        // Capnp thread: join channel, subscribe presence, then a
        // SECOND capnp connection publishes a cursor. Two capnp
        // sessions so publisher != subscriber.
        let (presence_tx, presence_rx) =
            std::sync::mpsc::channel::<(String, Vec<u8>, f32, f32)>();
        let capnp_path_sub = capnp_path.clone();
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
                    .join_channel("cursor-ch", b"identity-sub")
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

        // Grace period for subscriber to register.
        std::thread::sleep(Duration::from_millis(400));

        let capnp_path_pub = capnp_path.clone();
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
                    .join_channel("cursor-ch", b"identity-pub")
                    .await
                    .expect("join channel");
                let ph = channel.presence().await.expect("presence");
                ph.publish_cursor(11.0, 22.0).await.expect("publish cursor");
                tokio::time::sleep(Duration::from_millis(500)).await;
            });
        });

        // Legacy observer sees `RemoteCursorMoved`.
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
        assert!(legacy_seen, "legacy peer should have seen RemoteCursorMoved");

        // Capnp presence subscriber sees a Cursor with the
        // canonical pointer_default key bytes.
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

    /// Capnp `fetch_scrollback_for` returns ops previously validated
    /// through the legacy JSON path — proving the shared op-log
    /// storage.
    #[test]
    fn capnp_fetch_scrollback_for_reads_legacy_written_ops() {
        let (json_path, capnp_path) = unique_paths("scrollback");
        let handle = spawn_full(json_path.clone(), Some(capnp_path.clone()), None)
            .expect("spawn broker");
        let (artifact, editor_sk, editor_pk, editor_att_id) = setup_role_table(&handle);

        // Legacy peer writes ops through the broker.
        let mut legacy = wait_for_json_broker(&json_path);
        write_json_frame(
            &mut legacy,
            &CollabRequest::Hello {
                app_id: "writer".into(),
                cluster_id: uuid::Uuid::new_v4(),
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

        // Capnp client fetches. Uses the ArtifactId's UUID bytes.
        let artifact_bytes = artifact.0.as_bytes().to_vec();
        let capnp_path_clone = capnp_path.clone();
        let (batch_tx, batch_rx) =
            std::sync::mpsc::channel::<runtime_collab::ScrollbackBatch>();
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
                    .join_channel("sb-ch", b"identity")
                    .await
                    .expect("join");
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
        // Bracket seqs must reflect the ops in the batch — earliest is
        // the first op's seq, latest the last. Both were `0` before the
        // op-log wired them in.
        assert_eq!(
            batch.earliest_seq, 1,
            "earliest_seq should match the first op returned"
        );
        assert_eq!(
            batch.latest_seq, 3,
            "latest_seq should match the last op returned"
        );

        drop(legacy);
        drop(handle);
    }
}
