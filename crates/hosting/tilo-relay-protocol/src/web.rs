//! The browser carrier of the relay: HTTPS for the durable plane, presence
//! and calls. Served by the host itself.
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
//! | `/session` | [`Hello`](crate::auth::Hello) | [`SessionChallenge`]; `403` [`SessionRefusal`] |
//! | `/session/finish` | [`SessionFinish`] | [`SessionGrant`]; `403` [`SessionRefusal`] |
//! | `/session/end` ([`SESSION_END_PATH`]) | the session token, lowercase hex text | `204` |
//! | `/send` | `Vec<Envelope>` | `204` |
//! | `/events` | [`EventsRequest`] | [`EventsPage`] (long-poll) |
//! | `/rooms` | [`WorkspacesRequest`](crate::workspaces::RoomsRequest) | [`WorkspacesReply`](crate::workspaces::RoomsReply) |
//! | `/join` | [`JoinStep`](crate::workspaces::JoinStep) | [`JoinReply`](crate::workspaces::JoinReply) |
//! | `/here` ([`HERE_PATH`]) | [`HereRequest`] | [`HereReply`]; `400` out of bounds, `403` when not admitted |
//! | `/live` ([`LIVE_INFO_PATH`]) | [`LiveRequest`] | [`LiveInfo`]; `403` when the page's device is not admitted |
//! | `/fetch` ([`FETCH_PATH`]) | any method: `tilo_net::proxy` wire protocol v1 | the target's response, or the proxy's failure |
//!
//! `/fetch` is the host's fetch proxy, offered when the session grant
//! carries [`FEATURE_FETCH_PROXY`]: an admitted member's page reads, through
//! its host, the sites that send no CORS headers. Unlike every other route it
//! takes the method of the request it relays, and its session token in
//! [`SESSION_HEADER`] as the others do.
//!
//! `/workspace-join` is the host's workspace join exchange, the one its
//! iroh endpoint serves on the workspace-join ALPN, for a native device that
//! reaches the host over HTTPS only: it takes and answers exactly the
//! requests and responses the iroh exchange does. Over iroh the transport
//! authenticates the requesting device; here each request is bound to the
//! device it names, whose signature its proof carries, as `/join` binds a
//! page's steps.
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
//! `/join` and `/workspace-join` are the routes after `/certify` a device
//! calls without a session: a person a host does not know yet joins through
//! them. Routes after
//! the handshake carry the session token in
//! [`SESSION_HEADER`] (lowercase hex). A `401` means the session is gone:
//! the device runs the handshake again, resubscribes and resends its outbox.
//!
//! `/session/end` takes its token as the body instead, so a page that is
//! going away can say so with `navigator.sendBeacon`, which sends no
//! headers of its own: the host ends that session at once rather than when
//! its polls stop, and the people in the workspace stop counting the page.

use serde::{Deserialize, Serialize};

use crate::Envelope;
use crate::auth::{HelloAck, HelloAck2};

/// Path prefix of every browser carrier route.
pub const API_PREFIX: &str = "/peer/v1";
/// Header carrying the session token.
pub const SESSION_HEADER: &str = "x-tilo-session";
/// Where a page ends its session as it leaves, the token its body.
pub const SESSION_END_PATH: &str = "/session/end";
/// Header carrying the install token of a host awaiting its certificate.
pub const INSTALL_TOKEN_HEADER: &str = "x-tilo-install-token";
/// The route that installs a waiting host's certificate.
pub const INSTALL_ROUTE: &str = "/install";

/// What a waiting host says about itself on `GET /install` (JSON).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstallInfo {
    pub device_public_key: String,
    /// On a node, the session that runs the person's apps and agents beside
    /// the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<NodeSession>,
}

/// The device key of the session beside a node, vouched for by the node.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeSession {
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
    pub certificate: String,
    /// The session's certificate, when the host has one beside it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_certificate: Option<String>,
}

/// A waiting host's answer to an accepted `/install` (JSON).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    pub device_public_key: String,
    /// The identity it now serves, hex.
    pub user_public_key: String,
}

