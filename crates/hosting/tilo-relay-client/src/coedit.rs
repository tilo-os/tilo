//! The co-edit payloads the toolkit's collaboration layer exchanges through
//! the workspace host: [`CoeditPayload`] inside an `Envelope::Delta`,
//! [`AttributedPresence`] inside an `Envelope::Presence`, and the
//! [`Inbound`] events a session surfaces. Browser and native peers share
//! these; the relay session that speaks them over iroh
//! (`tilo_relay_session::coedit::RelayCoeditSession`) lives in
//! `tilo-relay-session`.
//!
//! Presence carries its author inside the payload because the host's
//! presence envelope has no author slot; see [`AttributedPresence`].

use serde::{Deserialize, Serialize};

/// Body carried inside an `Envelope::Delta`. Self-describing so the
/// receiver can distinguish retained checkpoints from their incremental tail.
/// Retention metadata is deliberately visible to the host while the document
/// body remains opaque.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CoeditPayload {
    /// A complete, idempotent state checkpoint for one logical document.
    /// The host replaces the previous checkpoint and its older tail for the
    /// same channel, allowing a device with no prior queue to bootstrap.
    Checkpoint {
        /// Stable digest of the logical document route.
        channel: [u8; 32],
        /// Opaque collaboration message.
        body: Vec<u8>,
    },
    /// An incremental update retained after the latest checkpoint for this
    /// channel. It is replayed to first-time subscribers as well as queued to
    /// already-known offline members.
    RetainedDelta {
        /// Stable digest of the logical document route.
        channel: [u8; 32],
        /// Opaque collaboration message.
        body: Vec<u8>,
    },
}

impl CoeditPayload {
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
#[doc(hidden)]
pub enum RetentionKind {
    Checkpoint,
    Delta,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Deserialize)]
enum RetentionMetadata<'a> {
    Checkpoint {
        channel: [u8; 32],
        #[serde(borrow)]
        body: &'a [u8],
    },
    RetainedDelta {
        channel: [u8; 32],
        #[serde(borrow)]
        body: &'a [u8],
    },
}

#[cfg(not(target_arch = "wasm32"))]
/// Decode only the host-visible retention route and kind. The opaque body is
/// consumed without allocating or exposing it to diagnostics.
#[doc(hidden)]
pub fn retention_metadata(bytes: &[u8]) -> Option<([u8; 32], RetentionKind)> {
    retention_view(bytes).map(|(channel, kind, _)| (channel, kind))
}

#[cfg(not(target_arch = "wasm32"))]
/// Channel, kind and borrowed opaque body of a co-edit payload, without
/// allocating. Accepts exactly the inputs [`CoeditPayload::decode`] accepts,
/// trailing bytes included, so a caller can switch between them without
/// changing which frames it classifies as co-edit payloads.
#[doc(hidden)]
pub fn retention_view(bytes: &[u8]) -> Option<([u8; 32], RetentionKind, &[u8])> {
    let metadata: RetentionMetadata<'_> = postcard::from_bytes(bytes).ok()?;
    Some(match metadata {
        RetentionMetadata::Checkpoint { channel, body } => {
            (channel, RetentionKind::Checkpoint, body)
        }
        RetentionMetadata::RetainedDelta { channel, body } => (channel, RetentionKind::Delta, body),
    })
}

