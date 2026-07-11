//! Route the toolkit's co-edit protocol through the workspace host.
//!
//! The `ui` collaboration layer (`ui::collaboration`) speaks
//! `CollabRequest` / `CollabEvent` frames carrying Automerge deltas +
//! presence; the host speaks [`Envelope`]. Historically the two never
//! met: apps reach a broker (Unix socket / capnp) that fans a document
//! out *within one machine*, while `qos-relay` — the durable,
//! offline-capable host tier — was unreachable from the app path.
//!
//! This module is that missing seam. [`RelayCoeditSession`] wraps a
//! [`RelayClient`] and exposes the co-edit verbs an app needs — publish
//! a document delta, publish presence, receive peer deltas/presence —
//! mapping each onto an [`Envelope`]. Because document deltas travel as
//! [`Envelope::Delta`] they inherit the host's fan-out **and** its
//! on-disk store-and-forward: two peers that are never simultaneously
//! online still converge on the shared document.
//!
//! # Correspondence with the toolkit protocol
//!
//! [`CoeditPayload`] is the self-describing body carried inside an
//! `Envelope::Delta`, so the signed and unsigned toolkit variants both
//! survive the round trip (the host treats the bytes as opaque):
//!
//! | toolkit (`ui::collaboration`)                    | this module                     |
//! |--------------------------------------------------|---------------------------------|
//! | `CollabRequest::SyncIncremental { data }`        | [`RelayCoeditSession::publish_delta`]   |
//! | `CollabRequest::SignedSyncIncremental { env }`   | [`RelayCoeditSession::publish_signed`]  |
//! | `CollabRequest::Presence { .. }`                 | [`RelayCoeditSession::publish_presence`]|
//! | `CollabEvent::CRDTIncremental { data }`          | [`Inbound::Delta`]              |
//! | `CollabEvent::SignedCRDTIncremental { env, .. }` | [`Inbound::SignedDelta`]       |
//! | `CollabEvent::RemotePresenceUpdate { .. }`       | [`Inbound::Presence`]          |
//!
//! Keeping the mapping here — rather than importing `ui` — mirrors the
//! `collab-bridge` crate, which likewise translates the toolkit
//! protocol without pulling `ui`/wgpu into a transport crate.
//!
//! Document-op *authorisation* (a viewer without a write cap must not
//! mutate the doc) is enforced by the broker/host layer that validates
//! `SignedOp`s, not by this transport shim — the host stores and
//! forwards `Envelope::Delta` bytes verbatim, exactly as it does for
//! any workspace-directory delta.

use serde::{Deserialize, Serialize};

use crate::client::RelayClient;
use crate::transport::Envelope;

/// Body carried inside an [`Envelope::Delta`]. Self-describing so the
/// receiver can route unsigned vs. signed toolkit deltas without extra
/// wire state; the host never inspects it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CoeditPayload {
    /// A raw Automerge incremental delta — the `SyncIncremental` /
    /// `CRDTIncremental` toolkit path.
    Unsigned(Vec<u8>),
    /// A postcard-encoded `collab_types::SignedOp` — the
    /// `SignedSyncIncremental` / `SignedCRDTIncremental` path. Opaque
    /// here; signature + role validation is the broker/host's job, not
    /// the transport's.
    Signed(Vec<u8>),
}

impl CoeditPayload {
    /// Encode for carriage inside an `Envelope::Delta`.
    pub fn encode(&self) -> Vec<u8> {
        postcard::to_stdvec(self).expect("postcard cannot fail on CoeditPayload")
    }

    /// Decode a body previously produced by [`Self::encode`]. Returns
    /// `None` for bytes that are not a `CoeditPayload` (e.g. a plain
    /// workspace-directory delta from a non-co-edit peer).
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
    /// A signed document op. `op_bytes` is the postcard-encoded
    /// `SignedOp`; the consumer validates + applies it (the
    /// `SignedCRDTIncremental` path).
    SignedDelta {
        /// Publishing device's public key.
        author: [u8; 32],
        /// Postcard-encoded `collab_types::SignedOp`.
        op_bytes: Vec<u8>,
    },
    /// An ephemeral presence payload fanned out by the host. Not
    /// persisted; dropped if no peer is subscribed.
    Presence {
        /// Opaque presence payload (the toolkit encodes attribution
        /// inside it, since the host's presence envelope carries none).
        payload: Vec<u8>,
    },
    /// The host's reply to the initial subscribe: how many queued
    /// deltas it is about to flush from this device's offline queue.
    Ready {
        /// Number of store-and-forwarded deltas being flushed.
        pending: u32,
    },
}

/// Failure modes for [`RelayCoeditSession`] operations.
#[derive(Debug)]
pub enum CoeditError {
    /// The underlying relay connection is gone (the client's outbound
    /// channel was closed). Mirrors the toolkit's fire-and-forget
    /// contract: the caller re-syncs on reconnect.
    Disconnected,
}

