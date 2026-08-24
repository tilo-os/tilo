//! `runtime-collab` — Cap'n Proto client for the qOS
//! collaboration broker.
//!
//! ## Usage shape
//!
//! ```ignore
//! let local = tokio::task::LocalSet::new();
//! local.run_until(async {
//!     let broker = runtime_collab::try_connect()
//!         .map_err(|e| { eprintln!("collab unavailable: {e}"); e })?;
//!     let channel = broker
//!         .join_channel("channel-id", identity_bytes)
//!         .await?;
//!     channel.send(op_bytes).await?;
//!     let mut sub = channel.subscribe().await?;
//!     while let Ok(next) = sub.read().await {
//!         handle(next);
//!     }
//! }).await;
//! ```
//!
//! Every call returns [`NotAvailable`] if the broker is missing or
//! the connection drops, never a panic.

use std::rc::Rc;

use runtime_portal::{NotAvailable, UnixCapnpClient, service_fd};

// Generated schema lands in OUT_DIR with the filename
// `<schema>_capnp.rs`. The generated code references its own module
// path absolutely as `crate::collab_capnp`, so we MUST include it
// at the crate root with that exact module name. This is the same
// pattern capnp's own samples use.
#[allow(clippy::all)]
#[allow(unused)]
#[allow(dead_code)]
pub mod collab_capnp {
    include!(concat!(env!("OUT_DIR"), "/collab_capnp.rs"));
}

use capnp_rpc::rpc_twoparty_capnp;

pub use collab_capnp::collab_service::Client as CollabServiceClient;
pub use collab_capnp::context_stream::Client as ContextStreamClient;
pub use collab_capnp::cursor_stream::Client as CursorStreamClient;
pub use collab_capnp::presence_stream::Client as PresenceStreamClient;
pub use collab_capnp::blob_stream::Client as BlobStreamClient;
pub use collab_capnp::stream::Client as StreamClient;

/// Try to dial the qOS collab broker via the launcher-supplied
/// service fd. Returns [`NotAvailable`] when the env var is unset
/// (most likely: app is running as a Flatpak on stock Linux) or
/// the fd is no longer open.
///
/// The returned client is the typed root capability; wrap it in a
/// [`CollabClient`] if you want the idiomatic Rust surface.
///
/// Must be called from within a `tokio::task::LocalSet`-driven
/// context — capnp-rpc futures are `!Send`.
pub fn try_connect() -> Result<CollabClient, NotAvailable> {
    let fd = service_fd("collab").ok_or(NotAvailable::ServiceFdMissing)?;
    let unix = UnixCapnpClient::from_connected_fd(fd, rpc_twoparty_capnp::Side::Client)?;
    let client: CollabServiceClient = unix.bootstrap();
    Ok(CollabClient::from_raw(client))
}

/// Idiomatic Rust wrapper over the generated [`CollabServiceClient`].
#[derive(Clone)]
pub struct CollabClient {
    inner: Rc<CollabServiceClient>,
}

impl CollabClient {
    /// Wrap an existing generated client (mainly useful for tests
    /// that own their own RPC system).
    pub fn from_raw(client: CollabServiceClient) -> Self {
        Self { inner: Rc::new(client) }
    }

    /// Join (or implicitly create) a channel by id, presenting the
    /// given opaque identity blob. `identity` is the postcard
    /// encoding of a `collab-types` identity record — opaque
    /// from the broker's perspective.
    pub async fn join_channel(
        &self,
        channel_id: &str,
        identity: &[u8],
    ) -> Result<ChannelHandle, NotAvailable> {
        let mut req = self.inner.join_channel_request();
        {
            let mut builder = req.get();
            builder.set_channel_id(channel_id);
            builder.set_identity(identity);
        }
        let reply = req.send().promise.await?;
        let handle = reply.get()?.get_handle()?;
        Ok(ChannelHandle { inner: Rc::new(handle) })
    }
}

/// A subscribed-to channel. Holds a typed capability backed by the
/// broker; dropping the handle closes the subscription. Cheap to
/// clone (the underlying capability is reference-counted).
#[derive(Clone)]
pub struct ChannelHandle {
    inner: Rc<collab_capnp::channel_handle::Client>,
}

impl ChannelHandle {
    /// Submit a single signed op into the channel. The op blob is
    /// a postcard-encoded `collab-types::SignedOp`.
    pub async fn send(&self, op: &[u8]) -> Result<(), NotAvailable> {
        let mut req = self.inner.send_request();
        req.get().set_op(op);
        req.send().promise.await?;
        Ok(())
    }

