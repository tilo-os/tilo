//! Route the toolkit's co-edit protocol through the workspace host.
//!
//! The `ui` collaboration layer (`ui::collaboration`) speaks
//! `CollabRequest` / `CollabEvent` frames carrying Automerge deltas and
//! presence; the host speaks [`Envelope`]. [`RelayCoeditSession`] wraps a
//! [`RelayClient`] and exposes the co-edit verbs an app needs — publish
//! a document delta, publish presence, receive peer deltas/presence —
//! mapping each onto an [`Envelope`]. Because document deltas travel as
//! [`Envelope::Delta`] they inherit the host's fan-out **and** its
//! on-disk store-and-forward: two peers that are not simultaneously
//! online still converge on the shared document.
//!
//! # Correspondence with the toolkit protocol
//!
//! [`CoeditPayload`] is the self-describing body carried inside an
//! `Envelope::Delta`. The production shell uses retained checkpoints and
//! messages to carry its authenticated `NeighbourMsg` protocol through the
//! durable host. Apps do not publish raw document bytes here; they use the
//! certified local collaboration service, which signs and authorizes them
//! before the shell transports them.
//!
//! Keeping the mapping here rather than importing `ui` mirrors the
//! native shell collaboration service, which likewise translates the toolkit
//! protocol without pulling `ui`/wgpu into a transport crate.
//!
//! # Presence attribution
//!
//! [`Envelope::Delta`] carries an `author_device_public_key`, so a
//! document delta is attributed to its publisher for free. But
//! [`Envelope::Presence`] has **no** author slot — the host fans the
//! opaque bytes out without inspecting or stamping them. So presence
//! attribution has to ride *inside* the payload: [`RelayCoeditSession::publish_presence`]
//! wraps the toolkit body in an [`AttributedPresence`] carrying the
//! session's own device key, and [`RelayCoeditSession::recv`] unwraps it
//! into [`Inbound::Presence`]`{ author, payload }`. Without this a
//! remote cursor would arrive unauthored and could not be drawn against
//! the right collaborator.
//!
//! Document-op *authorisation* (a viewer without a write cap must not
//! mutate the doc) is enforced by the broker/host layer that validates
//! `SignedOp`s, not by this transport shim — the host stores and
//! forwards `Envelope::Delta` bytes verbatim, exactly as it does for
//! any workspace-directory delta.

use serde::{Deserialize, Serialize};

use crate::client::RelayClient;
use crate::transport::Envelope;
use tilo_delivery_protocol::clock::now_ns;

/// Body carried inside an [`Envelope::Delta`]. Self-describing so the
/// receiver can distinguish retained checkpoints from their incremental tail.
/// Retention metadata is deliberately visible to the host while the document
/// body remains opaque.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CoeditPayload {
    /// A complete state checkpoint for one logical document, allowing a
    /// device with no prior queue to bootstrap. What it replaces on the host
    /// is stated by `coverage`, never inferred.
    Checkpoint {
        /// Stable digest of the logical document route.
        channel: [u8; 32],
        /// Publisher-chosen identity of this message, the same on every host
        /// and for every receiver (see [`Self::RetainedDelta::entry`]).
        entry: [u8; 32],
        /// Which retained messages this checkpoint holds.
        coverage: Coverage,
        /// Opaque collaboration message.
        body: Vec<u8>,
    },
    /// An incremental update retained after the latest checkpoint for this
    /// channel. It is replayed to first-time subscribers as well as queued to
    /// already-known offline members.
    RetainedDelta {
        /// Stable digest of the logical document route.
        channel: [u8; 32],
        /// Publisher-chosen identity of this message. A later checkpoint
        /// names it in [`Coverage::Entries`] once its content is in that
        /// checkpoint. Opaque to the host, so it works on encrypted channels.
        entry: [u8; 32],
        /// Opaque collaboration message.
        body: Vec<u8>,
    },
}

/// Most entry ids one [`Coverage::Entries`] may name. A host rejects a
/// checkpoint naming more.
pub const MAX_COVERED_ENTRIES: usize = 65_536;

