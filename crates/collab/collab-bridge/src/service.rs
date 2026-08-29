//! Cap'n Proto server implementations for the SDK-facing collab
//! interfaces. Each capnp method translates requests onto the upstream
//! broker via [`UpstreamHandle`] and returns a Promise the RPC runtime
//! drives. Server structs hold `Rc`-wrapped state so they can live under
//! the LocalSet without Send bounds.

use std::cell::RefCell;
use std::rc::Rc;

use capnp::capability::Promise;
use futures::FutureExt;

use runtime_collab::raw::{
    OpKind, channel_handle, collab_service, context_stream, cursor_stream, presence_event,
    presence_handle, presence_payload, presence_stream, scrollback_batch, stream,
};

use crate::translate::{hello_from_join, presence_request};
use crate::upstream::{PresenceEnvelope, ScrollbackReply, UpstreamHandle, connect};
use crate::wire::{CollabRequest, PresencePayload};
use collab_types::ArtifactId;

/// Factory closure that opens the upstream connection; kept as a closure so tests can substitute a fake broker without touching the real filesystem.
pub type UpstreamOpener = Rc<
    dyn Fn() -> std::pin::Pin<
        Box<dyn std::future::Future<Output = std::io::Result<UpstreamHandle>>>,
    >,
>;

/// Root capnp service, owned by the RPC system for one client connection; `joinChannel` is what dials the upstream broker.
pub struct BridgeCollabService {
    opener: UpstreamOpener,
}

impl BridgeCollabService {
    pub fn new(opener: UpstreamOpener) -> Self {
        Self { opener }
    }
}

impl collab_service::Server for BridgeCollabService {
    fn join_channel(
        self: Rc<Self>,
        params: collab_service::JoinChannelParams,
        mut results: collab_service::JoinChannelResults,
    ) -> Promise<(), capnp::Error> {
        let opener = self.opener.clone();
        Promise::from_future(async move {
            let reader = params.get()?;
            let channel_id = reader
                .get_channel_id()?
                .to_str()
                .map_err(|e| capnp::Error::failed(format!("channelId not utf8: {e}")))?
                .to_owned();
            let identity = reader.get_identity()?.to_vec();
            tracing::trace!(channel = %channel_id, identity_bytes = identity.len(), "SDK joinChannel");

            let upstream = opener().await.map_err(|e| {
                capnp::Error::failed(format!(
                    "upstream broker unavailable: {e}"
                ))
            })?;

            let hello = hello_from_join(&channel_id, &identity);
            upstream
                .send_request(&hello)
                .await
                .map_err(|e| capnp::Error::failed(format!("upstream Hello send failed: {e}")))?;

            let ch = BridgeChannel::new(upstream);
            let client: channel_handle::Client = capnp_rpc::new_client(ch);
            results.get().set_handle(client);
            Ok(())
        })
    }
}

/// Per-channel handle server, holding one upstream connection; every
/// method translates its capnp params to a `CollabRequest` and pushes it
/// upstream. Scrollback replies are matched FIFO per artifact (see
/// `pending_scrollback`).
pub struct BridgeChannel {
    upstream: UpstreamHandle,
    /// FIFO of artifact ids for outstanding `fetch_scrollback` calls, matched against each incoming `ScrollbackReply` by artifact id.
    pending_scrollback: Rc<RefCell<Vec<ArtifactId>>>,
}

impl BridgeChannel {
    pub fn new(upstream: UpstreamHandle) -> Self {
        Self {
            upstream,
            pending_scrollback: Rc::new(RefCell::new(Vec::new())),
        }
    }
}

impl channel_handle::Server for BridgeChannel {
    fn send(
        self: Rc<Self>,
        params: channel_handle::SendParams,
        _results: channel_handle::SendResults,
    ) -> Promise<(), capnp::Error> {
        let upstream = self.upstream.clone();
        Promise::from_future(async move {
            let op = params.get()?.get_op()?.to_vec();
            tracing::trace!(bytes = op.len(), "SDK send signed op");
            let envelope: collab_types::SignedOp = postcard::from_bytes(&op).map_err(|e| {
                capnp::Error::failed(format!(
                    "SDK-provided signed op is not a valid SignedOp postcard: {e}"
                ))
            })?;
            let req = CollabRequest::SignedSyncIncremental {
                envelope: Box::new(envelope),
            };
            upstream.send_request(&req).await.map_err(|e| {
                capnp::Error::failed(format!("upstream send failed: {e}"))
            })
        })
    }