    /// LEGACY — publish an unsigned full-document snapshot. Present
    /// so apps that have not yet been provisioned with a signing
    /// identity keep working across the ui-collab → runtime-collab
    /// migration; will be removed once identity provisioning covers
    /// the whole fleet. Prefer [`Self::send`] with a signed op.
    pub async fn send_unsigned_snapshot(&self, data: &[u8]) -> Result<(), NotAvailable> {
        let mut req = self.inner.send_unsigned_snapshot_request();
        req.get().set_data(data);
        req.send().promise.await?;
        Ok(())
    }

    /// LEGACY — publish an unsigned incremental Automerge change
    /// blob. Same transitional contract as
    /// [`Self::send_unsigned_snapshot`].
    pub async fn send_unsigned_incremental(&self, data: &[u8]) -> Result<(), NotAvailable> {
        let mut req = self.inner.send_unsigned_incremental_request();
        req.get().set_data(data);
        req.send().promise.await?;
        Ok(())
    }

    /// Open a server-pulled stream of ops. The returned
    /// [`OpStream`] yields one op per `read` call; the consumer is
    /// expected to drive it in a loop. Stream lifetime is bounded
    /// by the parent channel handle.
    pub async fn subscribe(&self) -> Result<OpStream, NotAvailable> {
        let req = self.inner.subscribe_request();
        let reply = req.send().promise.await?;
        let stream = reply.get()?.get_stream()?;
        Ok(OpStream { inner: Rc::new(stream) })
    }

    /// Open the channel's presence sub-handle.
    pub async fn presence(&self) -> Result<PresenceHandle, NotAvailable> {
        let req = self.inner.presence_request();
        let reply = req.send().promise.await?;
        let handle = reply.get()?.get_handle()?;
        Ok(PresenceHandle { inner: Rc::new(handle) })
    }

    /// Subscribe to roster + permission updates. The broker emits a
    /// fresh [`ChannelContext`] whenever its view changes; the
    /// stream is client-pulled like every other stream in this
    /// schema.
    pub async fn subscribe_context(&self) -> Result<ContextStream, NotAvailable> {
        let req = self.inner.subscribe_context_request();
        let reply = req.send().promise.await?;
        let stream = reply.get()?.get_stream()?;
        Ok(ContextStream { inner: Rc::new(stream) })
    }

    /// Hand the broker content this app has just stored.
    ///
    /// A document holds a reference to its binaries and never the
    /// bytes, so this is the only way the content behind a reference
    /// leaves the process that produced it. An app's blob store lives
    /// inside its sandbox — every app gets a private `HOME` and
    /// `XDG_CACHE_HOME` — so the shell cannot go and read it, and
    /// there must not be a directory where it could.
    pub async fn offer_blob(&self, hash: &str, bytes: &[u8]) -> Result<(), NotAvailable> {
        let mut req = self.inner.offer_blob_request();
        {
            let mut b = req.get();
            b.set_hash(hash);
            b.set_bytes(bytes);
        }
        req.send().promise.await?;
        Ok(())
    }

    /// Ask for content a document here references and this app lacks.
    ///
    /// Returns as soon as the broker has the request, not when the
    /// content lands: the answer arrives on [`Self::subscribe_blobs`].
    /// A caller that blocked on the bytes would be blocking a frame on
    /// a transfer that may take several round trips.
    pub async fn want_blob(&self, hash: &str) -> Result<(), NotAvailable> {
        let mut req = self.inner.want_blob_request();
        req.get().set_hash(hash);
        req.send().promise.await?;
        Ok(())
    }

    /// Open a server-pulled stream of arrived content — answers to
    /// [`Self::want_blob`], and content a peer offered the room.
    pub async fn subscribe_blobs(&self) -> Result<BlobStream, NotAvailable> {
        let req = self.inner.subscribe_blobs_request();
        let reply = req.send().promise.await?;
        let stream = reply.get()?.get_stream()?;
        Ok(BlobStream { inner: Rc::new(stream) })
    }

