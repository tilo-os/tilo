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

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]
#![allow(
    clippy::future_not_send,
    reason = "every future here wraps an Rc-held capnp-rpc capability; capnp-rpc futures \
              are inherently !Send and every call is documented to require a \
              tokio::task::LocalSet-driven context, so this is not fixable without \
              redesigning around the RPC library"
)]

use std::rc::Rc;

use runtime_portal::{NotAvailable, UnixCapnpClient, service_fd};

// Generated schema lands in OUT_DIR as `<schema>_capnp.rs`, which
// references its own module path absolutely as `crate::collab_capnp`
// — must be included at the crate root with that exact name (the
// same pattern capnp's own samples use).
#[allow(clippy::all)]
#[allow(unused)]
#[allow(dead_code)]
pub mod collab_capnp {
    include!(concat!(env!("OUT_DIR"), "/collab_capnp.rs"));
}

use capnp_rpc::rpc_twoparty_capnp;

pub use collab_capnp::action_stream::Client as ActionStreamClient;
pub use collab_capnp::blob_stream::Client as BlobStreamClient;
pub use collab_capnp::collab_service::Client as CollabServiceClient;
pub use collab_capnp::context_stream::Client as ContextStreamClient;
pub use collab_capnp::invocation_stream::Client as InvocationStreamClient;
pub use collab_capnp::presence_stream::Client as PresenceStreamClient;
pub use collab_capnp::stream::Client as StreamClient;