    fn subscribe(
        self: Rc<Self>,
        _params: channel_handle::SubscribeParams,
        mut results: channel_handle::SubscribeResults,
    ) -> Promise<(), capnp::Error> {
        let stream = BridgeOpStream {
            rx: self.upstream.op_rx.clone(),
        };
        let client: stream::Client = capnp_rpc::new_client(stream);
        results.get().set_stream(client);
        Promise::ok(())
    }

    fn presence(
        self: Rc<Self>,
        _params: channel_handle::PresenceParams,
        mut results: channel_handle::PresenceResults,
    ) -> Promise<(), capnp::Error> {
        let presence = BridgePresenceHandle {
            upstream: self.upstream.clone(),
        };
        let client: presence_handle::Client = capnp_rpc::new_client(presence);
        results.get().set_handle(client);
        Promise::ok(())
    }

    fn send_unsigned_snapshot(
        self: Rc<Self>,
        params: channel_handle::SendUnsignedSnapshotParams,
        _results: channel_handle::SendUnsignedSnapshotResults,
    ) -> Promise<(), capnp::Error> {
        let upstream = self.upstream.clone();
        Promise::from_future(async move {
            let data = params.get()?.get_data()?.to_vec();
            tracing::trace!(bytes = data.len(), "SDK unsigned snapshot");
            upstream
                .send_request(&CollabRequest::SyncCRDT { data })
                .await
                .map_err(|e| capnp::Error::failed(format!("upstream send failed: {e}")))
        })
    }

    fn send_unsigned_incremental(
        self: Rc<Self>,
        params: channel_handle::SendUnsignedIncrementalParams,
        _results: channel_handle::SendUnsignedIncrementalResults,
    ) -> Promise<(), capnp::Error> {
        let upstream = self.upstream.clone();
        Promise::from_future(async move {
            let data = params.get()?.get_data()?.to_vec();
            tracing::trace!(bytes = data.len(), "SDK unsigned incremental");
            upstream
                .send_request(&CollabRequest::SyncIncremental { data })
                .await
                .map_err(|e| capnp::Error::failed(format!("upstream send failed: {e}")))
        })
    }

    fn subscribe_context(
        self: Rc<Self>,
        _params: channel_handle::SubscribeContextParams,
        mut results: channel_handle::SubscribeContextResults,
    ) -> Promise<(), capnp::Error> {
        let stream = BridgeContextStream {
            rx: self.upstream.context_rx.clone(),
        };
        let client: context_stream::Client = capnp_rpc::new_client(stream);
        results.get().set_stream(client);
        Promise::ok(())
    }

    fn fetch_scrollback(
        self: Rc<Self>,
        params: channel_handle::FetchScrollbackParams,
        mut results: channel_handle::FetchScrollbackResults,
    ) -> Promise<(), capnp::Error> {
        let upstream = self.upstream.clone();
        let pending = self.pending_scrollback.clone();
        Promise::from_future(async move {
            let p = params.get()?;
            let before_seq = p.get_before_seq();
            let max = p.get_max();
            tracing::trace!(before_seq, max, "SDK fetch scrollback");

            // The SDK's fetch_scrollback has no artifact id; the legacy shell broker
            // keys by artifact, so we route on `ArtifactId::default()` (nil UUID) — a
            // documented lossy translation. Per-artifact scrollback needs a native
            // Cap'n Proto broker instead.
            let artifact = ArtifactId::default();
            pending.borrow_mut().push(artifact);

            let req = CollabRequest::FetchScrollback {
                artifact,
                before_seq,
                max: max as u16,
            };
            upstream.send_request(&req).await.map_err(|e| {
                capnp::Error::failed(format!("upstream fetch send failed: {e}"))
            })?;

            // Await the matching reply, FIFO by artifact id; non-matching entries are
            // discarded (shouldn't happen with the nil-artifact placeholder, but future-proofs it).
            let reply = pop_matching_reply(&upstream, artifact).await?;
            fill_scrollback_batch(results.get().init_batch(), reply);
            pending.borrow_mut().retain(|a| *a != artifact);
            Ok(())
        })
    }
}

