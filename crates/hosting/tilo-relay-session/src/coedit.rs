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

pub use tilo_relay_client::coedit::{
    AttributedPresence, CoeditError, CoeditPayload, Inbound, VoiceEvent,
};
// The daemon classifies retained frames with these (`tilo_relay::coedit::*`).
#[cfg(not(target_arch = "wasm32"))]
pub use tilo_relay_client::coedit::{RetentionKind, retention_metadata, retention_view};

use crate::client::RelayClient;
use crate::transport::Envelope;

/// The shortest gap between two moves of one pointer slot on any wire: 20 Hz.
/// Receivers glide a peer's pointer over the gap between two samples
/// (`ui::presence::POINTER_EASE_MAX` caps the glide at 100 ms), so 20 Hz moves
/// as smoothly as the 30 Hz producers publish at. The relay forwards a pointer
/// slot to each connection at most this often, a client writes one to a relay
/// at most this often, so a move the relay would only coalesce away never
/// costs a packet, and the shell broadcasts one to its direct (gossip)
/// neighbours at most this often too ([`crate::pointer_pacing`]).
pub const POINTER_GAP: std::time::Duration = std::time::Duration::from_millis(50);

/// The coalescing slot of a co-edit presence body: latest-state presence (a
/// pointer, a caret) has one, events do not. Only the two `NeighbourMsg`
/// variants that can have one are decoded, to tell which; the body itself is
/// sent and forwarded as it came.
#[must_use]
pub fn presence_slot(body: &[u8]) -> Option<neighbour_wire::PresenceSlot> {
    if !body
        .first()
        .is_some_and(|tag| neighbour_wire::NeighbourMsg::SLOTTED_PRESENCE_TAGS.contains(tag))
    {
        return None;
    }
    postcard::from_bytes::<neighbour_wire::NeighbourMsg>(body)
        .ok()?
        .presence_slot()
}

