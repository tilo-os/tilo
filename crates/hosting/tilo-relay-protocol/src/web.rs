//! The browser carriers of the relay: HTTPS for the durable plane and device
//! certification, WebTransport (`moq-lite`, with an HTTPS fallback) for the
//! live plane. PR 541's carrier model, served by the host itself.
//!
//! HTTPS carries the *same* values as the `tilo-relay/1` stream: the
//! [`auth`](crate::auth) handshake, then [`Envelope`]s both ways. A browser
//! session is one more peer of the host, served by the same per-workspace
//! authority as an iroh peer, so both see one host order.
//!
//! Every body is postcard. Routes (all `POST`, below [`API_PREFIX`]):
//!
//! | Route | Body | Answer |
//! |---|---|---|
//! | `/host` | — | [`HostInfo`] |
//! | `/certify` | `CertificationRequest` | `CertifyReply` |
//! | `/certify/wait` | request digest `[u8; 32]` | `CertifyReply` (long-poll) |
//! | `/session` | [`Hello`] | [`SessionChallenge`] |
//! | `/session/finish` | [`SessionFinish`] | [`SessionGrant`] |
//! | `/send` | `Vec<Envelope>` | `204` |
//! | `/events` | [`EventsRequest`] | [`EventsPage`] (long-poll) |
//! | `/live/grant` | workspace id `u64` | [`LiveGrant`] |
//! | `/live/grant/agent` | [`AgentLiveGrantRequest`] | [`LiveGrant`] for [`agent_publisher`] |
//! | `/live/fallback/{open,send,resume}` | live fallback frames | frame stream |
//!
//! Routes after the handshake carry the session token in
//! [`SESSION_HEADER`] (lowercase hex). A `401` means the session is gone:
//! the device runs the handshake again, resubscribes and resends its outbox.

use serde::{Deserialize, Serialize};

use crate::Envelope;
use crate::auth::{Hello, HelloAck, HelloAck2};

/// Path prefix of every browser carrier route.
pub const API_PREFIX: &str = "/peer/v1";
/// Header carrying the session token.
pub const SESSION_HEADER: &str = "x-tilo-session";
/// WebTransport path of the live relay.
pub const LIVE_PATH: &str = "/moq/live";
/// Longest an `/events` or `/certify/wait` poll is held.
pub const MAX_POLL_MS: u32 = 25_000;
/// Largest request body a route accepts.
pub const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
/// Most envelopes one `/events` answer carries.
pub const MAX_PAGE_ENVELOPES: usize = 256;
/// Payload bytes after which an `/events` answer ends (it always carries at
/// least one envelope). A late join's checkpoint and tail, or a segmented
/// multi-megabyte document, then stream as a series of bounded pages, each
/// acknowledged before the next.
pub const MAX_PAGE_BYTES: usize = 1024 * 1024;

/// Approximate wire size of `envelope`: its opaque payload plus a fixed
/// allowance for keys and framing. Bounds pages and session buffers without
/// encoding every envelope twice.
pub fn envelope_bytes(envelope: &Envelope) -> usize {
    const OVERHEAD: usize = 160;
    let payload = match envelope {
        Envelope::Delta { delta, .. } | Envelope::Commit { delta, .. } => delta.len(),
        Envelope::SyncResponse { delta_since, .. } => delta_since.len(),
        Envelope::Presence { payload, .. } => payload.len(),
        Envelope::Compacted { statement, .. } => statement.len(),
        Envelope::Live { frame, .. } | Envelope::RemoteAction { frame, .. } => frame.len(),
        Envelope::NotAuthorised { reason, .. }
        | Envelope::RemoteActionUndeliverable { reason, .. }
        | Envelope::CommitRefused { reason, .. }
        | Envelope::VoiceRefused { reason, .. } => reason.len(),
        Envelope::VoiceGranted { grant, .. } => grant
            .participants
            .len()
            .saturating_add(grant.subscriptions.len())
            .saturating_mul(200)
            .saturating_add(grant.endpoint.len()),
        Envelope::PeerRelayHello { hosted_workspaces } => hosted_workspaces.len().saturating_mul(8),
        Envelope::SyncRequest { have_heads, .. } => have_heads.len().saturating_mul(32),
        _ => 0,
    };
    payload.saturating_add(OVERHEAD)
}

/// What a page needs to know about the host before it has a certificate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostInfo {
    /// The host's device certificate (pin its device key).
    pub host_cert: tilo_user_identity::DeviceCert,
    /// UDP port of the WebTransport listener (same address as HTTPS).
    pub webtransport_port: u16,
    /// SHA-256 of the TLS certificate (lowercase hex) when it is a
    /// development certificate a browser pins with `serverCertificateHashes`.
    pub certificate_sha256: Option<String>,
}

/// The host's answer to a session [`Hello`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionChallenge {
    /// The same [`HelloAck`] the relay stream answers with.
    pub ack: HelloAck,
    /// Identifies this handshake for [`SessionFinish`].
    pub handshake: [u8; 16],
}

/// The device's signature over the host's challenge.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionFinish {
    /// From [`SessionChallenge::handshake`].
    pub handshake: [u8; 16],
    /// The same [`HelloAck2`] the relay stream carries.
    pub ack2: HelloAck2,
}