fn fill_scrollback_batch(
    mut b: scrollback_batch::Builder<'_>,
    reply: ScrollbackReply,
) {
    let (mut earliest, mut latest) = (0u64, 0u64);
    if !reply.ops.is_empty() {
        earliest = reply.ops.first().map(|op| op.seq).unwrap_or(0);
        latest = reply.ops.last().map(|op| op.seq).unwrap_or(0);
    }
    let n = reply.ops.len() as u32;
    let mut ops = b.reborrow().init_ops(n);
    for (i, op) in reply.ops.iter().enumerate() {
        match postcard::to_stdvec(&**op) {
            Ok(bytes) => ops.set(i as u32, &bytes),
            Err(e) => {
                tracing::warn!(error = %e, "cannot re-encode scrollback op — writing empty slot");
                ops.set(i as u32, &[]);
            }
        }
    }
    b.set_earliest_seq(earliest);
    b.set_latest_seq(latest);
    b.set_done(reply.exhausted);
}

async fn pop_matching_reply(
    upstream: &UpstreamHandle,
    artifact: ArtifactId,
) -> Result<ScrollbackReply, capnp::Error> {
    let mut rx = upstream.scrollback_rx.lock().await;
    loop {
        match rx.recv().await {
            Some(reply) if reply.artifact == artifact => return Ok(reply),
            Some(other) => {
                // No re-inject primitive on mpsc, so a mismatch can't be put back; in
                // practice the placeholder artifact means every reply matches, so this
                // arm is defensive.
                tracing::debug!(
                    got = ?other.artifact,
                    want = ?artifact,
                    "scrollback reply artifact mismatch — dropping"
                );
                continue;
            }
            None => {
                return Err(capnp::Error::disconnected(
                    "upstream closed before scrollback reply".into(),
                ));
            }
        }
    }
}


pub struct BridgeOpStream {
    rx: std::sync::Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<crate::upstream::OpEnvelope>>>,
}

impl stream::Server for BridgeOpStream {
    fn read(
        self: Rc<Self>,
        _params: stream::ReadParams,
        mut results: stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let rx = self.rx.clone();
        Promise::from_future(async move {
            let ev = rx.lock().await.recv().await.ok_or_else(|| {
                capnp::Error::disconnected("op stream upstream closed".into())
            })?;
            let mut r = results.get();
            r.set_op(&ev.op_bytes);
            r.set_from_user_id(&ev.from_user_id);
            // Tag which of the three frame kinds these bytes are — a reader can't tell from the bytes alone.
            r.set_kind(match ev.kind {
                crate::upstream::OpKind::Signed => OpKind::Signed,
                crate::upstream::OpKind::UnsignedIncremental => OpKind::UnsignedIncremental,
                crate::upstream::OpKind::UnsignedSnapshot => OpKind::UnsignedSnapshot,
            });
            Ok(())
        })
    }
}


pub struct BridgeContextStream {
    rx: std::sync::Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<crate::upstream::ContextSnapshot>>>,
}

impl context_stream::Server for BridgeContextStream {
    fn read(
        self: Rc<Self>,
        _params: context_stream::ReadParams,
        mut results: context_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let rx = self.rx.clone();
        Promise::from_future(async move {
            let snap = rx.lock().await.recv().await.ok_or_else(|| {
                capnp::Error::disconnected("context stream upstream closed".into())
            })?;
            let mut ctx = results.get().init_ctx();
            {
                let mut collabs = ctx
                    .reborrow()
                    .init_collaborators(snap.collaborators.len() as u32);
                for (i, c) in snap.collaborators.iter().enumerate() {
                    let mut slot = collabs.reborrow().get(i as u32);
                    slot.set_user_id(&c.id);
                    slot.set_display_name(&c.name);
                    slot.set_color(pack_color(c.color));
                    slot.set_is_agent(c.is_agent);
                    slot.set_is_self(c.is_self);
                }
            }
            {
                let mut perms = ctx.init_permissions(snap.permissions.len() as u32);
                for (i, p) in snap.permissions.iter().enumerate() {
                    perms.set(i as u32, p.as_str());
                }
            }
            Ok(())
        })
    }
}

/// Converts `[f32;3]` RGB (0.0..=1.0) into a packed `0x00RRGGBB` u32 per the schema.
fn pack_color(rgb: [f32; 3]) -> u32 {
    let clamp = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u32;
    let r = clamp(rgb[0]);
    let g = clamp(rgb[1]);
    let b = clamp(rgb[2]);
    (r << 16) | (g << 8) | b
}


pub struct BridgePresenceHandle {
    upstream: UpstreamHandle,
}

