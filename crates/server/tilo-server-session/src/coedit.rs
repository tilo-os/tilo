//! Route the toolkit's co-edit protocol through the workspace host.
//!
//! The `ui` collaboration layer (`ui::collaboration`) speaks
//! `CollabRequest` / `CollabEvent` frames carrying Automerge deltas and
//! presence; the host speaks [`Envelope`]. [`ServerCoeditSession`] wraps a
//! [`ServerClient`] and exposes the co-edit verbs an app needs — publish
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
//! attribution has to ride *inside* the payload: [`ServerCoeditSession::publish_presence`]
//! wraps the toolkit body in an [`AttributedPresence`] carrying the
//! session's own device key, and [`ServerCoeditSession::recv`] unwraps it
//! into [`Inbound::Presence`]`{ author, payload }`. Without this a
//! remote cursor would arrive unauthored and could not be drawn against
//! the right member.
//!
//! Document-op *authorisation* (a viewer without a write cap must not
//! mutate the doc) is enforced by the broker/host layer that validates
//! `SignedOp`s, not by this transport shim — the host stores and
//! forwards `Envelope::Delta` bytes verbatim, exactly as it does for
//! any workspace-directory delta.

pub use tilo_server_client::coedit::{AttributedPresence, CoeditError, CoeditPayload, Inbound};
// The daemon classifies retained frames with these (`tilo_server::coedit::*`).
#[cfg(not(target_arch = "wasm32"))]
pub use tilo_server_client::coedit::{RetentionKind, retention_metadata, retention_view};

use crate::client::ServerClient;
use crate::transport::Envelope;
use collab_types::WorkspaceId;

/// A co-edit session bound to one workspace on one host connection.
///
/// Construct it from an already-connected [`ServerClient`] via
/// [`ServerCoeditSession::join`]; then publish document deltas / presence
/// and drain inbound events with [`ServerCoeditSession::recv`].
pub struct ServerCoeditSession {
    client: ServerClient,
    workspace_id: WorkspaceId,
    author: [u8; 32],
    /// When the host was last heard from, as [`tilo_clock::unix_now_ns`].
    last_heard_ns: std::sync::atomic::AtomicU64,
    /// The on-demand records this session opened, by retained channel:
    /// opened again with every resubscription.
    opened: std::sync::Mutex<std::collections::BTreeSet<[u8; 32]>>,
    /// Whether the host said this workspace sends a device only its
    /// governance, its shared desktop and the heads of its chains, and every
    /// other record once the device opens it (its `Ready`): what a device
    /// reads as `documents_when_opened`.
    when_opened: std::sync::atomic::AtomicBool,
}

impl ServerCoeditSession {
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
        client: ServerClient,
        workspace_id: WorkspaceId,
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
            when_opened: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Open a record of this workspace by its retained channel
    /// (`neighbour_wire::app_document_group`): an on-demand one
    /// (`neighbour_wire::document_sync`), which a subscription does not
    /// replay, or any record of a workspace that sends them all only when
    /// they are opened ([`Self::documents_when_opened`]). The host sends what
    /// it retains of it as [`Inbound::Delta`]s, then [`Inbound::DocumentOpened`]
    /// saying how many frames that was, and its changes from then on; or it
    /// answers that the record is not this device's to open.
    ///
    /// A record this session opened already is not asked for again; a
    /// resubscription asks for every opened record again
    /// ([`Self::resubscribe`]).
    ///
    /// # Errors
    ///
    /// Returns [`CoeditError::Disconnected`] if the connection is gone.
    pub fn open_resource(&self, channel: [u8; 32]) -> Result<(), CoeditError> {
        if !self
            .opened
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(channel)
        {
            return Ok(());
        }
        self.client
            .send(Envelope::OpenDocument {
                workspace_id: self.workspace_id,
                channel,
            })
            .map_err(|_| CoeditError::Disconnected)
    }