/// Try to dial the qOS collab broker via the launcher-supplied service
/// fd. Returns [`NotAvailable`] when the env var is unset (likely: app
/// running as a Flatpak on stock Linux) or the fd is no longer open.
///
/// The returned client is the typed root capability; wrap it in a
/// [`CollabClient`] for the idiomatic Rust surface. Must be called from
/// within a `tokio::task::LocalSet`-driven context — capnp-rpc futures
/// are `!Send`.
///
/// # Errors
///
/// Returns [`NotAvailable::ServiceFdMissing`] if the launcher didn't hand
/// this process a collab service fd, or another [`NotAvailable`] variant
/// if the connection fails to establish.
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
        Self {
            inner: Rc::new(client),
        }
    }

    /// Join (or implicitly create) a channel by id, presenting the
    /// given opaque identity blob. `identity` is the postcard
    /// encoding of a `collab-types` identity record — opaque
    /// from the broker's perspective.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn join_channel(
        &self,
        channel_id: &str,
        identity: &[u8],
    ) -> Result<ChannelHandle, NotAvailable> {
        self.join_channel_as(channel_id, identity, "").await
    }

    /// Join a channel while announcing the participant's display handle.
    /// Empty handles retain the broker's room/local-identity fallback.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn join_channel_as(
        &self,
        channel_id: &str,
        identity: &[u8],
        participant_handle: &str,
    ) -> Result<ChannelHandle, NotAvailable> {
        let mut req = self.inner.join_channel_request();
        {
            let mut builder = req.get();
            builder.set_channel_id(channel_id);
            builder.set_identity(identity);
            builder.set_participant_handle(participant_handle);
        }
        let reply = req.send().promise.await?;
        let handle = reply.get()?.get_handle()?;
        Ok(ChannelHandle {
            inner: Rc::new(handle),
        })
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
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn send(&self, op: &[u8]) -> Result<(), NotAvailable> {
        let mut req = self.inner.send_request();
        req.get().set_op(op);
        req.send().promise.await?;
        Ok(())
    }

    /// LEGACY — publish an unsigned full-document snapshot, for apps not yet
    /// provisioned with a signing identity (removed once the whole fleet is
    /// provisioned). Prefer [`Self::send`] with a signed op.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn send_unsigned_snapshot(&self, data: &[u8]) -> Result<(), NotAvailable> {
        let mut req = self.inner.send_unsigned_snapshot_request();
        req.get().set_data(data);
        req.send().promise.await?;
        Ok(())
    }

    /// LEGACY — publish an unsigned incremental Automerge change
    /// blob. Same transitional contract as
    /// [`Self::send_unsigned_snapshot`].
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
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
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn subscribe(&self) -> Result<OpStream, NotAvailable> {
        let req = self.inner.subscribe_request();
        let reply = req.send().promise.await?;
        let stream = reply.get()?.get_stream()?;
        Ok(OpStream {
            inner: Rc::new(stream),
        })
    }

    /// Open the channel's presence sub-handle.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn presence(&self) -> Result<PresenceHandle, NotAvailable> {
        let req = self.inner.presence_request();
        let reply = req.send().promise.await?;
        let handle = reply.get()?.get_handle()?;
        Ok(PresenceHandle {
            inner: Rc::new(handle),
        })
    }

    /// Subscribe to roster + permission updates. The broker emits a
    /// fresh [`ChannelContext`] whenever its view changes; the
    /// stream is client-pulled like every other stream in this
    /// schema.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn subscribe_context(&self) -> Result<ContextStream, NotAvailable> {
        let req = self.inner.subscribe_context_request();
        let reply = req.send().promise.await?;
        let stream = reply.get()?.get_stream()?;
        Ok(ContextStream {
            inner: Rc::new(stream),
        })
    }

    /// Hand the broker content this app has just stored.
    ///
    /// A document holds a reference to its binaries, never the bytes, so
    /// this is the only way the content behind a reference leaves the
    /// process that produced it — an app's blob store lives inside its
    /// sandbox, so the shell cannot go and read it directly.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
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
    /// Returns as soon as the broker has the request, not when the content
    /// lands — the answer arrives on [`Self::subscribe_blobs`], so a caller
    /// doesn't block a frame on a transfer that may take several round trips.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn want_blob(&self, hash: &str) -> Result<(), NotAvailable> {
        let mut req = self.inner.want_blob_request();
        req.get().set_hash(hash);
        req.send().promise.await?;
        Ok(())
    }

    /// Open a server-pulled stream of arrived content — answers to
    /// [`Self::want_blob`], and content a peer offered the room.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn subscribe_blobs(&self) -> Result<BlobStream, NotAvailable> {
        let req = self.inner.subscribe_blobs_request();
        let reply = req.send().promise.await?;
        let stream = reply.get()?.get_stream()?;
        Ok(BlobStream {
            inner: Rc::new(stream),
        })
    }

    /// Publish a typed action into the room, for whichever app owns the
    /// document it belongs to.
    ///
    /// Every other send verb carries Automerge bytes, which an app can only
    /// produce for a document it holds. This one carries an action, so an
    /// app can address a document it has no replica of and no schema for —
    /// it says what should happen, and the owning app decides whether it
    /// recognises the request. Fanned out at cluster scope; a receiver
    /// applies it only if its own registry knows `type_id`.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn send_action(&self, type_id: &str, payload: &[u8]) -> Result<(), NotAvailable> {
        let mut req = self.inner.send_action_request();
        {
            let mut b = req.get();
            b.set_type_id(type_id);
            b.set_payload(payload);
        }
        req.send().promise.await?;
        Ok(())
    }

    /// Open a server-pulled stream of actions other apps published into
    /// this room.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn subscribe_actions(&self) -> Result<ActionStream, NotAvailable> {
        let req = self.inner.subscribe_actions_request();
        let reply = req.send().promise.await?;
        let stream = reply.get()?.get_stream()?;
        Ok(ActionStream {
            inner: Rc::new(stream),
        })
    }

    /// Tell this session what this application currently has open and what
    /// may be done to it.
    ///
    /// Level-triggered: each call replaces this connection's whole published
    /// surface, so a closed resource stops being addressable rather than
    /// lingering. What crosses is inert — an [`ExportedAction`] carries ids
    /// and labels; the closure an `ActionSpec` holds never leaves the
    /// process that built it, so nothing published here can be fired by
    /// whoever reads it, only requested back through
    /// [`Self::subscribe_invocations`].
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call, or if `actions` is longer than a capnp list
    /// can address (`u32::MAX` entries).
    pub async fn publish_actions(
        &self,
        app_id: &str,
        actions: &[ExportedAction],
    ) -> Result<(), NotAvailable> {
        let mut req = self.inner.publish_actions_request();
        {
            let mut b = req.get();
            b.set_app_id(app_id);
            let action_count = u32::try_from(actions.len())
                .map_err(|_| capnp::Error::failed("too many actions to publish".to_string()))?;
            let mut list = b.init_actions(action_count);
            for (i, a) in actions.iter().enumerate() {
                // `action_count` above already proved `actions.len()` fits in
                // u32, so every index here does too.
                let idx = u32::try_from(i).unwrap_or(u32::MAX);
                let mut row = list.reborrow().get(idx);
                row.set_resource(&a.resource);
                row.set_action(&a.action);
                row.set_resource_kind(&a.resource_kind);
                row.set_resource_label(&a.resource_label);
                row.set_action_label(&a.action_label);
                row.set_description(a.description.as_deref().unwrap_or(""));
                let mut input = row.init_input();
                match &a.input {
                    ExportedInput::None => input.set_none(()),
                    ExportedInput::Text {
                        prompt,
                        placeholder,
                        wants_content,
                    } => {
                        let mut t = input.init_text();
                        t.set_prompt(prompt);
                        t.set_placeholder(placeholder);
                        t.set_wants_content(*wants_content);
                    }
                }
            }
        }
        req.send().promise.await?;
        Ok(())
    }

    /// Open a server-pulled stream of invocations the session has addressed
    /// to *this* application — distinct from [`Self::subscribe_actions`]
    /// (room-wide fan-out): an invocation names one connection, an
    /// instruction to one app to perform one operation it published.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn subscribe_invocations(&self) -> Result<InvocationStream, NotAvailable> {
        let req = self.inner.subscribe_invocations_request();
        let reply = req.send().promise.await?;
        let stream = reply.get()?.get_stream()?;
        Ok(InvocationStream {
            inner: Rc::new(stream),
        })
    }

    /// Report what became of an invocation. An empty `code` means it ran;
    /// anything else is a refusal, reported to whoever asked rather than
    /// swallowed — a caller that can't see the screen has no other way to
    /// tell a completed operation from one that quietly did nothing.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn complete_invocation(
        &self,
        id: u64,
        code: &str,
        message: &str,
    ) -> Result<(), NotAvailable> {
        let mut req = self.inner.complete_invocation_request();
        {
            let mut b = req.get();
            b.set_id(id);
            b.set_code(code);
            b.set_message(message);
        }
        req.send().promise.await?;
        Ok(())
    }

    /// Ask this session to open the "say something about this" surface,
    /// addressed to the compositor on the far end of this socket, not the
    /// room. `reference` is a serialised `qos_reference::ResourceRef`
    /// (JSON); `beside` is `(x, y, w, h)` **in this window's own
    /// coordinates** — the session converts, since only it knows where this
    /// window sits on a desktop. The panel can't be drawn here: it opens
    /// beside the app's context menu, and a Wayland client has no way to
    /// paint outside its own window.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "beside carries on-screen window coordinates, always well within f32's \
                  range; the wire schema is f32 and only sub-pixel precision is lost"
    )]
    pub async fn compose_remark(
        &self,
        reference: &str,
        beside: (f64, f64, f64, f64),
    ) -> Result<(), NotAvailable> {
        let mut req = self.inner.compose_remark_request();
        {
            let mut b = req.get();
            b.set_reference(reference);
            b.set_x(beside.0 as f32);
            b.set_y(beside.1 as f32);
            b.set_w(beside.2 as f32);
            b.set_h(beside.3 as f32);
        }
        req.send().promise.await?;
        Ok(())
    }

    /// Fetch a page of previously-validated signed ops on a specific
    /// artifact. `artifact_id` is the 16-byte little-endian encoding
    /// of the artifact uuid used by `collab-types`.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
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
/// Three kinds of frame share the op stream and are not interchangeable:
/// a delta applies on top of the document the reader holds, a whole
/// document merges into it, a signed envelope decodes before either.
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