/// A co-edit session bound to one workspace on one host connection.
///
/// Construct it from an already-connected [`RelayClient`] via
/// [`RelayCoeditSession::join`]; then publish document deltas / presence
/// and drain inbound events with [`RelayCoeditSession::recv`].
pub struct RelayCoeditSession {
    client: RelayClient,
    workspace_id: u64,
    author: [u8; 32],
    /// When the host was last heard from, as [`tilo_clock::unix_now_ns`].
    last_heard_ns: std::sync::atomic::AtomicU64,
    /// The on-demand records this session opened, by retained channel:
    /// opened again with every resubscription.
    opened: std::sync::Mutex<std::collections::BTreeSet<[u8; 32]>>,
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
            last_heard_ns: std::sync::atomic::AtomicU64::new(tilo_clock::unix_now_ns()),
            opened: std::sync::Mutex::default(),
        })
    }

    /// Open an on-demand record of this workspace
    /// (`neighbour_wire::record_sync`), one a subscription does not
    /// replay, by its retained channel
    /// (`neighbour_wire::app_document_channel`). The host sends what it
    /// retains of it as [`Inbound::Delta`]s, then [`Inbound::RecordOpened`],
    /// and its changes from then on.
    ///
    /// # Errors
    ///
    /// Returns [`CoeditError::Disconnected`] if the connection is gone.
    pub fn open_record(&self, channel: [u8; 32]) -> Result<(), CoeditError> {
        self.opened
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(channel);
        self.client
            .send(Envelope::OpenRecord {
                workspace_id: self.workspace_id,
                channel,
            })
            .map_err(|_| CoeditError::Disconnected)
    }

    /// Ask the host to replay the workspace's retained state again, as it
    /// did for the first subscription (and answer `Ready` after it): for a
    /// device that dropped history it could not use then.
    ///
    /// # Errors
    ///
    /// Returns [`CoeditError::Disconnected`] if the connection is gone.
    pub fn resubscribe(&self) -> Result<(), CoeditError> {
        self.client
            .send(Envelope::Subscribe {
                workspace_id: self.workspace_id,
            })
            .map_err(|_| CoeditError::Disconnected)?;
        let opened = self
            .opened
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        for channel in opened {
            self.client
                .send(Envelope::OpenRecord {
                    workspace_id: self.workspace_id,
                    channel,
                })
                .map_err(|_| CoeditError::Disconnected)?;
        }
        Ok(())
    }

    /// Join the workspace's live plane, where presence and previews of every
    /// member (native, browser, agent) meet. Call after [`Self::join`].
    ///
    /// # Errors
    ///
    /// Returns [`CoeditError::Disconnected`] if the connection is gone.
    pub fn live_join(&self) -> Result<(), CoeditError> {
        self.client
            .send(Envelope::LiveJoin {
                workspace_id: self.workspace_id,
            })
            .map_err(|_| CoeditError::Disconnected)
    }

    /// Publish one sealed live frame
    /// ([`SealedLiveFrame`](crate::live::SealedLiveFrame)) to the
    /// workspace's live members. Lossy: nothing is queued or retained.
    ///
    /// # Errors
    ///
    /// Returns [`CoeditError::Unsealed`] for anything but a canonical sealed
    /// frame, and [`CoeditError::Disconnected`] if the connection is gone.
    pub fn publish_live(&self, frame: Vec<u8>) -> Result<(), CoeditError> {
        if crate::live::SealedLiveFrame::decode(&frame).is_err() {
            return Err(CoeditError::Unsealed);
        }
        self.client
            .send(Envelope::Live {
                workspace_id: self.workspace_id,
                // The host overwrites it from the authenticated session.
                from_device_public_key: self.author,
                frame,
            })
            .map_err(|_| CoeditError::Disconnected)
    }

    /// Ask the host for a `Pong`. A host that stops answering is gone even
    /// when the connection underneath has not noticed, see
    /// [`Self::silent_for`].
    ///
    /// # Errors
    ///
    /// Returns [`CoeditError::Disconnected`] if the connection is gone.
    pub fn ping(&self) -> Result<(), CoeditError> {
        self.client
            .send(Envelope::Ping)
            .map_err(|_| CoeditError::Disconnected)
    }

    /// The host connection's packet and frame counters
    /// ([`RelayClient::transport_stats`]).
    #[must_use]
    pub fn transport_stats(&self) -> Option<iroh::endpoint::ConnectionStats> {
        self.client.transport_stats()
    }

    /// How long nothing arrived from the host, keep-alives included.
    pub fn silent_for(&self) -> std::time::Duration {
        let heard = self
            .last_heard_ns
            .load(std::sync::atomic::Ordering::Relaxed);
        std::time::Duration::from_nanos(tilo_clock::unix_now_ns().saturating_sub(heard))
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

    /// The device key of the relay this session is with, as its handshake
    /// authenticated it: the device a remote action for the relay itself is
    /// addressed to.
    #[must_use]
    pub const fn relay_device_public_key(&self) -> [u8; 32] {
        self.client.relay_peer.device_public_key
    }

    /// Publish and retain a complete checkpoint for `channel`.
    ///
    /// # Errors
    ///
    /// Returns [`CoeditError::Disconnected`] if the relay connection is
    /// gone, or [`CoeditError::Encode`] if the payload can't be
    /// postcard-encoded.
    pub fn publish_checkpoint(&self, channel: [u8; 32], body: Vec<u8>) -> Result<(), CoeditError> {
        self.publish_payload(&CoeditPayload::Checkpoint { channel, body })
    }

    /// Publish an incremental update and retain it after the current channel
    /// checkpoint so a device that has never connected can catch up.
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
        self.publish_payload(&CoeditPayload::RetainedDelta { channel, body })
    }

    fn publish_payload(&self, payload: &CoeditPayload) -> Result<(), CoeditError> {
        let delta = payload.encode().map_err(CoeditError::Encode)?;
        self.client
            .send(Envelope::Delta {
                workspace_id: self.workspace_id,
                author_device_public_key: self.author,
                authored_at_ns: tilo_clock::unix_now_ns(),
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

    /// Send a piece of a record's content to one device of this workspace
    /// through the host, which hands it to that device's live connection
    /// only and drops it when it cannot.
    ///
    /// # Errors
    ///
    /// Returns [`CoeditError::Disconnected`] if the connection is gone.
    pub fn publish_record_content(&self, to: [u8; 32], frame: Vec<u8>) -> Result<(), CoeditError> {
        self.client
            .send(Envelope::RecordContent {
                workspace_id: self.workspace_id,
                to_device_public_key: to,
                from_device_public_key: [0; 32],
                frame,
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

    /// Ask the room's governor to change its calls; the answer arrives as
    /// [`VoiceEvent::Result`] carrying `request`.
    ///
    /// # Errors
    ///
    /// Returns [`CoeditError::Disconnected`] if the connection is gone.
    pub fn voice_call(
        &self,
        request: [u8; 16],
        operation: tilo_relay_protocol::voice::CallOperation,
    ) -> Result<(), CoeditError> {
        self.client
            .send(Envelope::VoiceCall {
                workspace_id: self.workspace_id,
                request,
                operation,
            })
            .map_err(|_| CoeditError::Disconnected)
    }

    /// Join `call`, or with `participation` repeat that join as its
    /// heartbeat; answered with [`VoiceEvent::Granted`] or
    /// [`VoiceEvent::Refused`].
    ///
    /// # Errors
    ///
    /// Returns [`CoeditError::Disconnected`] if the connection is gone.
    pub fn voice_join(
        &self,
        call: [u8; 32],
        participation: Option<[u8; 32]>,
    ) -> Result<(), CoeditError> {
        self.client
            .send(Envelope::VoiceJoin {
                workspace_id: self.workspace_id,
                call,
                participation,
            })
            .map_err(|_| CoeditError::Disconnected)
    }

    /// Ask for the room's calls with who is in each; answered with
    /// [`VoiceEvent::Calls`].
    ///
    /// # Errors
    ///
    /// Returns [`CoeditError::Disconnected`] if the connection is gone.
    pub fn voice_calls(&self) -> Result<(), CoeditError> {
        self.client
            .send(Envelope::VoiceCalls {
                workspace_id: self.workspace_id,
            })
            .map_err(|_| CoeditError::Disconnected)
    }

    /// Say what `participation` in `call` shows besides its voice from now
    /// on (its camera, a shared screen). Not answered: the call's next grant
    /// and call list carry it.
    ///
    /// # Errors
    ///
    /// Returns [`CoeditError::Disconnected`] if the connection is gone.
    pub fn voice_show(
        &self,
        call: [u8; 32],
        participation: [u8; 32],
        showing: tilo_relay_protocol::voice::Showing,
    ) -> Result<(), CoeditError> {
        self.client
            .send(Envelope::VoiceShow {
                workspace_id: self.workspace_id,
                call,
                participation,
                showing,
            })
            .map_err(|_| CoeditError::Disconnected)
    }

    /// End `participation` in `call`.
    ///
    /// # Errors
    ///
    /// Returns [`CoeditError::Disconnected`] if the connection is gone.
    pub fn voice_leave(&self, call: [u8; 32], participation: [u8; 32]) -> Result<(), CoeditError> {
        self.client
            .send(Envelope::VoiceLeave {
                workspace_id: self.workspace_id,
                call,
                participation,
            })
            .map_err(|_| CoeditError::Disconnected)
    }

    /// Await the next inbound co-edit event, translating the host
    /// [`Envelope`] into the toolkit-facing [`Inbound`]. Keep-alives
    /// (`Ping`/`Pong`) and other non-co-edit frames are skipped; a
    /// `NotAuthorised` reply or a closed connection ends the stream
    /// (`None`).
    pub async fn recv(&self) -> Option<Inbound> {
        loop {
            let envelope = self.client.recv().await?;
            self.last_heard_ns.store(
                tilo_clock::unix_now_ns(),
                std::sync::atomic::Ordering::Relaxed,
            );
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
                envelope @ (Envelope::RemoteAction { .. }
                | Envelope::RecordContent { .. }
                | Envelope::Live { .. }) => {
                    if let Some(inbound) = self.device_frame(envelope) {
                        return Some(inbound);
                    }
                }
                Envelope::Ready { pending_deltas, .. } => {
                    return Some(Inbound::Ready {
                        pending: pending_deltas,
                    });
                }
                envelope @ (Envelope::LiveMember { .. }
                | Envelope::RecordOpened { .. }
                | Envelope::VoiceCallResult { .. }
                | Envelope::VoiceGranted { .. }
                | Envelope::VoiceRefused { .. }
                | Envelope::VoiceCallList { .. }) => {
                    if let Some(inbound) = self.workspace_news(envelope) {
                        return Some(inbound);
                    }
                }
                Envelope::NotAuthorised { .. } => return None,
                Envelope::RemoteActionUndeliverable {
                    workspace_id,
                    to_device_public_key,
                    reason,
                } if workspace_id == self.workspace_id => {
                    tracing::warn!(
                        workspace_id,
                        to = %hex::encode(to_device_public_key),
                        %reason,
                        "the host could not deliver a remote action sent on this session"
                    );
                }
                // Keep-alives and federation chatter carry no co-edit
                // payload — keep waiting.
                _ => {}
            }
        }
    }

    /// What the host tells this session about its workspace: a member
    /// joining or leaving the live plane, a record opened, a call's news.
    /// `None` for another workspace's.
    fn workspace_news(&self, envelope: Envelope) -> Option<Inbound> {
        match envelope {
            Envelope::LiveMember {
                workspace_id,
                device_public_key,
                active,
            } if workspace_id == self.workspace_id => Some(Inbound::Member {
                device: device_public_key,
                active,
            }),
            Envelope::RecordOpened {
                workspace_id,
                channel,
                frames,
            } if workspace_id == self.workspace_id => {
                Some(Inbound::RecordOpened { channel, frames })
            }
            Envelope::VoiceCallResult {
                workspace_id,
                request,
                result,
            } if workspace_id == self.workspace_id => {
                Some(Inbound::Voice(VoiceEvent::Result { request, result }))
            }
            Envelope::VoiceGranted {
                workspace_id,
                grant,
            } if workspace_id == self.workspace_id => {
                Some(Inbound::Voice(VoiceEvent::Granted(grant)))
            }
            Envelope::VoiceRefused {
                workspace_id,
                call,
                reason,
            } if workspace_id == self.workspace_id => {
                Some(Inbound::Voice(VoiceEvent::Refused { call, reason }))
            }
            Envelope::VoiceCallList {
                workspace_id,
                result,
            } if workspace_id == self.workspace_id => {
                Some(Inbound::Voice(VoiceEvent::Calls(result)))
            }
            _ => None,
        }
    }

    /// A frame another device of this workspace sent this one, with the
    /// sender the host stamped; `None` for another workspace's.
    fn device_frame(&self, envelope: Envelope) -> Option<Inbound> {
        match envelope {
            Envelope::RemoteAction {
                workspace_id,
                from_device_public_key: from,
                frame,
                ..
            } if workspace_id == self.workspace_id => Some(Inbound::RemoteAction { from, frame }),
            Envelope::RecordContent {
                workspace_id,
                from_device_public_key: from,
                frame,
                ..
            } if workspace_id == self.workspace_id => Some(Inbound::RecordContent { from, frame }),
            Envelope::Live {
                workspace_id,
                from_device_public_key: from,
                frame,
            } if workspace_id == self.workspace_id => Some(Inbound::Live { from, frame }),
            _ => None,
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
            body: vec![6, 5],
        };
        let retained = CoeditPayload::RetainedDelta {
            channel: [4; 32],
            body: vec![2, 1],
        };
        for payload in [checkpoint, retained] {
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
                    body: vec![8; 1024],
                },
                ([3; 32], RetentionKind::Checkpoint),
            ),
            (
                CoeditPayload::RetainedDelta {
                    channel: [4; 32],
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
            tilo_clock::unix_now_ns(),
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
