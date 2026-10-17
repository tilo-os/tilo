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
//! | `/live/legacy-codecs` | [`CodecsRequest`](crate::legacy_window::CodecsRequest) | `204`, `403` when not admitted |
//! | `/live/fallback/{open,send,resume}` | live fallback frames | frame stream |
//! | `/rooms` | [`RoomsRequest`](crate::rooms::RoomsRequest) | [`RoomsReply`](crate::rooms::RoomsReply) |
//! | `/join` | [`JoinStep`](crate::rooms::JoinStep) | [`JoinReply`](crate::rooms::JoinReply) |
//! | `/here` ([`HERE_PATH`]) | [`HereRequest`] | [`HereReply`]; `400` out of bounds, `403` when not admitted |
//! | `/channel-keys` ([`CHANNEL_KEYS_PATH`]) | [`ChannelKeysRequest`] | [`ChannelKeys`]; `403` when not admitted |
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
        Envelope::Live { frame, .. }
        | Envelope::RemoteAction { frame, .. }
        | Envelope::RecordContent { frame, .. } => frame.len(),
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
    /// UDP port of the WebTransport listener (same address as HTTPS), or
    /// `None` when browsers reach the host through a proxy that publishes
    /// only HTTPS: a page then opens the live plane over the HTTPS stream at
    /// once instead of first failing a WebTransport handshake.
    pub webtransport_port: Option<u16>,
    /// SHA-256 of the TLS certificate (lowercase hex) when it is a
    /// development certificate a browser pins with `serverCertificateHashes`.
    pub certificate_sha256: Option<String>,
    /// The `https://` origin to dial WebTransport at when it is not the
    /// page's: a proxy in front of the page passes HTTP only, so QUIC
    /// reaches the host directly under another name. The HTTPS stream
    /// fallback stays on the page origin.
    pub webtransport_origin: Option<String>,
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

/// Route a spectator of a public room takes a channel's content keys from,
/// to open what the room's members sealed there. Only a device of the room's
/// spectator person is answered, only in a public room and only while it is
/// subscribed; a member is admitted to a channel by its steward instead.
pub const CHANNEL_KEYS_PATH: &str = "/channel-keys";

/// The record whose channel's content keys a spectator asks for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelKeysRequest {
    /// The record's app.
    pub app_id: String,
    /// The room (the record's cluster).
    pub room: String,
    /// The record id.
    pub record: String,
}

/// A channel's content keys, in their endpoint encoding
/// (`channel_crypto::ChannelContentKeys::to_bytes`).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelKeys {
    /// The encoded keys.
    pub keys: Vec<u8>,
}

impl core::fmt::Debug for ChannelKeys {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ChannelKeys").finish_non_exhaustive()
    }
}

/// Route of "who is here and what are they doing" in a workspace: a member
/// posts their own recent semantic actions and gets everyone's line back.
pub const HERE_PATH: &str = "/here";
/// Bound of [`HereRequest::actions`].
pub const MAX_HERE_ACTIONS: usize = 16;
/// Bound, in characters, of each text field of a [`HereAction`].
pub const MAX_HERE_FIELD_CHARS: usize = 96;
/// Bound, in characters, of [`HerePerson::status`]. The same bound as a
/// person's typed or inferred status line in the UI (`tilo-ui`'s
/// `live_collaboration::MAX_STATUS_CHARS`), so a host line fits wherever a
/// status shows.
pub const MAX_HERE_STATUS_CHARS: usize = 96;
/// Bound, in characters, of [`HereRequest::label`] and [`HerePerson::label`].
pub const MAX_HERE_LABEL_CHARS: usize = 32;
/// Bound, in characters, of [`HerePerson::recent`].
pub const MAX_HERE_RECENT_CHARS: usize = 320;
/// How long a person stays listed after their last post, in seconds: the
/// lifetime of an inferred status in the UI (`tilo-ui`'s
/// `live_collaboration::INFERRED_STATUS_TTL`).
pub const HERE_TTL_SECS: u64 = 5 * 60;

/// One semantic action a person took: the titles of the action, the record
/// it acted on and the app, never keystrokes or bodies.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HereAction {
    /// The action's id; a summary cites it.
    pub id: [u8; 16],
    /// The action's title, e.g. "Edited".
    pub action: String,
    /// The record's title, e.g. "Welcome note".
    pub resource: String,
    /// The app's name, e.g. "Notes".
    pub app: String,
    /// When it happened, Unix milliseconds.
    pub at_ms: u64,
}