/// Longest an `/events` or `/certify/wait` poll is held.
pub const MAX_POLL_MS: u32 = 25_000;
/// Largest request body a route accepts.
pub const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
/// Most envelopes one `/events` answer carries. A late join's tail is
/// thousands of small envelopes (a few hundred bytes each), and every page
/// costs the browser a round trip to the host: the count must let
/// [`MAX_PAGE_BYTES`] end a page of small envelopes, not cut it short. At
/// 256 a Lounge with ~4000 envelopes of history took 16 pages, 8 s over a
/// 450 ms route, before the page could open.
pub const MAX_PAGE_ENVELOPES: usize = 8192;
// Envelopes of 128 payload bytes (plus the per-envelope allowance) fill a
// page by bytes before by count.
const _: () = assert!(MAX_PAGE_ENVELOPES * (128 + ENVELOPE_OVERHEAD) >= MAX_PAGE_BYTES);
/// Payload bytes after which an `/events` answer ends (it always carries at
/// least one envelope). A late join's checkpoint and tail, or a segmented
/// multi-megabyte document, then stream as a series of bounded pages, each
/// acknowledged before the next.
pub const MAX_PAGE_BYTES: usize = 1024 * 1024;

/// What [`envelope_bytes`] allows each envelope for its keys and framing.
const ENVELOPE_OVERHEAD: usize = 160;

/// Approximate wire size of `envelope`: its opaque payload plus a fixed
/// allowance for keys and framing. Bounds pages and session buffers without
/// encoding every envelope twice.
pub fn envelope_bytes(envelope: &Envelope) -> usize {
    let payload = match envelope {
        Envelope::Delta { delta, .. } | Envelope::Commit { delta, .. } => delta.len(),
        Envelope::SyncResponse { delta_since, .. } => delta_since.len(),
        Envelope::RetiredPresence { payload, .. } => payload.len(),
        Envelope::Compacted { statement, .. } => statement.len(),
        Envelope::RetiredLive { frame, .. }
        | Envelope::RemoteAction { frame, .. }
        | Envelope::HostAction { frame }
        | Envelope::RecordContentWant { frame, .. }
        | Envelope::RecordContent { frame, .. } => frame.len(),
        Envelope::WorkspaceBootstrap { bootstrap, .. } => bootstrap.len(),
        Envelope::NotAuthorised { reason, .. }
        | Envelope::RemoteActionUndeliverable { reason, .. }
        | Envelope::CommitRefused { reason, .. }
        | Envelope::VoiceRefused { reason, .. } => reason.len(),
        Envelope::VoiceGranted { grant, .. } => grant.participants.len().saturating_mul(200),
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
    payload.saturating_add(ENVELOPE_OVERHEAD)
}

/// What a page needs to know about the host before it has a certificate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostInfo {
    /// The host's device certificate (pin its device key).
    pub host_cert: tilo_user_identity::DeviceCert,
    /// Where the host listens on iroh, which is how a page reaches it for
    /// everything a session and its planes carry. `None` in a host that serves
    /// no iroh endpoint.
    pub address: Option<HostAddress>,
}

/// Where a host's iroh endpoint is dialled.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostAddress {
    /// The endpoint's key and the addresses it answers at.
    pub endpoint_addr: iroh_base::EndpointAddr,
    /// The packet relay it is reached through, when it names one; a page dials
    /// the endpoint through it rather than over the network's own relays.
    pub relay_url: Option<String>,
}

/// What a node runs beside its relay, for a member's session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeInfo {
    /// The device of the session that runs the person's apps (legacy apps
    /// among them) on the node: the device a page asks to open one.
    pub session: Option<[u8; 32]>,
}

/// The host's answer to a session [`Hello`](crate::auth::Hello).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionChallenge {
    /// The same [`HelloAck`] the relay stream answers with.
    pub ack: HelloAck,
    /// Identifies this handshake for [`SessionFinish`].
    pub handshake: [u8; 16],
}