impl presence_handle::Server for BridgePresenceHandle {
    fn publish_cursor(
        self: Rc<Self>,
        params: presence_handle::PublishCursorParams,
        _results: presence_handle::PublishCursorResults,
    ) -> Promise<(), capnp::Error> {
        let upstream = self.upstream.clone();
        Promise::from_future(async move {
            let r = params.get()?;
            let (x, y) = (r.get_x(), r.get_y());
            // SDK-side pointer publish carries no key, but the shell broker's
            // `Presence` variant requires one — synthesize a stable placeholder Tab
            // key; apps needing per-object cursor routing should use
            // publish_selection with start==end instead.
            let key = default_presence_key();
            let req = CollabRequest::Presence {
                key,
                payload: PresencePayload::Pointer { x, y },
            };
            upstream
                .send_request(&req)
                .await
                .map_err(|e| capnp::Error::failed(format!("upstream send failed: {e}")))
        })
    }

    fn publish_pointer(
        self: Rc<Self>,
        params: presence_handle::PublishPointerParams,
        _results: presence_handle::PublishPointerResults,
    ) -> Promise<(), capnp::Error> {
        // Keyed door like caret/selection/arrived/left: unlike `publish_cursor`
        // above, this key must be the caller's own, since it names the coordinate
        // space the position was measured in.
        publish_keyed(
            self.upstream.clone(),
            params.get().and_then(|r| {
                Ok((
                    r.get_key_bytes()?.to_vec(),
                    PresencePayload::Pointer {
                        x: r.get_x(),
                        y: r.get_y(),
                    },
                ))
            }),
        )
    }

    fn subscribe_remote_cursors(
        self: Rc<Self>,
        _params: presence_handle::SubscribeRemoteCursorsParams,
        mut results: presence_handle::SubscribeRemoteCursorsResults,
    ) -> Promise<(), capnp::Error> {
        // Legacy schema-v1 stream: yields cursor-payload events only, skipping
        // others from the shared presence queue. Because the queue is shared with
        // `subscribe_all`, whichever stream reads first consumes the event — apps
        // needing both should call `subscribe_all` and discriminate on the payload.
        let handle = BridgeCursorStream {
            presence: self.upstream.presence.clone(),
        };
        let client: cursor_stream::Client = capnp_rpc::new_client(handle);
        results.get().set_stream(client);
        Promise::ok(())
    }

    fn publish_caret(
        self: Rc<Self>,
        params: presence_handle::PublishCaretParams,
        _results: presence_handle::PublishCaretResults,
    ) -> Promise<(), capnp::Error> {
        publish_keyed(
            self.upstream.clone(),
            params.get().and_then(|r| {
                Ok((r.get_key_bytes()?.to_vec(), PresencePayload::Caret { pos: r.get_pos() as usize }))
            }),
        )
    }

    fn publish_selection(
        self: Rc<Self>,
        params: presence_handle::PublishSelectionParams,
        _results: presence_handle::PublishSelectionResults,
    ) -> Promise<(), capnp::Error> {
        publish_keyed(
            self.upstream.clone(),
            params.get().and_then(|r| {
                Ok((
                    r.get_key_bytes()?.to_vec(),
                    PresencePayload::Selection {
                        start: r.get_start() as usize,
                        end: r.get_end() as usize,
                    },
                ))
            }),
        )
    }

    fn publish_arrived(
        self: Rc<Self>,
        params: presence_handle::PublishArrivedParams,
        _results: presence_handle::PublishArrivedResults,
    ) -> Promise<(), capnp::Error> {
        publish_keyed(
            self.upstream.clone(),
            params
                .get()
                .and_then(|r| Ok((r.get_key_bytes()?.to_vec(), PresencePayload::Arrived))),
        )
    }

    fn publish_left(
        self: Rc<Self>,
        params: presence_handle::PublishLeftParams,
        _results: presence_handle::PublishLeftResults,
    ) -> Promise<(), capnp::Error> {
        publish_keyed(
            self.upstream.clone(),
            params
                .get()
                .and_then(|r| Ok((r.get_key_bytes()?.to_vec(), PresencePayload::Left))),
        )
    }

    fn subscribe_all(
        self: Rc<Self>,
        _params: presence_handle::SubscribeAllParams,
        mut results: presence_handle::SubscribeAllResults,
    ) -> Promise<(), capnp::Error> {
        let handle = BridgePresenceStream {
            presence: self.upstream.presence.clone(),
        };
        let client: presence_stream::Client = capnp_rpc::new_client(handle);
        results.get().set_stream(client);
        Promise::ok(())
    }
}