    /// Fetch a page of previously-validated signed ops with
    /// `seq < before_seq`. See [`ScrollbackBatch`] for the response
    /// shape.
    ///
    /// LEGACY — this method does not carry an artifact id and the
    /// broker keys the read on `ArtifactId::default()`. Prefer
    /// [`Self::fetch_scrollback_for`] when a specific artifact is
    /// available.
    pub async fn fetch_scrollback(
        &self,
        before_seq: u64,
        max: u32,
    ) -> Result<ScrollbackBatch, NotAvailable> {
        let mut req = self.inner.fetch_scrollback_request();
        {
            let mut b = req.get();
            b.set_before_seq(before_seq);
            b.set_max(max);
        }
        let reply = req.send().promise.await?;
        let batch = reply.get()?.get_batch()?;
        let ops_reader = batch.get_ops()?;
        let mut ops = Vec::with_capacity(ops_reader.len() as usize);
        for i in 0..ops_reader.len() {
            ops.push(ops_reader.get(i)?.to_vec());
        }
        Ok(ScrollbackBatch {
            ops,
            earliest_seq: batch.get_earliest_seq(),
            latest_seq: batch.get_latest_seq(),
            done: batch.get_done(),
        })
    }

    /// Fetch a page of previously-validated signed ops on a specific
    /// artifact. `artifact_id` is the 16-byte little-endian encoding
    /// of the artifact uuid used by `collab-types`.
    pub async fn fetch_scrollback_for(
        &self,
        artifact_id: &[u8],
        before_seq: u64,
        max: u32,
    ) -> Result<ScrollbackBatch, NotAvailable> {
        let mut req = self.inner.fetch_scrollback_for_request();
        {
            let mut b = req.get();
            b.set_artifact_id(artifact_id);
            b.set_before_seq(before_seq);
            b.set_max(max);
        }
        let reply = req.send().promise.await?;
        let batch = reply.get()?.get_batch()?;
        let ops_reader = batch.get_ops()?;
        let mut ops = Vec::with_capacity(ops_reader.len() as usize);
        for i in 0..ops_reader.len() {
            ops.push(ops_reader.get(i)?.to_vec());
        }
        Ok(ScrollbackBatch {
            ops,
            earliest_seq: batch.get_earliest_seq(),
            latest_seq: batch.get_latest_seq(),
            done: batch.get_done(),
        })
    }
}

/// What an [`OpEvent`]'s bytes are.
///
/// Three kinds of frame share the op stream and they are not
/// interchangeable: a delta is applied on top of the document the
/// reader holds, a whole document is merged into it, and a signed
/// envelope is decoded before either. A reader that assumes one kind
/// discards the others without saying so.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OpKind {
    /// `op` is a postcard-encoded `SignedOp`.
    #[default]
    Signed,
    /// `op` is a raw Automerge change delta.
    UnsignedIncremental,
    /// `op` is a whole Automerge document.
    UnsignedSnapshot,
}

/// A single op as delivered by [`OpStream::read`]. `kind` says what
/// `op` is; `from_user_id` is the broker's attribution string for the
/// author. Duplicating the id on the RPC lets non-Rust clients cheaply
/// route by author without decoding the payload.
#[derive(Debug, Clone)]
pub struct OpEvent {
    pub op: Vec<u8>,
    pub from_user_id: String,
    pub kind: OpKind,
}

/// Client-pulled op stream. Each `.read()` is a single RPC.
#[derive(Clone)]
pub struct OpStream {
    inner: Rc<StreamClient>,
}

impl OpStream {
    /// Pull the next op along with the broker's attribution string
    /// for its author. Resolves when the broker has one available.
    pub async fn read(&self) -> Result<OpEvent, NotAvailable> {
        let req = self.inner.read_request();
        let reply = req.send().promise.await?;
        let r = reply.get()?;
        let op = r.get_op()?.to_vec();
        let from_user_id = r
            .get_from_user_id()?
            .to_str()
            .map_err(|e| capnp::Error::failed(format!("invalid utf8 from_user_id: {e}")))?
            .to_owned();
        // An unrecognised ordinal comes from a newer broker naming a
        // kind this build has no handler for. Reading it as `Signed`
        // would hand raw bytes to the envelope decoder; leaving it
        // unmapped and letting the caller ignore it is the honest
        // direction, so it decodes to the default and the caller's
        // match arms decide.
        let kind = match r.get_kind() {
            Ok(collab_capnp::OpKind::Signed) => OpKind::Signed,
            Ok(collab_capnp::OpKind::UnsignedIncremental) => OpKind::UnsignedIncremental,
            Ok(collab_capnp::OpKind::UnsignedSnapshot) => OpKind::UnsignedSnapshot,
            Err(_) => OpKind::Signed,
        };
        Ok(OpEvent {
            op,
            from_user_id,
            kind,
        })
    }
}