/// A single op as delivered by [`OpStream::read`]. `kind` says what `op`
/// is; `from_user_id` is the broker's attribution string for the author
/// — duplicated on the RPC so non-Rust clients can route by author
/// without decoding the payload.
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
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable,
    /// drops during the call, or the reply carries a `from_user_id` that
    /// isn't valid UTF-8.
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
        // An unrecognised ordinal comes from a newer broker naming a kind
        // this build has no handler for. Reading it as `Signed` would hand
        // raw bytes to the envelope decoder; leaving it unmapped and letting
        // the caller ignore it is the honest direction.
        let kind = match r.get_kind() {
            Ok(collab_capnp::OpKind::Signed) | Err(_) => OpKind::Signed,
            Ok(collab_capnp::OpKind::UnsignedIncremental) => OpKind::UnsignedIncremental,
            Ok(collab_capnp::OpKind::UnsignedSnapshot) => OpKind::UnsignedSnapshot,
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
    pub bytes: Vec<u8>,
}

/// Client-pulled stream of arrived document content. Each `.read()` is a
/// single RPC, like every other stream in this schema.
#[derive(Clone)]
pub struct BlobStream {
    inner: Rc<BlobStreamClient>,
}

impl BlobStream {
    /// Pull the next piece of arrived content. The caller MUST verify the
    /// bytes against `hash` before storing them — the key is the content's
    /// identity, so bytes hashing to something else aren't what was asked for.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable,
    /// drops during the call, or the reply carries a `hash` that isn't
    /// valid UTF-8.
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

/// One action off an [`ActionStream`].
#[derive(Debug, Clone)]
pub struct ActionEvent {
    /// The registry type id the action was encoded under. Names the app
    /// that owns the document it belongs to.
    pub type_id: String,
    /// The encoded action, exactly as `qos_actions::EncodedAction`
    /// carries it.
    pub payload: Vec<u8>,
    /// The broker's attribution for whoever sent the frame. Who *sent*
    /// it, which is a different claim from who the action says authored
    /// it.
    pub from_user_id: String,
}

/// Client-pulled stream of actions published into this room.
#[derive(Clone)]
pub struct ActionStream {
    inner: Rc<ActionStreamClient>,
}

impl ActionStream {
    /// Pull the next action published into this room.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable,
    /// drops during the call, or the reply carries a string field that
    /// isn't valid UTF-8.
    pub async fn read(&self) -> Result<ActionEvent, NotAvailable> {
        let req = self.inner.read_request();
        let reply = req.send().promise.await?;
        let r = reply.get()?;
        let type_id = r
            .get_type_id()?
            .to_str()
            .map_err(|e| capnp::Error::failed(format!("invalid utf8 action type id: {e}")))?
            .to_owned();
        let payload = r.get_payload()?.to_vec();
        let from_user_id = r
            .get_from_user_id()?
            .to_str()
            .map_err(|e| capnp::Error::failed(format!("invalid utf8 attribution: {e}")))?
            .to_owned();
        Ok(ActionEvent {
            type_id,
            payload,
            from_user_id,
        })
    }
}

/// What one exported action's invocation must carry, as it crosses the
/// wire — the transport-side twin of `qos_actions::door::InputSpec`.
/// Kept here rather than depending on `qos-actions` so this crate stays
/// a client for one socket; an app wanting the richer type converts at
/// its own edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportedInput {
    None,
    Text {
        prompt: String,
        placeholder: String,
        /// Whether that text names content already in this machine's
        /// store rather than words somebody says.
        wants_content: bool,
    },
}