/// Which retained messages a [`CoeditPayload::Checkpoint`] holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Coverage {
    /// A whole-state baseline whose newest version wins (governance frames,
    /// continuity checkpoints): it replaces the retained base and its tail.
    Supersedes,
    /// A merge of the document whose content contains exactly these
    /// retained entries, as the publisher read from the bytes it saved. The
    /// host replaces its base only when the base is among them, then trims
    /// its tail from the front while each entry is among them.
    Entries(Vec<[u8; 32]>),
}

impl CoeditPayload {
    /// A checkpoint of `body` that names nothing it holds, identified by the
    /// digest of its body. It retires nothing by declaration; only a host's
    /// own causal proof may collect what it covers.
    pub fn checkpoint_of(channel: [u8; 32], body: Vec<u8>) -> Self {
        Self::Checkpoint {
            channel,
            entry: *blake3::hash(&body).as_bytes(),
            coverage: Coverage::Entries(Vec::new()),
            body,
        }
    }

    /// A retained delta of `body`, identified by the digest of its body.
    pub fn retained_of(channel: [u8; 32], body: Vec<u8>) -> Self {
        Self::RetainedDelta {
            channel,
            entry: *blake3::hash(&body).as_bytes(),
            body,
        }
    }

    /// Encode for carriage inside an `Envelope::Delta`.
    ///
    /// # Errors
    ///
    /// Returns an error if postcard fails to serialize `self`.
    pub fn encode(&self) -> Result<Vec<u8>, postcard::Error> {
        postcard::to_stdvec(self)
    }

    /// Decode a body previously produced by [`Self::encode`]. Returns
    /// `None` for bytes that are not a `CoeditPayload` (e.g. a plain
    /// workspace-directory delta from a non-co-edit peer).
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        postcard::from_bytes(bytes).ok()
    }
}

// Only the native relay peer retains coedit history.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RetentionKind {
    Checkpoint,
    Delta,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Deserialize)]
enum RetentionMetadata<'a> {
    Checkpoint {
        channel: [u8; 32],
        entry: [u8; 32],
        coverage: Coverage,
        #[serde(borrow)]
        body: &'a [u8],
    },
    RetainedDelta {
        channel: [u8; 32],
        entry: [u8; 32],
        #[serde(borrow)]
        body: &'a [u8],
    },
}

#[cfg(not(target_arch = "wasm32"))]
/// Decode only the host-visible retention route and kind. The opaque body is
/// consumed without allocating or exposing it to diagnostics.
pub(crate) fn retention_metadata(bytes: &[u8]) -> Option<([u8; 32], RetentionKind)> {
    let metadata: RetentionMetadata<'_> = postcard::from_bytes(bytes).ok()?;
    match metadata {
        RetentionMetadata::Checkpoint {
            channel,
            entry,
            coverage,
            body,
        } => {
            let _ = (entry, coverage, body);
            Some((channel, RetentionKind::Checkpoint))
        }
        RetentionMetadata::RetainedDelta {
            channel,
            entry,
            body,
        } => {
            let _ = (entry, body);
            Some((channel, RetentionKind::Delta))
        }
    }
}

/// Presence body wrapper that carries author attribution.
///
/// [`Envelope::Presence`] has no author field — the host forwards the
/// bytes verbatim. So the publisher stamps its own device public key
/// around the opaque toolkit presence `body`; the receiver recovers it
/// in [`Inbound::Presence`]. (Document deltas ride
/// `Envelope::Delta::author_device_public_key` instead and need no
/// wrapper.)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttributedPresence {
    /// Publishing device's public key.
    pub author: [u8; 32],
    /// Opaque toolkit presence body (the `(PresenceKey, PresencePayload)`
    /// pair the toolkit encodes).
    pub body: Vec<u8>,
}

impl AttributedPresence {
    /// Encode for carriage inside an `Envelope::Presence`.
    ///
    /// # Errors
    ///
    /// Returns an error if postcard fails to serialize `self`.
    pub fn encode(&self) -> Result<Vec<u8>, postcard::Error> {
        postcard::to_stdvec(self)
    }

    /// Decode a body previously produced by [`Self::encode`]. Returns
    /// `None` for presence bytes that are not an `AttributedPresence`
    /// (e.g. a non-co-edit presence frame from another tier).
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        postcard::from_bytes(bytes).ok()
    }
}