/// Why the host turned a session handshake away: the body of a `403` from
/// `/session` or `/session/finish`, so a device decides what to do from the
/// host's own reason rather than from how often it was refused.
///
/// The handshake checks the device's certificate, the host's revocations
/// and its live trust policy, which is one allow list for the whole host.
/// So it tells a revoked device apart from a person the host does not admit,
/// but not why it does not admit them: one it never knew, one it forgot (its
/// state was reset, or it pruned them after an absence) and one a workspace
/// removed or banned all read [`Self::NotAdmitted`]. Whether such a person
/// may come back is the workspace's door to answer, not the handshake's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionRefusal {
    /// The host admits nobody of this device's person: joining again, where
    /// the workspace lets them, admits them anew.
    NotAdmitted,
    /// This device is on the host's revocation list. It never comes back;
    /// its person's other devices are unaffected.
    DeviceRevoked,
    /// The certificate did not verify, or was not valid at the host's time:
    /// a newly issued certificate for the same device may be admitted.
    CertificateInvalid,
    /// The handshake itself failed: it was unknown or too old by its second
    /// step, or the challenge signature did not verify. A new handshake may
    /// succeed.
    Handshake,
}

impl SessionRefusal {
    /// The refusal for a failed [`crate::auth`] check.
    #[must_use]
    pub fn from_auth(error: &crate::auth::AuthError) -> Self {
        use crate::auth::AuthError;
        match error {
            AuthError::Revoked => Self::DeviceRevoked,
            AuthError::PolicyReject => Self::NotAdmitted,
            AuthError::Cert(_) => Self::CertificateInvalid,
            AuthError::BadChallengeSig | AuthError::EndpointMismatch => Self::Handshake,
        }
    }
}

/// The device's signature over the host's challenge.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionFinish {
    /// From [`SessionChallenge::handshake`].
    pub handshake: [u8; 16],
    /// The same [`HelloAck2`] the relay stream carries.
    pub ack2: HelloAck2,
    /// The device's page keeps this session beside the workspace it is in, only
    /// to deliver for it: nobody is online by it, so it takes no seat and
    /// shows no face, and its person counts only by another session.
    pub background: bool,
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

/// The host governs calls in its workspaces: it accepts
/// [`crate::transport::Envelope::VoiceCall`],
/// [`crate::transport::Envelope::VoiceJoin`],
/// [`crate::transport::Envelope::VoiceLeave`],
/// [`crate::transport::Envelope::VoiceShow`] and
/// [`crate::transport::Envelope::VoiceCalls`] (see [`crate::voice`]).
pub const FEATURE_VOICE: u64 = 4;

/// The host governs workspaces a page administers and joins over HTTPS
/// (`/rooms`, `/join`; see [`crate::workspaces`]).
pub const FEATURE_ROOMS: u64 = 8;

/// The host serves its members' pages a fetch proxy at [`FETCH_PATH`]
/// (`tilo_net::proxy`, wire protocol v1).
pub const FEATURE_FETCH_PROXY: u64 = 16;

/// The host's fetch proxy, below [`API_PREFIX`].
pub const FETCH_PATH: &str = "/fetch";

/// Encode a `/session/finish` answer: the [`SessionGrant`] followed by the
/// host's feature bits.
pub fn encode_grant(grant: &SessionGrant, features: u64) -> Result<Vec<u8>, postcard::Error> {
    let mut bytes = postcard::to_allocvec(grant)?;
    bytes.extend(postcard::to_allocvec(&features)?);
    Ok(bytes)
}

/// Decode a `/session/finish` answer into the grant and the host's feature
/// bits.
pub fn decode_grant(bytes: &[u8]) -> Result<(SessionGrant, u64), postcard::Error> {
    let (grant, rest) = postcard::take_from_bytes::<SessionGrant>(bytes)?;
    Ok((grant, postcard::from_bytes(rest)?))
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

/// Route of the live plane: how a page learns where the relay's WebTransport
/// listener is and which certificate to pin for it.
pub const LIVE_INFO_PATH: &str = "/live";

/// A page's ask to join `workspace_id`'s live plane. The host answers only
/// when the page's session device is admitted to the workspace now, so the
/// answer is also the admission the page dials with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveRequest {
    /// The workspace as this host numbers it, for the admission decision.
    pub workspace_id: u64,
    /// The workspace's own name, which its live namespace carries: the same
    /// string every device of the workspace names its tracks by. The host
    /// checks it against [`tilo_relay_protocol::room_workspace_id_matches`].
    pub workspace: String,
}