/// A person's own recent semantic actions in one workspace (titles of
/// actions and records only; never keystrokes or bodies). Each post is the
/// poster's whole current set, so empty `actions` lists them where they are
/// with no line.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HereRequest {
    /// The workspace, which the session's person must be admitted to.
    pub workspace_id: u64,
    /// The poster's own display name, at most [`MAX_HERE_LABEL_CHARS`]
    /// characters, possibly empty. Self-asserted like a live participant
    /// label: shown, never trusted for anything else.
    pub label: String,
    /// The tab id of the desktop window the poster has focused in the
    /// room's shared desktop, if any. The desktop tree is replicated, so the
    /// id means the same window to every member.
    pub window: Option<u64>,
    /// Their recent actions, at most [`MAX_HERE_ACTIONS`].
    pub actions: Vec<HereAction>,
}

impl HereRequest {
    /// Whether the request is within the published bounds: at most
    /// [`MAX_HERE_ACTIONS`] actions with distinct ids, each with a non-empty
    /// action title and text fields of at most [`MAX_HERE_FIELD_CHARS`]
    /// characters without control characters. A host refuses a request past
    /// them rather than shortening it, which would summarise something the
    /// person did not send.
    ///
    /// # Errors
    ///
    /// Names the first bound broken.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.label.chars().count() > MAX_HERE_LABEL_CHARS {
            return Err("the label is too long");
        }
        if self.label.chars().any(char::is_control) {
            return Err("the label holds a control character");
        }
        if self.actions.len() > MAX_HERE_ACTIONS {
            return Err("too many actions");
        }
        for (index, action) in self.actions.iter().enumerate() {
            if action.action.trim().is_empty() {
                return Err("an action needs a title");
            }
            for field in [&action.action, &action.resource, &action.app] {
                if field.chars().count() > MAX_HERE_FIELD_CHARS {
                    return Err("an action field is too long");
                }
                if field.chars().any(char::is_control) {
                    return Err("an action field holds a control character");
                }
            }
            if self
                .actions
                .iter()
                .take(index)
                .any(|seen| seen.id == action.id)
            {
                return Err("action ids repeat");
            }
        }
        Ok(())
    }
}

/// Everyone listed in a workspace now, the caller included.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HereReply {
    /// Ordered by device key.
    pub people: Vec<HerePerson>,
}

/// One person on one device and what they are doing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HerePerson {
    /// The session's authenticated device key (what the live plane calls
    /// the publisher).
    pub device: [u8; 32],
    /// The person's user root.
    pub user: [u8; 32],
    /// The display name of their latest post; self-asserted, display only.
    pub label: String,
    /// The window of their latest post ([`HereRequest::window`]).
    pub window: Option<u64>,
    /// One line, present tense, e.g. "Drafting the welcome note in Notes";
    /// at most [`MAX_HERE_STATUS_CHARS`] characters.
    pub status: String,
    /// What they did in the last minutes, at most [`MAX_HERE_RECENT_CHARS`]
    /// characters.
    pub recent: String,
    /// The action ids the text cites.
    pub sources: Vec<[u8; 16]>,
    /// `true`: model prose that passed grounding; `false`: the host's
    /// deterministic summary.
    pub generated: bool,
    /// When the line last changed, Unix milliseconds.
    pub updated_ms: u64,
}

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

    #[test]
    fn here_requests_past_their_bounds_are_refused() {
        let action = |id: u8, title: &str| HereAction {
            id: [id; 16],
            action: title.into(),
            resource: "Welcome note".into(),
            app: "Notes".into(),
            at_ms: 1,
        };
        let request = |actions| HereRequest {
            workspace_id: 1,
            label: "Ada".into(),
            window: None,
            actions,
        };
        assert!(request(Vec::new()).validate().is_ok());
        let full: Vec<_> = (0..16u8).map(|id| action(id, "Edited")).collect();
        assert_eq!(full.len(), MAX_HERE_ACTIONS);
        assert!(request(full.clone()).validate().is_ok());
        let mut over = full;
        over.push(action(200, "Edited"));
        assert!(request(over).validate().is_err());
        let long = "é".repeat(MAX_HERE_FIELD_CHARS);
        assert!(request(vec![action(1, &long)]).validate().is_ok());
        let longer = format!("{long}x");
        for refused in [
            vec![action(1, &longer)],
            vec![action(1, " ")],
            vec![action(1, "Edited\nnow")],
            vec![action(1, "Edited"), action(1, "Opened")],
        ] {
            assert!(request(refused).validate().is_err());
        }
        for label in ["x".repeat(MAX_HERE_LABEL_CHARS + 1), "A\u{7}da".into()] {
            let mut labelled = request(Vec::new());
            labelled.label = label;
            assert!(labelled.validate().is_err());
        }
    }
}