/// Synthesizes a stable placeholder `PresenceKey` for `publish_cursor` (which carries none); the shell broker treats it as an opaque routing tag.
fn default_presence_key() -> crate::wire::PresenceKey {
    crate::wire::PresenceKey::Tab {
        id: "qos-collab-bridge:cursor".into(),
    }
}

/// Shared body for keyed publishers: decodes the postcard key, dropping the event on decode failure.
fn publish_keyed(
    upstream: UpstreamHandle,
    decoded: Result<(Vec<u8>, PresencePayload), capnp::Error>,
) -> Promise<(), capnp::Error> {
    Promise::from_future(async move {
        let (key_bytes, payload) = decoded?;
        let Some(req) = presence_request(&key_bytes, payload.clone()) else {
            tracing::warn!(payload = ?payload, "SDK PresenceKey postcard decode failed — dropping");
            return Ok(());
        };
        upstream
            .send_request(&req)
            .await
            .map_err(|e| capnp::Error::failed(format!("upstream presence send failed: {e}")))
    }.boxed_local())
}


pub struct BridgeCursorStream {
    presence: std::sync::Arc<crate::upstream::DropOldest<PresenceEnvelope>>,
}

impl cursor_stream::Server for BridgeCursorStream {
    fn read(
        self: Rc<Self>,
        _params: cursor_stream::ReadParams,
        mut results: cursor_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let presence = self.presence.clone();
        Promise::from_future(async move {
            loop {
                match presence.pop().await {
                    PresenceEnvelope::Cursor { user_id, x, y } => {
                        let mut r = results.get();
                        r.set_user_id(&user_id);
                        r.set_x(x);
                        r.set_y(y);
                        return Ok(());
                    }
                    // Skip non-cursor events on this legacy stream.
                    PresenceEnvelope::Update { .. } => continue,
                }
            }
        })
    }
}

pub struct BridgePresenceStream {
    presence: std::sync::Arc<crate::upstream::DropOldest<PresenceEnvelope>>,
}

impl presence_stream::Server for BridgePresenceStream {
    fn read(
        self: Rc<Self>,
        _params: presence_stream::ReadParams,
        mut results: presence_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let presence = self.presence.clone();
        Promise::from_future(async move {
            let env = presence.pop().await;
            let mut ev: presence_event::Builder = results.get().init_event();
            match env {
                PresenceEnvelope::Cursor { user_id, x, y } => {
                    ev.set_user_id(&user_id);
                    // Legacy cursor path carries no key; emit an empty byte-string so
                    // decoders reading `keyBytes` see a well-formed empty blob.
                    ev.set_key_bytes(&[]);
                    let p: presence_payload::Builder = ev.init_payload();
                    let mut c = p.init_cursor();
                    c.set_x(x);
                    c.set_y(y);
                }
                PresenceEnvelope::Update {
                    user_id,
                    key_bytes,
                    payload,
                } => {
                    ev.set_user_id(&user_id);
                    ev.set_key_bytes(&key_bytes);
                    let mut p: presence_payload::Builder = ev.init_payload();
                    match payload {
                        PresencePayload::Pointer { x, y } => {
                            // Keyed `pointer` case: these `key_bytes` are the publisher's own, naming
                            // the coordinate space (unlike the keyless `Cursor` envelope above).
                            let mut c = p.init_pointer();
                            c.set_x(x);
                            c.set_y(y);
                        }
                        PresencePayload::Caret { pos } => {
                            p.init_caret().set_pos(pos as u64);
                        }
                        PresencePayload::Selection { start, end } => {
                            let mut s = p.init_selection();
                            s.set_start(start as u64);
                            s.set_end(end as u64);
                        }
                        PresencePayload::Arrived => p.set_arrived(()),
                        PresencePayload::Left => p.set_left(()),
                    }
                }
            }
            Ok(())
        })
    }
}


/// Builds a [`BridgeCollabService`] whose upstream opener dials `path` on every `joinChannel` call.
pub fn service_for_path(path: std::path::PathBuf) -> BridgeCollabService {
    let opener: UpstreamOpener = Rc::new(move || {
        let p = path.clone();
        Box::pin(async move { connect(p).await })
    });
    BridgeCollabService::new(opener)
}