/// Where a page joins the live plane: the relay's WebTransport listener, its
/// certificate, and the origin to dial it at.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveInfo {
    /// The UDP port of the relay's WebTransport listener.
    pub webtransport_port: u16,
    /// Base64 SHA-256 of the listener's leaf certificate: what a page pins as
    /// `serverCertificateHashes` (a development or self-signed certificate is
    /// trusted by no root store).
    pub certificate_sha256: String,
    /// `https://<host>:<port>`: the origin the page dials, with the route and
    /// the `moq-lite` subprotocol added by the page's carrier.
    pub webtransport_origin: String,
}

/// The one length of an encoded [`LiveGrant`]: the version, the workspace as
/// a little-endian `u64`, and the page's session token.
pub const LIVE_GRANT_BYTES: usize = 1 + 8 + 32;

/// The version an encoded [`LiveGrant`] starts with.
const LIVE_GRANT_VERSION: u8 = 2;

/// What a page admits itself to the live plane with: the workspace as the host
/// numbers it, and the token of the page's HTTPS session. The host answers
/// [`LIVE_INFO_PATH`] only to a device the workspace admits, so the grant is
/// the admission itself; the WebTransport listener takes it as its proof
/// because a page holds no device certificate where its network stack runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiveGrant {
    /// The workspace as the host numbers it.
    pub workspace_id: u64,
    /// The page's HTTPS session token, which every route carries as hex.
    pub token: [u8; 32],
}

impl LiveGrant {
    /// The grant of the page whose HTTPS session token is `session` (hex, as
    /// the routes carry it) to `workspace_id`'s live plane, or `None` when
    /// `session` is not a session token.
    #[must_use]
    pub fn from_session(workspace_id: u64, session: &str) -> Option<Self> {
        let mut token = [0_u8; 32];
        hex::decode_to_slice(session, &mut token).ok()?;
        Some(Self {
            workspace_id,
            token,
        })
    }

    /// The grant's bytes: the live carrier's admission proof.
    #[must_use]
    pub fn encode(self) -> [u8; LIVE_GRANT_BYTES] {
        let mut bytes = [0_u8; LIVE_GRANT_BYTES];
        let fields = [LIVE_GRANT_VERSION]
            .into_iter()
            .chain(self.workspace_id.to_le_bytes())
            .chain(self.token);
        for (slot, field) in bytes.iter_mut().zip(fields) {
            *slot = field;
        }
        bytes
    }

    /// The grant `proof` carries, or `None` for any other proof.
    #[must_use]
    pub fn decode(proof: &[u8]) -> Option<Self> {
        let proof = <[u8; LIVE_GRANT_BYTES]>::try_from(proof).ok()?;
        let (&version, rest) = proof.split_first()?;
        if version != LIVE_GRANT_VERSION {
            return None;
        }
        let (workspace_id, token) = rest.split_at(core::mem::size_of::<u64>());
        Some(Self {
            workspace_id: u64::from_le_bytes(workspace_id.try_into().ok()?),
            token: token.try_into().ok()?,
        })
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
    /// workspace's shared desktop, if any. The desktop tree is replicated, so the
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
    /// When the person last changed the profile this host keeps for them
    /// (their name and picture), Unix milliseconds; 0 when it keeps none.
    /// A page fetches the profile again when it changes.
    pub profile_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_page_grant_names_its_workspace_and_token() {
        let grant = LiveGrant {
            workspace_id: 7,
            token: [9; 32],
        };
        assert_eq!(grant.encode().len(), LIVE_GRANT_BYTES);
        assert_eq!(LiveGrant::decode(&grant.encode()), Some(grant));
        assert!(LiveGrant::decode(&[]).is_none());
        assert!(LiveGrant::decode(&[1; LIVE_GRANT_BYTES]).is_none());
        assert!(LiveGrant::decode(&[2; LIVE_GRANT_BYTES.saturating_add(1)]).is_none());
    }

    #[test]
    fn a_session_token_becomes_the_pages_grant() {
        let session = hex::encode([9; 32]);
        assert_eq!(
            LiveGrant::from_session(7, &session),
            Some(LiveGrant {
                workspace_id: 7,
                token: [9; 32],
            })
        );
        assert!(LiveGrant::from_session(7, "9").is_none());
        assert!(LiveGrant::from_session(7, "zz".repeat(32).as_str()).is_none());
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
