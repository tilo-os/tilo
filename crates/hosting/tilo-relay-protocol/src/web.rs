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
//! | `/node` | — | [`NodeInfo`] |
//! | `/certify` | `CertificationRequest` | `CertifyReply` |
//! | `/certify/wait` | request digest `[u8; 32]` | `CertifyReply` (long-poll) |
//! | `/session` | [`Hello`] | [`SessionChallenge`] |
//! | `/session/finish` | [`SessionFinish`] | [`SessionGrant`] |
//! | `/send` | `Vec<Envelope>` | `204` |
//! | `/events` | [`EventsRequest`] | [`EventsPage`] (long-poll) |
//! | `/live/grant` | workspace id `u64` | [`LiveGrant`] |
//! | `/live/grant/agent` | [`AgentLiveGrantRequest`] | [`LiveGrant`] for [`agent_publisher`] |
//! | `/live/legacy-input` | [`InputRequest`](crate::legacy_window::InputRequest) | `204`, `403` when the member may not drive the window |
//! | `/live/fallback/{open,send,resume}` | live fallback frames | frame stream |
//! | `/rooms` | [`RoomsRequest`](crate::rooms::RoomsRequest) | [`RoomsReply`](crate::rooms::RoomsReply) |
//! | `/join` | [`JoinStep`](crate::rooms::JoinStep) | [`JoinReply`](crate::rooms::JoinReply) |
//!
//! A host started without a certificate serves only `/install` until it has
//! one: the body is the root-signed `DeviceCert` for its device key as JSON
//! (what a page's root issues) or in its canonical encoding, or
//! [`InstallCertificates`], with the install token the host was
//! provisioned with in [`INSTALL_TOKEN_HEADER`]. `200` answers
//! [`Installed`] as JSON; every other route answers `503` meanwhile. A
//! `GET` with the token answers [`InstallInfo`]: the host's key and, on a
//! node, the key of the session beside it, which the root certifies at the
//! same time as one of the person's devices.
//!
//! `/join` is the one route after `/certify` a device calls without a
//! session: a person a host does not know yet joins through it. Routes after
//! the handshake carry the session token in
//! [`SESSION_HEADER`] (lowercase hex). A `401` means the session is gone:
//! the device runs the handshake again, resubscribes and resends its outbox.

use serde::{Deserialize, Serialize};

use crate::Envelope;
use crate::auth::{Hello, HelloAck, HelloAck2};

/// Path prefix of every browser carrier route.
pub const API_PREFIX: &str = "/peer/v1";
/// Header carrying the session token.
pub const SESSION_HEADER: &str = "x-tilo-session";
/// Header carrying the install token of a host awaiting its certificate.
pub const INSTALL_TOKEN_HEADER: &str = "x-tilo-install-token";
/// The route that installs a waiting host's certificate.
pub const INSTALL_ROUTE: &str = "/install";

/// What a waiting host says about itself on `GET /install` (JSON).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallInfo {
    /// The host's device key, hex.
    pub device_public_key: String,
    /// On a node, the session that runs the person's apps and agents beside
    /// the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<NodeSession>,
}

/// The device key of the session beside a node, vouched for by the node.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeSession {
    /// The session's device key, hex.
    pub device_public_key: String,
    /// The node's device key's Ed25519 signature over
    /// [`node_session_message`], hex. A page that got the node's key out of
    /// band certifies the session's key only when this verifies.
    pub signature: String,
}

/// What a node signs to vouch for the session beside it.
#[must_use]
pub fn node_session_message(node: &[u8; 32], session: &[u8; 32]) -> Vec<u8> {
    let mut message = b"tilo node session v1\0".to_vec();
    message.extend_from_slice(node);
    message.extend_from_slice(session);
    message
}

/// `POST /install` with the certificates of a node and of the session
/// beside it (JSON): each in its canonical encoding, base64, as a page's
/// `certifyDevice` resolves.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallCertificates {
    /// The host's certificate.
    pub certificate: String,
    /// The session's certificate, when the host has one beside it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_certificate: Option<String>,
}

/// A waiting host's answer to an accepted `/install` (JSON).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    /// The host's device key, hex.
    pub device_public_key: String,
    /// The identity it now serves, hex.
    pub user_public_key: String,
}

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
        Envelope::VoiceCallList { result, .. } => match result {
            Ok(calls) => calls.iter().fold(0usize, |bytes, call| {
                bytes.saturating_add(
                    call.participants
                        .len()
                        .saturating_add(1)
                        .saturating_mul(200),
                )
            }),
            Err(reason) => reason.len(),
        },
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

/// What a node runs beside its relay, for a member's session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeInfo {
    /// The device of the session that runs the person's apps (legacy apps
    /// among them) on the node: the device a page asks to open one.
    pub session: Option<[u8; 32]>,
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
/// [`crate::transport::Envelope::VoiceJoin`],
/// [`crate::transport::Envelope::VoiceLeave`] and
/// [`crate::transport::Envelope::VoiceCalls`] (see [`crate::voice`]).
pub const FEATURE_VOICE: u64 = 4;

/// The host governs rooms a page administers and joins over HTTPS
/// (`/rooms`, `/join`; see [`crate::rooms`]).
pub const FEATURE_ROOMS: u64 = 8;

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