/// One piece of content off a [`BlobStream`].
#[derive(Debug, Clone)]
pub struct BlobEvent {
    /// blake3 hex the content is addressed by, in every store on every
    /// machine.
    pub hash: String,
    /// The content itself.
    pub bytes: Vec<u8>,
}

/// Client-pulled stream of arrived document content. Each `.read()` is
/// a single RPC, like every other stream in this schema.
#[derive(Clone)]
pub struct BlobStream {
    inner: Rc<BlobStreamClient>,
}

impl BlobStream {
    /// Pull the next piece of arrived content.
    ///
    /// The caller MUST verify the bytes against `hash` before storing
    /// them. The key is the content's identity, so bytes that hash to
    /// something else are not the content that was asked for.
    pub async fn read(&self) -> Result<BlobEvent, NotAvailable> {
        let req = self.inner.read_request();
        let reply = req.send().promise.await?;
        let r = reply.get()?;
        let hash = r
            .get_hash()?
            .to_str()
            .map_err(|e| capnp::Error::failed(format!("invalid utf8 blob hash: {e}")))?
            .to_owned();
        let bytes = r.get_bytes()?.to_vec();
        Ok(BlobEvent { hash, bytes })
    }
}

/// Per-channel presence handle.
#[derive(Clone)]
pub struct PresenceHandle {
    inner: Rc<collab_capnp::presence_handle::Client>,
}

impl PresenceHandle {
    /// Publish a cursor position. The broker fan-outs to peers.
    pub async fn publish_cursor(&self, x: f32, y: f32) -> Result<(), NotAvailable> {
        let mut req = self.inner.publish_cursor_request();
        {
            let mut b = req.get();
            b.set_x(x);
            b.set_y(y);
        }
        req.send().promise.await?;
        Ok(())
    }

    /// Publish the local pointer at `(x, y)` in the coordinate space
    /// `key_bytes` names.
    ///
    /// [`Self::publish_cursor`] with the space kept. `key_bytes` is a
    /// postcard-encoded `ui::PresenceKey` — for a pointer, a
    /// `PresenceKey::Pointer` carrying a `PointerSpace`. The broker
    /// relays those bytes untouched, so the receiver reads the space
    /// its publisher chose rather than one synthesised on the way.
    pub async fn publish_pointer(
        &self,
        key_bytes: &[u8],
        x: f32,
        y: f32,
    ) -> Result<(), NotAvailable> {
        let mut req = self.inner.publish_pointer_request();
        {
            let mut b = req.get();
            b.set_key_bytes(key_bytes);
            b.set_x(x);
            b.set_y(y);
        }
        req.send().promise.await?;
        Ok(())
    }

    /// Publish the local caret position on the artifact identified
    /// by `key_bytes`. `key_bytes` is a postcard-encoded
    /// `ui::PresenceKey`; the broker treats it as an opaque routing
    /// tag.
    pub async fn publish_caret(&self, key_bytes: &[u8], pos: u64) -> Result<(), NotAvailable> {
        let mut req = self.inner.publish_caret_request();
        {
            let mut b = req.get();
            b.set_key_bytes(key_bytes);
            b.set_pos(pos);
        }
        req.send().promise.await?;
        Ok(())
    }

    /// Publish a selection range on the keyed artifact. `start ==
    /// end` MAY be used to represent a caret; either publish path
    /// is accepted.
    pub async fn publish_selection(
        &self,
        key_bytes: &[u8],
        start: u64,
        end: u64,
    ) -> Result<(), NotAvailable> {
        let mut req = self.inner.publish_selection_request();
        {
            let mut b = req.get();
            b.set_key_bytes(key_bytes);
            b.set_start(start);
            b.set_end(end);
        }
        req.send().promise.await?;
        Ok(())
    }

    /// Publish "the local user has arrived at the keyed object".
    pub async fn publish_arrived(&self, key_bytes: &[u8]) -> Result<(), NotAvailable> {
        let mut req = self.inner.publish_arrived_request();
        req.get().set_key_bytes(key_bytes);
        req.send().promise.await?;
        Ok(())
    }