/// Presence body wrapper that carries author attribution.
///
/// `Envelope::Presence` has no author field — the host forwards the
/// opaque body. The publisher wraps its device public key around toolkit
/// presence `body`; the governing relay stamps device attribution from the
/// authenticated publishing connection before forwarding. The receiver recovers it
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
        /// [`AttributedPresence`] wrapper, stamped by the governing relay for
        /// a publishing device. The host's presence envelope
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
    /// A piece of a record's content another device of the workspace
    /// addressed to this one. The relay stamped the sender from its
    /// authenticated connection.
    RecordContent {
        /// The sending device's public key.
        from: [u8; 32],
        /// The piece, as the sender encoded it.
        frame: Vec<u8>,
    },
    /// A live-plane frame (presence, previews) another live member of the
    /// workspace published: a native shell, a browser, an agent. The host
    /// checked its sealed form and stamped the sender from its
    /// authenticated session.
    Live {
        /// The publishing device's public key.
        from: [u8; 32],
        /// The canonical `tilo_relay_protocol::live::SealedLiveFrame` bytes.
        frame: Vec<u8>,
    },
    /// The host's reply to the initial subscribe: how many queued
    /// deltas it is about to flush from this device's offline queue.
    Ready {
        /// Number of store-and-forwarded deltas being flushed.
        pending: u32,
    },
    /// The room's governor answered about one of its calls.
    Voice(VoiceEvent),
}

/// What the governor of a room says about its calls; see
/// [`tilo_relay_protocol::voice`].
#[derive(Debug, Clone)]
pub enum VoiceEvent {
    /// The outcome of a call change this device requested.
    Result {
        /// The request's id.
        request: [u8; 16],
        /// The call as governance now records it, or why it was refused.
        result: Result<tilo_relay_protocol::voice::CallState, String>,
    },
    /// A participation's current grant.
    Granted(Box<tilo_relay_protocol::voice::VoiceGrant>),
    /// The device may not be in `call` (any more).
    Refused {
        /// The call.
        call: [u8; 32],
        /// Why.
        reason: String,
    },
}

/// Failure modes for `RelayCoeditSession` operations.
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
    /// A live frame that is not sealed with its record's channel keys is
    /// never sent.
    Unsealed,
}

impl std::fmt::Display for CoeditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CoeditError::Disconnected => f.write_str("relay co-edit session disconnected"),
            CoeditError::Encode(error) => write!(f, "co-edit payload encode failed: {error}"),
            CoeditError::Unsealed => f.write_str("a live frame is sent only sealed"),
        }
    }
}

impl std::error::Error for CoeditError {}

#[cfg(test)]
#[cfg(not(target_arch = "wasm32"))]
mod tests {
    use super::{CoeditPayload, RetentionKind, retention_view};

    fn owned(bytes: &[u8]) -> Option<([u8; 32], RetentionKind, Vec<u8>)> {
        CoeditPayload::decode(bytes).map(|payload| match payload {
            CoeditPayload::Checkpoint { channel, body } => {
                (channel, RetentionKind::Checkpoint, body)
            }
            CoeditPayload::RetainedDelta { channel, body } => (channel, RetentionKind::Delta, body),
        })
    }

    #[test]
    fn retention_view_matches_owned_decode() {
        let mut inputs = Vec::new();
        for payload in [
            CoeditPayload::Checkpoint {
                channel: [3; 32],
                body: vec![8; 1024],
            },
            CoeditPayload::RetainedDelta {
                channel: [4; 32],
                body: vec![7; 300],
            },
            CoeditPayload::Checkpoint {
                channel: [5; 32],
                body: Vec::new(),
            },
        ] {
            let encoded = payload.encode().unwrap();
            let mut trailing = encoded.clone();
            trailing.extend_from_slice(&[0, 1, 2]);
            // A truncated frame must be rejected by both decoders.
            let truncated = encoded[..encoded.len() - 1].to_vec();
            inputs.extend([encoded, trailing, truncated]);
        }
        // Empty, an out-of-range variant tag, and arbitrary garbage.
        inputs.extend([vec![], vec![5, 0, 0], vec![0xff; 40], vec![1]]);

        let mut accepted = 0;
        for input in &inputs {
            let view = retention_view(input).map(|(c, k, body)| (c, k, body.to_vec()));
            assert_eq!(view, owned(input), "input {input:?}");
            accepted += usize::from(view.is_some());
        }
        // Three payloads, each plain and with trailing bytes.
        assert_eq!(accepted, 6);
    }
}