/// One operation an application exports, as inert data.
///
/// Everything a caller needs to decide whether to perform it, and
/// nothing that could perform it — read-only is the shape of this
/// struct, not a promise about how it is used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedAction {
    pub resource: String,
    pub action: String,
    pub resource_kind: String,
    pub resource_label: String,
    pub action_label: String,
    pub description: Option<String>,
    pub input: ExportedInput,
}

/// One invocation off an [`InvocationStream`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// Echoed back on [`ChannelHandle::complete_invocation`], so the
    /// session can match an answer to the question it asked.
    pub id: u64,
    pub resource: String,
    pub action: String,
    /// `None` when the caller supplied none. Separate from the empty
    /// string so an action that legitimately takes `""` can still say it
    /// was given one.
    pub argument: Option<String>,
    /// Who asked, as the session resolved them. An attribution the
    /// resulting change commits under — not an authority; the session
    /// already decided this may happen.
    pub actor: String,
}

/// Client-pulled stream of invocations addressed to this application.
#[derive(Clone)]
pub struct InvocationStream {
    inner: Rc<InvocationStreamClient>,
}

impl InvocationStream {
    /// Pull the next invocation addressed to this application.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable,
    /// drops during the call, or the reply carries a string field that
    /// isn't valid UTF-8.
    pub async fn read(&self) -> Result<Invocation, NotAvailable> {
        let req = self.inner.read_request();
        let reply = req.send().promise.await?;
        let r = reply.get()?;
        let text = |t: capnp::text::Reader<'_>, what: &str| {
            t.to_str()
                .map(std::borrow::ToOwned::to_owned)
                .map_err(|e| capnp::Error::failed(format!("invalid utf8 {what}: {e}")))
        };
        let resource = text(r.get_resource()?, "invocation resource")?;
        let action = text(r.get_action()?, "invocation action")?;
        let actor = text(r.get_actor()?, "invocation actor")?;
        let argument = if r.get_has_argument() {
            Some(text(r.get_argument()?, "invocation argument")?)
        } else {
            None
        };
        Ok(Invocation {
            id: r.get_id(),
            resource,
            action,
            argument,
            actor,
        })
    }
}