impl std::fmt::Display for CoeditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CoeditError::Disconnected => f.write_str("relay co-edit session disconnected"),
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

    /// The workspace this session is bound to.
    pub fn workspace_id(&self) -> u64 {
        self.workspace_id
    }

    /// Publish a raw Automerge incremental delta — the toolkit
    /// `SyncIncremental` path. The host fans it out to online peers and
    /// queues it on disk for offline members.
    pub fn publish_delta(&self, delta: Vec<u8>) -> Result<(), CoeditError> {
        self.publish_payload(CoeditPayload::Unsigned(delta))
    }

    /// Publish a signed op's postcard bytes — the toolkit
    /// `SignedSyncIncremental` path.
    pub fn publish_signed(&self, op_bytes: Vec<u8>) -> Result<(), CoeditError> {
        self.publish_payload(CoeditPayload::Signed(op_bytes))
    }

    fn publish_payload(&self, payload: CoeditPayload) -> Result<(), CoeditError> {
        self.client
            .send(Envelope::Delta {
                workspace_id: self.workspace_id,
                author_device_public_key: self.author,
                authored_at_ns: now_ns(),
                delta: payload.encode(),
                // The op-level signature (for the signed variant) lives
                // inside the CoeditPayload; the envelope-level slot is
                // unused on the co-edit path, matching how the durable
                // store forwards opaque delta bytes.
                signature: [0u8; 64],
            })
            .map_err(|_| CoeditError::Disconnected)
    }

    /// Publish an ephemeral presence payload — the toolkit `Presence`
    /// path. Rides an `Envelope::Presence`, so it is fanned out but
    /// never persisted.
    pub fn publish_presence(&self, payload: Vec<u8>) -> Result<(), CoeditError> {
        self.client
            .send(Envelope::Presence {
                workspace_id: self.workspace_id,
                payload,
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
            match self.client.recv().await? {
                Envelope::Delta {
                    author_device_public_key,
                    delta,
                    ..
                } => {
                    // A co-edit peer wraps its body in a CoeditPayload; a
                    // plain workspace-directory delta from another tier
                    // won't decode — skip it rather than mis-apply.
                    match CoeditPayload::decode(&delta) {
                        Some(CoeditPayload::Unsigned(data)) => {
                            return Some(Inbound::Delta {
                                author: author_device_public_key,
                                data,
                            });
                        }
                        Some(CoeditPayload::Signed(op_bytes)) => {
                            return Some(Inbound::SignedDelta {
                                author: author_device_public_key,
                                op_bytes,
                            });
                        }
                        None => continue,
                    }
                }
                Envelope::Presence { payload, .. } => {
                    return Some(Inbound::Presence { payload });
                }
                Envelope::Ready { pending_deltas, .. } => {
                    return Some(Inbound::Ready {
                        pending: pending_deltas,
                    });
                }
                Envelope::NotAuthorised { .. } => return None,
                // Keep-alives and federation chatter carry no co-edit
                // payload — keep waiting.
                _ => continue,
            }
        }
    }

    /// Consume the session and hand back the underlying client (e.g. to
    /// unsubscribe or reuse the connection for another workspace).
    pub fn into_client(self) -> RelayClient {
        self.client
    }
}

/// Wall-clock nanoseconds since the epoch, used as the envelope's
/// `authored_at_ns`. The host assigns its own monotonic delivery seq
/// for the pending queue, so this value is metadata only.
fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coedit_payload_round_trips_both_variants() {
        let unsigned = CoeditPayload::Unsigned(vec![1, 2, 3, 4]);
        let signed = CoeditPayload::Signed(vec![9, 8, 7]);

        assert_eq!(CoeditPayload::decode(&unsigned.encode()), Some(unsigned));
        assert_eq!(CoeditPayload::decode(&signed.encode()), Some(signed));
    }

    #[test]
    fn non_coedit_bytes_do_not_masquerade_as_coedit() {
        // Bodies that are not a CoeditPayload must not decode into a
        // bogus variant — `recv` skips such frames rather than mis-apply
        // them. Empty, an out-of-range enum tag, and a genuine Automerge
        // incremental delta all fail to decode.
        assert!(CoeditPayload::decode(&[]).is_none());
        assert!(CoeditPayload::decode(&[5, 0, 0]).is_none());

        use automerge::transaction::Transactable;
        let mut doc = automerge::AutoCommit::new();
        doc.put(automerge::ROOT, "k", "v").unwrap();
        let raw_delta = doc.save_incremental();
        assert!(!raw_delta.is_empty());
        assert!(
            CoeditPayload::decode(&raw_delta).is_none(),
            "a raw Automerge delta must not be mistaken for a CoeditPayload"
        );
    }
}