/// An inbound co-edit event surfaced from the host, ready to hand to
/// the toolkit as the matching `CollabEvent`.
#[derive(Debug, Clone)]
pub enum Inbound {
    /// A document delta (fanned out live, or flushed from the offline
    /// store-and-forward queue on reconnect). `author` is the device
    /// that published it.
    Delta {
        /// Publishing device's public key.
        author: [u8; 32],
        /// Raw Automerge incremental delta bytes.
        data: Vec<u8>,
    },
    /// An ephemeral presence payload fanned out by the host. Not
    /// persisted; dropped if no peer is subscribed.
    Presence {
        /// Publishing device's public key, recovered from the
        /// [`AttributedPresence`] wrapper. The host's presence envelope
        /// carries no author slot, so this is the only place the
        /// attribution survives — the toolkit maps it onto the
        /// `RemotePresenceUpdate` / `RemoteCursorMoved` `user_id`.
        author: [u8; 32],
        /// Opaque toolkit presence body (the `(PresenceKey,
        /// PresencePayload)` pair the toolkit encodes).
        payload: Vec<u8>,
    },
    /// A remote-action frame another device of the workspace addressed to
    /// this one, relayed because there was no direct route. The relay
    /// stamped the sender from its authenticated connection; the frame is
    /// the same bytes the direct route would have carried.
    RemoteAction {
        /// The sending device's public key.
        from: [u8; 32],
        /// The remote-action frame, as the sender encoded it.
        frame: Vec<u8>,
    },
    /// One document channel's retained state, answering
    /// [`RelayCoeditSession::request_channel_replay`]: `(author, body)` per
    /// retained message, checkpoint first. A large channel arrives in
    /// several of these, in order.
    Replayed {
        /// Digest of the document route.
        channel: [u8; 32],
        /// The retained messages, each with its publishing device.
        deltas: Vec<([u8; 32], Vec<u8>)>,
    },
    /// The host's reply to the initial subscribe: how many queued
    /// deltas it is about to flush from this device's offline queue.
    Ready {
        /// Number of store-and-forwarded deltas being flushed.
        pending: u32,
    },
    /// The host refused this device the workspace. The session carries
    /// nothing more; `reason` says why, for the log a person reads.
    Refused {
        /// Whether asking again unchanged can help.
        refusal: tilo_relay_protocol::transport::Refusal,
        /// The host's reason.
        reason: String,
    },
}

/// Failure modes for [`RelayCoeditSession`] operations.
#[derive(Debug)]
pub enum CoeditError {
    /// The underlying relay connection is gone (the client's outbound
    /// channel was closed). Mirrors the toolkit's fire-and-forget
    /// contract: the caller re-syncs on reconnect.
    Disconnected,
    /// The outgoing payload could not be postcard-encoded. In practice
    /// this does not happen for the owned, self-describing types this
    /// module encodes; surfaced rather than hidden so a future payload
    /// shape that can't serialize fails loudly instead of panicking.
    Encode(postcard::Error),
}

impl std::fmt::Display for CoeditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CoeditError::Disconnected => f.write_str("relay co-edit session disconnected"),
            CoeditError::Encode(error) => write!(f, "co-edit payload encode failed: {error}"),
        }
    }
}

impl std::error::Error for CoeditError {}

/// A co-edit session bound to one workspace on one host connection.
///
/// Construct it from an already-connected [`RelayClient`] via
/// [`RelayCoeditSession::join`]; then publish document deltas / presence
/// and drain inbound events with [`RelayCoeditSession::recv`].
pub struct RelayCoeditSession {
    client: RelayClient,
    workspace_id: u64,
    author: [u8; 32],
}

impl RelayCoeditSession {
    /// Subscribe `client` to `workspace_id` and wrap it as a co-edit
    /// session authored as `author` (the device public key). The host
    /// replies with a `Ready` (and flushes any queued deltas first);
    /// drain them with [`Self::recv`].
    ///
    /// # Errors
    ///
    /// Returns [`CoeditError::Disconnected`] if `client`'s outbound
    /// channel is already closed.
    pub fn join(
        client: RelayClient,
        workspace_id: u64,
        author: [u8; 32],
    ) -> Result<Self, CoeditError> {
        client
            .send(Envelope::Subscribe { workspace_id })
            .map_err(|_| CoeditError::Disconnected)?;
        Ok(Self {
            client,
            workspace_id,
            author,
        })
    }