/// Per-channel presence handle.
#[derive(Clone)]
pub struct PresenceHandle {
    inner: Rc<collab_capnp::presence_handle::Client>,
}

impl PresenceHandle {
    /// Publish a cursor position. The broker fan-outs to peers.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
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
    /// `key_bytes` names — [`Self::publish_cursor`] with the space kept.
    /// `key_bytes` is a postcard-encoded `ui::PresenceKey`
    /// (`PresenceKey::Pointer` for a pointer); the broker relays those bytes
    /// untouched, so the receiver reads the space its publisher chose.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
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
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
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
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
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
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn publish_arrived(&self, key_bytes: &[u8]) -> Result<(), NotAvailable> {
        let mut req = self.inner.publish_arrived_request();
        req.get().set_key_bytes(key_bytes);
        req.send().promise.await?;
        Ok(())
    }

    /// Publish "the local user has left the keyed object".
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn publish_left(&self, key_bytes: &[u8]) -> Result<(), NotAvailable> {
        let mut req = self.inner.publish_left_request();
        req.get().set_key_bytes(key_bytes);
        req.send().promise.await?;
        Ok(())
    }

    /// Subscribe to every kind of remote presence update in one
    /// stream. Each event carries the full [`PresencePayload`]
    /// union; clients discriminate on payload variant.
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable or
    /// drops during the call.
    pub async fn subscribe_all(&self) -> Result<PresenceStream, NotAvailable> {
        let req = self.inner.subscribe_all_request();
        let reply = req.send().promise.await?;
        let stream = reply.get()?.get_stream()?;
        Ok(PresenceStream {
            inner: Rc::new(stream),
        })
    }
}

/// Rust mirror of the capnp `PresencePayload` union, kept as a typed
/// enum so app code doesn't touch the generated Reader/Builder types.
#[derive(Debug, Clone, PartialEq)]
pub enum PresencePayload {
    /// A pointer published without a coordinate space — the legacy
    /// `publish_cursor` shape. `key_bytes` says nothing about where this
    /// position was measured; a reader must not invent a space for it.
    Cursor {
        x: f32,
        y: f32,
    },
    /// A pointer published *with* its space, by
    /// [`PresenceHandle::publish_pointer`]. The event's `key_bytes`
    /// decode to the `ui::PresenceKey` the publisher chose and are
    /// authoritative.
    Pointer {
        x: f32,
        y: f32,
    },
    Caret {
        pos: u64,
    },
    Selection {
        start: u64,
        end: u64,
    },
    Arrived,
    Left,
}

/// A single remote presence update: `user_id` is the broker's
/// attribution, `key_bytes` is the postcard of a `ui::PresenceKey`
/// (opaque to the SDK), `payload` discriminates the variant.
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
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable,
    /// drops during the call, the reply carries a `user_id` that isn't
    /// valid UTF-8, or the payload union's discriminant is unrecognised.
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
    /// Whether this member is an agent rather than a person, as the room
    /// recorded when it admitted them. Carried so no consumer has to
    /// read the id and guess.
    pub is_agent: bool,
    /// Whether this row is this machine's own participant. `user_id` is the
    /// address other machines use for a participant, and a machine never
    /// announces its own participant to its own applications — without
    /// this, an app had no way to find itself in the room's own roster.
    pub is_self: bool,
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
    ///
    /// # Errors
    ///
    /// Returns [`NotAvailable`] if the broker connection is unavailable,
    /// drops during the call, or the reply carries a string field that
    /// isn't valid UTF-8.
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
                is_agent: c.get_is_agent(),
                is_self: c.get_is_self(),
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
/// [`ChannelHandle::fetch_scrollback_for`].
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