    /// Publish "the local user has left the keyed object".
    pub async fn publish_left(&self, key_bytes: &[u8]) -> Result<(), NotAvailable> {
        let mut req = self.inner.publish_left_request();
        req.get().set_key_bytes(key_bytes);
        req.send().promise.await?;
        Ok(())
    }

    /// LEGACY — subscribe to only remote-cursor updates. Kept so
    /// clients built against schema v1 keep compiling; new code
    /// uses [`Self::subscribe_all`].
    pub async fn subscribe_remote_cursors(&self) -> Result<CursorStream, NotAvailable> {
        let req = self.inner.subscribe_remote_cursors_request();
        let reply = req.send().promise.await?;
        let stream = reply.get()?.get_stream()?;
        Ok(CursorStream { inner: Rc::new(stream) })
    }

    /// Subscribe to every kind of remote presence update in one
    /// stream. Each event carries the full [`PresencePayload`]
    /// union; clients discriminate on payload variant.
    pub async fn subscribe_all(&self) -> Result<PresenceStream, NotAvailable> {
        let req = self.inner.subscribe_all_request();
        let reply = req.send().promise.await?;
        let stream = reply.get()?.get_stream()?;
        Ok(PresenceStream { inner: Rc::new(stream) })
    }
}

/// A single remote-cursor update.
#[derive(Debug, Clone)]
pub struct CursorUpdate {
    pub user_id: String,
    pub x: f32,
    pub y: f32,
}

/// Client-pulled remote-cursor stream.
#[derive(Clone)]
pub struct CursorStream {
    inner: Rc<CursorStreamClient>,
}

impl CursorStream {
    /// Pull the next remote-cursor update.
    pub async fn read(&self) -> Result<CursorUpdate, NotAvailable> {
        let req = self.inner.read_request();
        let reply = req.send().promise.await?;
        let r = reply.get()?;
        let user_id = r
            .get_user_id()?
            .to_str()
            .map_err(|e| capnp::Error::failed(format!("invalid utf8 user_id: {e}")))?
            .to_owned();
        Ok(CursorUpdate {
            user_id,
            x: r.get_x(),
            y: r.get_y(),
        })
    }
}

/// Rust mirror of the capnp `PresencePayload` union. Kept as a
/// typed enum here so app code doesn't have to touch the generated
/// Reader / Builder types.
#[derive(Debug, Clone, PartialEq)]
pub enum PresencePayload {
    /// A pointer published without a coordinate space — the legacy
    /// `publishCursor` shape. The event's `key_bytes` say nothing
    /// about where this position is measured, so a reader must not
    /// invent a space for it.
    Cursor { x: f32, y: f32 },
    /// A pointer published *with* its space, by `publish_pointer`.
    /// The event's `key_bytes` decode to the `ui::PresenceKey` the
    /// publisher chose and are authoritative.
    Pointer { x: f32, y: f32 },
    Caret { pos: u64 },
    Selection { start: u64, end: u64 },
    Arrived,
    Left,
}

/// A single remote presence update. `user_id` is the broker's
/// attribution string, `key_bytes` is the postcard of a
/// `ui::PresenceKey` (opaque to the SDK), `payload` discriminates
/// the variant.
#[derive(Debug, Clone)]
pub struct PresenceEvent {
    pub user_id: String,
    pub key_bytes: Vec<u8>,
    pub payload: PresencePayload,
}

/// Client-pulled unified presence stream.
#[derive(Clone)]
pub struct PresenceStream {
    inner: Rc<PresenceStreamClient>,
}

impl PresenceStream {
    /// Pull the next presence event across all payload kinds.
    pub async fn read(&self) -> Result<PresenceEvent, NotAvailable> {
        let req = self.inner.read_request();
        let reply = req.send().promise.await?;
        let r = reply.get()?;
        let evt = r.get_event()?;
        let user_id = evt
            .get_user_id()?
            .to_str()
            .map_err(|e| capnp::Error::failed(format!("invalid utf8 user_id: {e}")))?
            .to_owned();
        let key_bytes = evt.get_key_bytes()?.to_vec();
        let payload = decode_presence_payload(evt.get_payload()?)?;
        Ok(PresenceEvent {
            user_id,
            key_bytes,
            payload,
        })
    }
}