    /// [`Self::join`], first redeeming the public room's standing invitation
    /// `invite_id` at this host, which the room's owner delegated admission
    /// to. The host admits this connection's own certified device, so the
    /// subscribe that follows on the same ordered stream is accepted. A
    /// refused redemption surfaces as the subscribe's `NotAuthorised`, which
    /// ends [`Self::recv`].
    ///
    /// # Errors
    ///
    /// Returns [`CoeditError::Disconnected`] if `client`'s outbound
    /// channel is already closed.
    pub fn join_redeeming(
        client: RelayClient,
        workspace_id: u64,
        author: [u8; 32],
        invite_id: [u8; 32],
    ) -> Result<Self, CoeditError> {
        client
            .send(Envelope::Redeem {
                workspace_id,
                invite_id,
            })
            .map_err(|_| CoeditError::Disconnected)?;
        Self::join(client, workspace_id, author)
    }

    /// Read support belongs to this exact negotiated connection.
    pub fn supports_agent_reads_v1(&self) -> bool {
        self.client.supports_agent_reads_v1()
    }

    /// Enable the private v2 lane on this same co-edit connection. Await its
    /// `ActionsReady` with `recv_action`; v1 sessions explicitly return an error.
    ///
    /// # Errors
    /// Returns an error for a different room, v1, or a full/disconnected queue.
    pub fn enable_actions(&self, room: uuid::Uuid) -> Result<(), crate::client::ClientError> {
        if !crate::room_workspace_id_matches(&room.to_string(), self.workspace_id) {
            return Err(crate::client::ClientError::Codec(
                "action room differs from co-edit room".into(),
            ));
        }
        self.client
            .send_action(tilo_delivery_protocol::transport_v2::EnvelopeV2::EnableActions { room })
    }

    /// Negotiate client access to an attached durable broker on this same lane.
    /// Call after `enable_actions`, which preserves subscribe-before-enable ordering.
    ///
    /// # Errors
    ///
    /// Returns `crate::client::ClientError` if the inputs violate this operation's bounds or invariants.
    pub fn enable_broker_client(&self, room: uuid::Uuid) -> Result<(), crate::client::ClientError> {
        if !crate::room_workspace_id_matches(&room.to_string(), self.workspace_id) {
            return Err(crate::client::ClientError::Codec(
                "action room differs from co-edit room".into(),
            ));
        }
        self.client.send_action(
            tilo_delivery_protocol::transport_v2::EnvelopeV2::EnableBrokerClient { room },
        )
    }

    /// Send privately through the co-edit session's existing demultiplexer.
    ///
    /// # Errors
    /// Returns an error for a different room, v1, or a full/disconnected queue.
    pub fn send_action(
        &self,
        action: tilo_delivery_protocol::peer_actions::DirectedAction,
    ) -> Result<(), crate::client::ClientError> {
        if !crate::room_workspace_id_matches(&action.room.to_string(), self.workspace_id) {
            return Err(crate::client::ClientError::Codec(
                "action room differs from co-edit room".into(),
            ));
        }
        self.client
            .send_action(tilo_delivery_protocol::transport_v2::EnvelopeV2::Directed(
                action,
            ))
    }

    /// Receive only private events; this never consumes shared co-edit state.
    pub async fn recv_action(&self) -> Option<tilo_delivery_protocol::transport_v2::EnvelopeV2> {
        self.client.recv_action().await
    }

    /// Attach an already healthy durable broker to this private lane.
    ///
    /// # Errors
    ///
    /// Returns `crate::client::ClientError` if the inputs violate this operation's bounds or invariants.
    pub fn register_action_broker(
        &self,
        room: uuid::Uuid,
        host: uuid::Uuid,
    ) -> Result<(), crate::client::ClientError> {
        if !crate::room_workspace_id_matches(&room.to_string(), self.workspace_id) || host.is_nil()
        {
            return Err(crate::client::ClientError::Codec(
                "invalid broker room or host".into(),
            ));
        }
        self.client.send_action(
            tilo_delivery_protocol::transport_v2::EnvelopeV2::RegisterBroker { room, host },
        )
    }
    /// Receive opaque source evidence using this connection's authenticated demultiplexer.
    pub async fn recv_certified_action(&self) -> Option<crate::client::CertifiedActionEvent> {
        self.client.recv_certified_action().await
    }