    /// Whether the host said this workspace sends a device only its
    /// governance, its shared desktop and the heads of its chained records,
    /// and every other record once the device opens it
    /// ([`Self::open_resource`]) — the `documents_when_opened` of its `Ready`.
    /// `false` until the host says otherwise.
    #[must_use]
    pub fn documents_when_opened(&self) -> bool {
        self.when_opened.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Whether this session opened `channel`'s record
    /// ([`Self::open_resource`]): the host sends it in the record's own order,
    /// what it retains first, and every write of it is decided by its place
    /// there, never by when it arrived.
    #[must_use]
    pub fn opened_resource(&self, channel: &[u8; 32]) -> bool {
        self.opened
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(channel)
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
                .send(Envelope::OpenDocument {
                    workspace_id: self.workspace_id,
                    channel,
                })
                .map_err(|_| CoeditError::Disconnected)?;
        }
        Ok(())
    }

    /// Join the workspace's live session, where presence and previews of every
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
    /// ([`ServerClient::transport_stats`]).
    #[cfg(not(target_arch = "wasm32"))]
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

    /// Close the session.
    pub async fn close(&self) {
        self.client.disconnect().await;
    }

    /// The workspace this session is bound to.
    pub fn workspace_id(&self) -> WorkspaceId {
        self.workspace_id
    }

    /// The device key of the server this session is with, as its handshake
    /// authenticated it: the device a remote action for the server itself is
    /// addressed to.
    #[must_use]
    pub const fn server_device_public_key(&self) -> [u8; 32] {
        self.client.server_peer.device_public_key
    }

    /// Publish and retain a complete checkpoint for `channel`.
    ///
    /// # Errors
    ///
    /// Returns [`CoeditError::Disconnected`] if the server connection is
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
    /// Returns [`CoeditError::Disconnected`] if the server connection is
    /// gone, or [`CoeditError::Encode`] if the payload can't be
    /// postcard-encoded.
    pub fn publish_retained_delta(
        &self,
        channel: [u8; 32],
        body: Vec<u8>,
    ) -> Result<(), CoeditError> {
        self.publish_payload(&CoeditPayload::RetainedDelta { channel, body })
    }

    /// Send `payload`, a write this device must know the host keeps, as
    /// commit `op`: the host admits it as it would the same payload
    /// published, and answers [`Inbound::Committed`] once it is durable, or
    /// [`Inbound::CommitRefused`]. Sending the same `op` again, on this or a
    /// later session, is answered from the host's log and kept once.
    ///
    /// # Errors
    ///
    /// Returns [`CoeditError::Disconnected`] if the server connection is
    /// gone, or [`CoeditError::Encode`] if the payload can't be
    /// postcard-encoded.
    pub fn commit(&self, op: [u8; 16], payload: &CoeditPayload) -> Result<(), CoeditError> {
        let delta = payload.encode().map_err(CoeditError::Encode)?;
        self.client
            .send(Envelope::Commit {
                workspace_id: self.workspace_id,
                op,
                author_device_public_key: self.author,
                authored_at_ns: tilo_clock::unix_now_ns(),
                delta,
            })
            .map_err(|_| CoeditError::Disconnected)
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

    /// Ask the current authenticated server owner to refresh its genuine retained
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
    /// Returns [`CoeditError::Disconnected`] if the server connection is
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
            .send(Envelope::DocumentContent {
                workspace_id: self.workspace_id,
                to_device_public_key: to,
                from_device_public_key: [0; 32],
                frame,
            })
            .map_err(|_| CoeditError::Disconnected)
    }