fn decode_presence_payload(
    reader: collab_capnp::presence_payload::Reader<'_>,
) -> Result<PresencePayload, capnp::Error> {
    use collab_capnp::presence_payload::Which as W;
    Ok(match reader.which()? {
        W::Cursor(r) => PresencePayload::Cursor {
            x: r.get_x(),
            y: r.get_y(),
        },
        W::Pointer(r) => PresencePayload::Pointer {
            x: r.get_x(),
            y: r.get_y(),
        },
        W::Caret(r) => PresencePayload::Caret { pos: r.get_pos() },
        W::Selection(r) => PresencePayload::Selection {
            start: r.get_start(),
            end: r.get_end(),
        },
        W::Arrived(()) => PresencePayload::Arrived,
        W::Left(()) => PresencePayload::Left,
    })
}

/// A single collaborator in a [`ChannelContext`].
#[derive(Debug, Clone)]
pub struct Collaborator {
    pub user_id: String,
    pub display_name: String,
    /// Packed as `0x00RRGGBB`; the top byte is reserved and MUST be
    /// zero on the wire. Consumers unpack into their native colour
    /// space.
    pub color: u32,
}

/// Roster + local permissions snapshot delivered on
/// [`ContextStream`]. See the capnp docs for the exact semantics.
#[derive(Debug, Clone)]
pub struct ChannelContext {
    pub collaborators: Vec<Collaborator>,
    pub permissions: Vec<String>,
}

/// Client-pulled channel-context stream.
#[derive(Clone)]
pub struct ContextStream {
    inner: Rc<ContextStreamClient>,
}

impl ContextStream {
    /// Pull the next roster / permission snapshot.
    pub async fn read(&self) -> Result<ChannelContext, NotAvailable> {
        let req = self.inner.read_request();
        let reply = req.send().promise.await?;
        let r = reply.get()?;
        let ctx = r.get_ctx()?;
        let collabs_reader = ctx.get_collaborators()?;
        let mut collaborators = Vec::with_capacity(collabs_reader.len() as usize);
        for i in 0..collabs_reader.len() {
            let c = collabs_reader.get(i);
            let user_id = c
                .get_user_id()?
                .to_str()
                .map_err(|e| capnp::Error::failed(format!("invalid utf8 user_id: {e}")))?
                .to_owned();
            let display_name = c
                .get_display_name()?
                .to_str()
                .map_err(|e| capnp::Error::failed(format!("invalid utf8 display_name: {e}")))?
                .to_owned();
            collaborators.push(Collaborator {
                user_id,
                display_name,
                color: c.get_color(),
            });
        }
        let perms_reader = ctx.get_permissions()?;
        let mut permissions = Vec::with_capacity(perms_reader.len() as usize);
        for i in 0..perms_reader.len() {
            permissions.push(
                perms_reader
                    .get(i)?
                    .to_str()
                    .map_err(|e| capnp::Error::failed(format!("invalid utf8 permission: {e}")))?
                    .to_owned(),
            );
        }
        Ok(ChannelContext {
            collaborators,
            permissions,
        })
    }
}

/// A page of previously-validated signed ops returned by
/// [`ChannelHandle::fetch_scrollback`].
#[derive(Debug, Clone)]
pub struct ScrollbackBatch {
    /// Postcard-encoded `SignedOp` blobs in ascending `seq` order.
    pub ops: Vec<Vec<u8>>,
    /// Lowest `seq` in the page (inclusive). Zero when `ops` is
    /// empty.
    pub earliest_seq: u64,
    /// Highest `seq` in the page (inclusive). Zero when `ops` is
    /// empty.
    pub latest_seq: u64,
    /// True iff the broker has no further in-memory history in the
    /// requested range.
    pub done: bool,
}

/// Re-export the raw generated module for power users that need to
/// reach below the idiomatic wrappers (server impls, tests).
pub mod raw {
    pub use super::collab_capnp::*;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn clear_qos_env() {
        for (key, _) in env::vars() {
            if key.starts_with("QOS_") {
                unsafe { env::remove_var(&key) };
            }
        }
    }

    #[test]
    fn try_connect_without_env_returns_service_fd_missing() {
        clear_qos_env();
        // Not using `expect_err` because `CollabClient` intentionally
        // doesn't implement `Debug` (the underlying capnp client hook
        // isn't `Debug`).
        match try_connect() {
            Err(NotAvailable::ServiceFdMissing) => {}
            Err(other) => panic!("unexpected error: {other:?}"),
            Ok(_) => panic!("expected ServiceFdMissing"),
        }
    }
}