    /// Close both lanes when an attached native broker loses its bounded path.
    pub async fn close(&self) {
        self.client.disconnect().await;
    }

    /// The workspace this session is bound to.
    pub fn workspace_id(&self) -> u64 {
        self.workspace_id
    }

    /// Ask the host for `channel`'s retained state again — its checkpoint
    /// and tail — delivered as [`Inbound::Replayed`].
    ///
    /// # Errors
    ///
    /// Returns [`CoeditError::Disconnected`] if the relay connection is gone.
    pub fn request_channel_replay(&self, channel: [u8; 32]) -> Result<(), CoeditError> {
        self.client
            .send(Envelope::ReplayChannel {
                workspace_id: self.workspace_id,
                channel,
            })
            .map_err(|_| CoeditError::Disconnected)
    }

    /// Publish and retain a complete checkpoint for `channel` that names
    /// nothing it holds ([`CoeditPayload::checkpoint_of`]).
    ///
    /// # Errors
    ///
    /// Returns [`CoeditError::Disconnected`] if the relay connection is
    /// gone, or [`CoeditError::Encode`] if the payload can't be
    /// postcard-encoded.
    pub fn publish_checkpoint(&self, channel: [u8; 32], body: Vec<u8>) -> Result<(), CoeditError> {
        self.publish_payload(&CoeditPayload::checkpoint_of(channel, body))
    }

    /// Publish and retain a complete checkpoint for `channel` identified as
    /// `entry`, stating in `coverage` which retained messages it holds.
    ///
    /// # Errors
    ///
    /// Returns [`CoeditError::Disconnected`] if the relay connection is
    /// gone, or [`CoeditError::Encode`] if the payload can't be
    /// postcard-encoded.
    pub fn publish_covering_checkpoint(
        &self,
        channel: [u8; 32],
        entry: [u8; 32],
        coverage: Coverage,
        body: Vec<u8>,
    ) -> Result<(), CoeditError> {
        self.publish_payload(&CoeditPayload::Checkpoint {
            channel,
            entry,
            coverage,
            body,
        })
    }

    /// Publish an incremental update and retain it after the current channel
    /// checkpoint so a device that has never connected can catch up. It is
    /// identified by the digest of its body ([`CoeditPayload::retained_of`]).
    ///
    /// # Errors
    ///
    /// Returns [`CoeditError::Disconnected`] if the relay connection is
    /// gone, or [`CoeditError::Encode`] if the payload can't be
    /// postcard-encoded.
    pub fn publish_retained_delta(
        &self,
        channel: [u8; 32],
        body: Vec<u8>,
    ) -> Result<(), CoeditError> {
        self.publish_payload(&CoeditPayload::retained_of(channel, body))
    }

    /// [`Self::publish_retained_delta`] identified as `entry`, the id a later
    /// checkpoint names in [`Coverage::Entries`] once it holds this update.
    ///
    /// # Errors
    ///
    /// Returns [`CoeditError::Disconnected`] if the relay connection is
    /// gone, or [`CoeditError::Encode`] if the payload can't be
    /// postcard-encoded.
    pub fn publish_retained_entry(
        &self,
        channel: [u8; 32],
        entry: [u8; 32],
        body: Vec<u8>,
    ) -> Result<(), CoeditError> {
        self.publish_payload(&CoeditPayload::RetainedDelta {
            channel,
            entry,
            body,
        })
    }

    /// [`Self::publish_retained_delta`] on behalf of `author`, a device of
    /// this session's own person that cannot reach the relay itself (a
    /// browser its Cloud host carries). The relay takes it once `author`'s
    /// certified hello went this way first and the person is admitted; every
    /// member attributes it to `author` and verifies it as theirs.
    ///
    /// # Errors
    ///
    /// Returns [`CoeditError::Disconnected`] if the relay connection is
    /// gone, or [`CoeditError::Encode`] if the payload can't be
    /// postcard-encoded.
    pub fn forward_retained_delta(
        &self,
        author: [u8; 32],
        channel: [u8; 32],
        body: Vec<u8>,
    ) -> Result<(), CoeditError> {
        self.publish_payload_as(author, &CoeditPayload::retained_of(channel, body))
    }