    /// Await the next inbound co-edit event, translating the host
    /// [`Envelope`] into the toolkit-facing [`Inbound`]. Keep-alives
    /// (`Ping`/`Pong`) and other non-co-edit frames are skipped; a
    /// `NotAuthorised` reply or a closed connection ends the stream
    /// (`None`).
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per frame a server sends: the translation is read top to bottom"
    )]
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
                envelope @ (Envelope::RemoteAction { .. } | Envelope::DocumentContent { .. }) => {
                    if let Some(inbound) = self.device_frame(envelope) {
                        return Some(inbound);
                    }
                }
                Envelope::Ready {
                    pending_deltas,
                    documents_when_opened,
                    ..
                } => {
                    self.when_opened
                        .store(documents_when_opened, std::sync::atomic::Ordering::Release);
                    return Some(Inbound::Ready {
                        pending: pending_deltas,
                        documents_when_opened,
                    });
                }
                envelope @ (Envelope::LiveMember { .. } | Envelope::DocumentOpened { .. }) => {
                    if let Some(inbound) = self.workspace_news(envelope) {
                        return Some(inbound);
                    }
                }
                Envelope::Committed {
                    workspace_id, op, ..
                } if workspace_id == self.workspace_id => {
                    return Some(Inbound::Committed { op });
                }
                Envelope::CommitRefused {
                    workspace_id,
                    op,
                    refusal,
                } if workspace_id == self.workspace_id => {
                    return Some(Inbound::CommitRefused { op, refusal });
                }
                Envelope::ChangeRefused {
                    workspace_id,
                    app_id,
                    cluster_id,
                    document_id,
                    changes,
                    refusal,
                } if workspace_id == self.workspace_id => {
                    return Some(Inbound::ChangeRefused {
                        app_id,
                        cluster_id,
                        document_id,
                        changes,
                        refusal,
                    });
                }
                Envelope::NotAuthorised { .. } => return None,
                Envelope::RemoteActionUndeliverable {
                    workspace_id,
                    to_device_public_key,
                    refusal,
                } if workspace_id == self.workspace_id => {
                    tracing::warn!(
                        workspace_id = %workspace_id,
                        to = %hex::encode(to_device_public_key),
                        %refusal,
                        "the host could not deliver a remote action sent on this session"
                    );
                }
                // Keep-alives and attach chatter carry no co-edit payload —
                // keep waiting.
                _ => {}
            }
        }
    }

    /// What the host tells this session about its workspace: a member
    /// joining or leaving the live session, a record opened, a call's news.
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
            Envelope::DocumentOpened {
                workspace_id,
                channel,
                answer,
            } if workspace_id == self.workspace_id => {
                Some(Inbound::DocumentOpened { channel, answer })
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
            Envelope::DocumentContent {
                workspace_id,
                from_device_public_key: from,
                frame,
                ..
            } if workspace_id == self.workspace_id => {
                Some(Inbound::DocumentContent { from, frame })
            }
            _ => None,
        }
    }

    /// Consume the session and hand back the underlying client (e.g. to
    /// unsubscribe or reuse the connection for another workspace).
    pub fn into_client(self) -> ServerClient {
        self.client
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn coedit_payloads_round_trip_and_name_their_retention;

        phase coedit_wrappers_round_trip_and_reject_foreign_bytes;
        {
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
            // is drawn against the right member — the host envelope carries
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

        phase retention_metadata_reports_only_kind_and_channel;
        {
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
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
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
                workspace_id: WorkspaceId::from_u128(42),
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
                workspace_id: WorkspaceId::from_u128(42),
                payload: AttributedPresence {
                    author: [3; 32],
                    body: vec![5],
                }
                .encode()
                .unwrap(),
            },
            Envelope::Ready {
                workspace_id: WorkspaceId::from_u128(42),
                pending_deltas: 0,
                documents_when_opened: false,
            },
        ];
        for frame in bad {
            let (outbound, _outgoing) = tokio::sync::mpsc::channel(4);
            let (incoming, inbound) = tokio::sync::mpsc::channel(4);
            let client = ServerClient::from_authenticated_session(
                peer.clone(),
                SessionChannels { outbound, inbound },
                Box::new(Carrier),
            );
            let session = ServerCoeditSession::join(
                client,
                tilo_server_protocol::WorkspaceId::from_u128(41),
                [9; 32],
            )
            .unwrap();
            incoming.send(frame).await.unwrap();
            drop(incoming);
            assert!(
                session.recv().await.is_none(),
                "wrong workspace must produce no coedit input"
            );
        }
    }
}