/// An authenticated session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionGrant {
    /// Bearer token for [`SESSION_HEADER`].
    pub token: [u8; 32],
    /// The session ends here at the latest; the device then runs the
    /// handshake again.
    pub expires_at_ns: u64,
}

/// The host accepts [`crate::transport::Envelope::SubscribeFrom`] and issues
/// [`crate::transport::Envelope::Position`]s.
pub const FEATURE_RESUME: u64 = 1;

/// The host accepts [`crate::transport::Envelope::CompactVolunteer`] and
/// [`crate::transport::Envelope::Compacted`], and may send volunteers
/// [`crate::transport::Envelope::CompactRequest`].
pub const FEATURE_COMPACTION: u64 = 2;

/// The host governs calls in its rooms: it accepts
/// [`crate::transport::Envelope::VoiceCall`],
/// [`crate::transport::Envelope::VoiceJoin`] and
/// [`crate::transport::Envelope::VoiceLeave`] (see [`crate::voice`]).
pub const FEATURE_VOICE: u64 = 4;

/// Encode a `/session/finish` answer: the [`SessionGrant`] followed by the
/// host's feature bits. A device that predates the features reads the grant
/// and ignores the rest.
///
/// # Errors
///
/// Returns an error if postcard fails to encode.
pub fn encode_grant(grant: &SessionGrant, features: u64) -> Result<Vec<u8>, postcard::Error> {
    let mut bytes = postcard::to_allocvec(grant)?;
    bytes.extend(postcard::to_allocvec(&features)?);
    Ok(bytes)
}

/// Decode a `/session/finish` answer into the grant and the host's feature
/// bits, which are `0` from a host that predates them.
///
/// # Errors
///
/// Returns an error if the grant does not decode.
pub fn decode_grant(bytes: &[u8]) -> Result<(SessionGrant, u64), postcard::Error> {
    let (grant, rest) = postcard::take_from_bytes::<SessionGrant>(bytes)?;
    Ok((grant, postcard::from_bytes(rest).unwrap_or(0)))
}

/// Host-signed position in one session's event stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cursor {
    /// Sequence of the last event covered.
    pub seq: u64,
    /// Host MAC binding `seq` to the session.
    pub mac: [u8; 32],
}

/// One `/events` poll.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventsRequest {
    /// Acknowledge every event up to this cursor; `None` on the first poll.
    pub ack: Option<Cursor>,
    /// Hold the poll this long for new events, at most [`MAX_POLL_MS`].
    pub wait_ms: u32,
}

/// Events after the acknowledged cursor, in host order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventsPage {
    /// Cursor covering the last envelope here (unchanged when empty).
    pub cursor: Option<Cursor>,
    /// The envelopes the relay stream would have delivered.
    pub envelopes: Vec<Envelope>,
}

/// Admission to the live plane of one room.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveGrant {
    /// Opaque grant for the WebTransport admission preface (or the HTTPS
    /// fallback `open`).
    pub token: Vec<u8>,
    /// Room component of the live namespace (64 lowercase hex).
    pub room: String,
    /// This device's publisher component (64 lowercase hex).
    pub publisher: String,
    /// The grant ends here; request a new one before.
    pub expires_at_ns: u64,
}

/// A live grant for an agent this device runs: its presence and typing
/// previews publish under [`agent_publisher`], not the device's own key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentLiveGrantRequest {
    /// The room's workspace, as for `/live/grant`.
    pub workspace_id: u64,
    /// The agent's id on this device, 1..=[`MAX_AGENT_ID_BYTES`] bytes.
    pub agent_id: String,
}

/// Bound of [`AgentLiveGrantRequest::agent_id`].
pub const MAX_AGENT_ID_BYTES: usize = 128;

/// Publisher component of the live namespace of agent `agent_id` run by
/// `device` in `room` (64 hex, [`live_room`]):
/// `blake3(domain ‖ u64le(len room) ‖ room ‖ device ‖ agent_id)`. A digest,
/// never a device key, so an agent's presence cannot pass for a device's.
pub fn agent_publisher(room: &str, device: [u8; 32], agent_id: &str) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(b"tilo-live-agent-publisher-v1\0");
    hash.update(&(room.len() as u64).to_le_bytes());
    hash.update(room.as_bytes());
    hash.update(&device);
    hash.update(agent_id.as_bytes());
    *hash.finalize().as_bytes()
}

/// Room component of the live namespace of `workspace_id`.
pub fn live_room(workspace_id: u64) -> String {
    let mut hash = blake3::Hasher::new();
    hash.update(b"tilo-live-room-v1\0");
    hash.update(&workspace_id.to_le_bytes());
    hex::encode(hash.finalize().as_bytes())
}

/// The first request of a session.
pub type SessionHello = Hello;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_publishers_are_per_room_device_and_agent_and_never_the_device() {
        let room = live_room(7);
        let device = [3u8; 32];
        let agent = agent_publisher(&room, device, "Reading agent");
        assert_eq!(agent, agent_publisher(&room, device, "Reading agent"));
        assert_ne!(agent, device);
        assert_ne!(agent, agent_publisher(&room, device, "Other agent"));
        assert_ne!(agent, agent_publisher(&room, [4u8; 32], "Reading agent"));
        assert_ne!(
            agent,
            agent_publisher(&live_room(8), device, "Reading agent")
        );
    }
}