    fn publish_payload(&self, payload: &CoeditPayload) -> Result<(), CoeditError> {
        self.publish_payload_as(self.author, payload)
    }

    fn publish_payload_as(
        &self,
        author: [u8; 32],
        payload: &CoeditPayload,
    ) -> Result<(), CoeditError> {
        let delta = payload.encode().map_err(CoeditError::Encode)?;
        self.client
            .send(Envelope::Delta {
                workspace_id: self.workspace_id,
                author_device_public_key: author,
                authored_at_ns: now_ns(),
                delta,
                // The op-level signature (for the signed variant) lives
                // inside the CoeditPayload; the envelope-level slot is
                // unused on the co-edit path, matching how the durable
                // store forwards opaque delta bytes.
                signature: [0u8; 64],
            })
            .map_err(|_| CoeditError::Disconnected)
    }

    /// Ask the current authenticated relay owner to refresh its genuine retained
    /// checkpoint. Queue acceptance is neither durability nor a checkpoint ACK.
    ///
    /// # Errors
    ///
    /// Returns `CoeditError` if the request cannot be sent or its reply is rejected.
    pub fn request_checkpoint_refresh(&self) -> Result<(), CoeditError> {
        self.client
            .send(Envelope::SyncRequest {
                workspace_id: self.workspace_id,
                have_heads: Vec::new(),
            })
            .map_err(|_| CoeditError::Disconnected)
    }

    /// Publish an ephemeral presence payload — the toolkit `Presence`
    /// path. Rides an `Envelope::Presence`, so it is fanned out but
    /// never persisted.
    ///
    /// `payload` is the opaque toolkit presence body; this method stamps
    /// the session's `author` device key around it (via
    /// [`AttributedPresence`]) so the receiver can attribute the
    /// presence — the host's presence envelope carries no author of its
    /// own.
    ///
    /// # Errors
    ///
    /// Returns [`CoeditError::Disconnected`] if the relay connection is
    /// gone, or [`CoeditError::Encode`] if the wrapped payload can't be
    /// postcard-encoded.
    pub fn publish_presence(&self, payload: Vec<u8>) -> Result<(), CoeditError> {
        let wrapped = AttributedPresence {
            author: self.author,
            body: payload,
        };
        let encoded = wrapped.encode().map_err(CoeditError::Encode)?;
        self.client
            .send(Envelope::Presence {
                workspace_id: self.workspace_id,
                payload: encoded,
            })
            .map_err(|_| CoeditError::Disconnected)
    }

    /// Send a remote-action frame to one device of this workspace through
    /// the host. The host routes it to that device's live connection only;
    /// it is never stored or fanned out.
    ///
    /// # Errors
    ///
    /// Returns [`CoeditError::Disconnected`] if the connection is gone.
    pub fn publish_remote_action(&self, to: [u8; 32], frame: Vec<u8>) -> Result<(), CoeditError> {
        self.client
            .send(Envelope::RemoteAction {
                workspace_id: self.workspace_id,
                version: tilo_relay_protocol::transport::REMOTE_ACTION_ENVELOPE_VERSION,
                to_device_public_key: to,
                // The host overwrites both from the authenticated connection.
                from_device_public_key: self.author,
                from_user_public_key: [0; 32],
                frame,
            })
            .map_err(|_| CoeditError::Disconnected)
    }

    /// Await the next inbound co-edit event, translating the host
    /// [`Envelope`] into the toolkit-facing [`Inbound`]. Keep-alives
    /// (`Ping`/`Pong`) and other non-co-edit frames are skipped; a
    /// `NotAuthorised` reply surfaces once as [`Inbound::Refused`]; a closed
    /// connection ends the stream (`None`).
    pub async fn recv(&self) -> Option<Inbound> {
        loop {
            let envelope = self.client.recv().await?;
            if matches!(&envelope,
                Envelope::Delta { workspace_id, .. }
                | Envelope::Presence { workspace_id, .. }
                | Envelope::Ready { workspace_id, .. }
                if *workspace_id != self.workspace_id)
            {
                return None;
            }
            match envelope {
                Envelope::Delta {
                    author_device_public_key,
                    delta,
                    ..
                } => {
                    // A co-edit peer wraps its body in a CoeditPayload; a
                    // plain workspace-directory delta from another tier
                    // won't decode — skip it rather than mis-apply.
                    match CoeditPayload::decode(&delta) {
                        Some(
                            CoeditPayload::Checkpoint { body: data, .. }
                            | CoeditPayload::RetainedDelta { body: data, .. },
                        ) => {
                            return Some(Inbound::Delta {
                                author: author_device_public_key,
                                data,
                            });
                        }
                        None => {}
                    }
                }
                Envelope::Presence { payload, .. } => {
                    // Presence rides an author-stamped wrapper (the host
                    // envelope has no author of its own). A non-co-edit
                    // presence frame won't decode — skip it rather than
                    // surface it unattributed.
                    if let Some(AttributedPresence { author, body }) =
                        AttributedPresence::decode(&payload)
                    {
                        return Some(Inbound::Presence {
                            author,
                            payload: body,
                        });
                    }
                }
                Envelope::RemoteAction {
                    workspace_id,
                    from_device_public_key,
                    frame,
                    ..
                } if workspace_id == self.workspace_id => {
                    return Some(Inbound::RemoteAction {
                        from: from_device_public_key,
                        frame,
                    });
                }
                Envelope::ChannelReplay {
                    workspace_id,
                    channel,
                    frames,
                } if workspace_id == self.workspace_id => {
                    let deltas = frames
                        .iter()
                        .filter_map(|frame| {
                            let Envelope::Delta {
                                author_device_public_key,
                                delta,
                                ..
                            } = crate::frame::decode(frame.get(4..)?).ok()?
                            else {
                                return None;
                            };
                            match CoeditPayload::decode(&delta)? {
                                CoeditPayload::Checkpoint { body, .. }
                                | CoeditPayload::RetainedDelta { body, .. } => {
                                    Some((author_device_public_key, body))
                                }
                            }
                        })
                        .collect();
                    return Some(Inbound::Replayed { channel, deltas });
                }
                Envelope::Ready { pending_deltas, .. } => {
                    return Some(Inbound::Ready {
                        pending: pending_deltas,
                    });
                }
                Envelope::NotAuthorised {
                    refusal, reason, ..
                } => {
                    return Some(Inbound::Refused { refusal, reason });
                }
                // Keep-alives and federation chatter carry no co-edit
                // payload — keep waiting.
                _ => {}
            }
        }
    }

    /// Consume the session and hand back the underlying client (e.g. to
    /// unsubscribe or reuse the connection for another workspace).
    pub fn into_client(self) -> RelayClient {
        self.client
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coedit_wrappers_round_trip_and_reject_foreign_bytes() {
        use automerge::transaction::Transactable;

        // Every payload variant round-trips.
        let checkpoint = CoeditPayload::Checkpoint {
            channel: [3; 32],
            entry: [7; 32],
            coverage: Coverage::Entries(vec![[8; 32], [9; 32]]),
            body: vec![6, 5],
        };
        let baseline = CoeditPayload::Checkpoint {
            channel: [3; 32],
            entry: [1; 32],
            coverage: Coverage::Supersedes,
            body: vec![6],
        };
        let retained = CoeditPayload::RetainedDelta {
            channel: [4; 32],
            entry: [5; 32],
            body: vec![2, 1],
        };
        for payload in [checkpoint, baseline, retained] {
            assert_eq!(
                CoeditPayload::decode(&payload.encode().unwrap()),
                Some(payload)
            );
        }

        // The author key must survive the presence wrapper so a remote cursor
        // is drawn against the right collaborator — the host envelope carries
        // no author of its own.
        let wrapped = AttributedPresence {
            author: [7u8; 32],
            body: vec![10, 20, 30],
        };
        let decoded = AttributedPresence::decode(&wrapped.encode().unwrap()).unwrap();
        assert_eq!(decoded.author, [7u8; 32]);
        assert_eq!(decoded.body, vec![10, 20, 30]);

        // Bodies that are not a CoeditPayload must not decode into a bogus
        // variant — `recv` skips such frames rather than mis-apply them.
        // Empty, an out-of-range enum tag, and a genuine Automerge
        // incremental delta all fail to decode.
        assert!(CoeditPayload::decode(&[]).is_none());
        assert!(CoeditPayload::decode(&[5, 0, 0]).is_none());
        let mut doc = automerge::AutoCommit::new();
        doc.put(automerge::ROOT, "k", "v").unwrap();
        let raw_delta = doc.save_incremental();
        assert!(!raw_delta.is_empty());
        assert!(
            CoeditPayload::decode(&raw_delta).is_none(),
            "a raw Automerge delta must not be mistaken for a CoeditPayload"
        );
    }

    #[test]
    fn retention_metadata_reports_only_kind_and_channel() {
        for (payload, expected) in [
            (
                CoeditPayload::Checkpoint {
                    channel: [3; 32],
                    entry: [5; 32],
                    coverage: Coverage::Entries(vec![[6; 32]]),
                    body: vec![8; 1024],
                },
                ([3; 32], RetentionKind::Checkpoint),
            ),
            (
                CoeditPayload::RetainedDelta {
                    channel: [4; 32],
                    entry: [6; 32],
                    body: vec![7; 1024],
                },
                ([4; 32], RetentionKind::Delta),
            ),
        ] {
            let encoded = payload.encode().unwrap();
            assert_eq!(retention_metadata(&encoded), Some(expected));
            let mut trailing = encoded;
            trailing.push(0);
            assert_eq!(retention_metadata(&trailing), Some(expected));
            assert!(CoeditPayload::decode(&trailing).is_some());
        }
        assert_eq!(retention_metadata(&[]), None);
    }

    #[tokio::test]
    async fn coedit_rejects_wrong_workspace_before_emitting_any_input() {
        use crate::auth;
        use crate::session_carrier::{SessionCarrier, SessionChannels};
        use tilo_user_identity::{
            DeviceCapabilities, DeviceCert, RevocationList, sign_device_cert,
        };
        struct Carrier;
        impl SessionCarrier for Carrier {
            fn abort(&self, _: &'static [u8]) {}
            fn disconnect(&self) -> n0_future::boxed::BoxFuture<()> {
                Box::pin(async {})
            }
            fn close_owned(&self) -> n0_future::boxed::BoxFuture<()> {
                Box::pin(async {})
            }
        }
        let root = ed25519_dalek::SigningKey::from_bytes(&[31; 32]);
        let host = ed25519_dalek::SigningKey::from_bytes(&[32; 32]);
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: host.verifying_key().to_bytes(),
            device_label: "coedit scoped receive".into(),
            capabilities: DeviceCapabilities::host(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, &root).unwrap();
        let ack = auth::build_hello_ack(cert.clone(), &host, [2; 32], &[1; 32]);
        let peer = auth::verify_hello_ack(
            &ack,
            &cert.user_public_key,
            &RevocationList::new(),
            tilo_delivery_protocol::clock::now_ns(),
            &[1; 32],
        )
        .unwrap();
        let bad = [
            Envelope::Delta {
                workspace_id: 42,
                author_device_public_key: [3; 32],
                authored_at_ns: 1,
                signature: [0; 64],
                delta: CoeditPayload::RetainedDelta {
                    channel: [4; 32],
                    entry: [6; 32],
                    body: vec![5],
                }
                .encode()
                .unwrap(),
            },
            Envelope::Presence {
                workspace_id: 42,
                payload: AttributedPresence {
                    author: [3; 32],
                    body: vec![5],
                }
                .encode()
                .unwrap(),
            },
            Envelope::Ready {
                workspace_id: 42,
                pending_deltas: 0,
            },
        ];
        for frame in bad {
            let (outbound, _outgoing) = tokio::sync::mpsc::channel(4);
            let (incoming, inbound) = tokio::sync::mpsc::channel(4);
            let (_action_incoming, action_inbound) = tokio::sync::mpsc::channel(1);
            let client = RelayClient::from_authenticated_session(
                peer.clone(),
                [9; 32],
                SessionChannels {
                    outbound,
                    inbound,
                    action_outbound: None,
                    action_inbound,
                },
                Box::new(Carrier),
            );
            let session = RelayCoeditSession::join(client, 41, [9; 32]).unwrap();
            incoming.send(frame).await.unwrap();
            drop(incoming);
            assert!(
                session.recv().await.is_none(),
                "wrong workspace must produce no coedit input"
            );
        }
    }
}
