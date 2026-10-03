//! Bounded inert `LIVE_V1` wire values: ephemeral multiplayer state (presence,
//! cursors, selections, drags, text previews) and early forwarding of signed
//! durable changes, carried as `moq-lite` tracks beside voice.
//!
//! Live frames are never durable and never authority. The durable delivery
//! carrier (state upload, replay, cursors) stays the only source of truth; a
//! forwarded [`OpsFrame::SignedChange`] carries the original canonical bytes and
//! must pass the same admission as replay before a receiver applies it.
//! Decoding never grants authority; the host verifies grants and roster.
use serde::{Deserialize, Deserializer, Serialize, de};
use tilo_user_identity::canonical::signing_digest;

/// Signed media-capability bit advertising the installed live service.
pub const MEDIA_LIVE_V1: u64 = 2;
/// Exact WebTransport path of the live relay on the host's QUIC endpoint.
pub const LIVE_WEBTRANSPORT_PATH: &str = "/moq/live";
/// WebTransport subprotocol; identical to voice (`moq-lite` draft 06).
pub const LIVE_PROTOCOL: &str = "moq-lite-06";
/// Proof-required HTTPS route that issues a [`SignedLiveGrant`] to a data session.
pub const LIVE_GRANT_TARGET: &str = "/peer/v3/live/grant";
/// First path component of every live broadcast.
pub const LIVE_NAMESPACE_ROOT: &str = "tilo-live";
/// Latest-value datagram track for presence, cursors, selections and drags.
pub const PRESENCE_TRACK: &str = "presence";
/// Prefix of the ordered per-record stream track (`ops/<hex record digest>`).
pub const OPS_TRACK_PREFIX: &str = "ops/";
/// Live grant signature domain.
pub const LIVE_GRANT_DOMAIN: &[u8] = b"tilo-live-grant-v1\0";
/// Grant lifetime ceiling; clients renew before expiry through [`LIVE_GRANT_TARGET`].
pub const GRANT_LIFETIME_NS: u64 = 60_000_000_000;
/// Grant request/response ceiling.
pub const MAX_GRANT_BYTES: usize = 2048;
/// A presence frame must fit one QUIC datagram on common paths.
pub const MAX_DATAGRAM_FRAME_BYTES: usize = 1100;
/// Ordered stream frame ceiling (text previews, forwarded signed changes).
pub const MAX_STREAM_FRAME_BYTES: usize = 64 * 1024;
/// Per-publisher presence/drag rate ceiling enforced by the relay (frames/s).
pub const MAX_PRESENCE_RATE: u32 = 120;
/// Per-publisher ordered frame rate ceiling enforced by the relay (frames/s).
pub const MAX_OPS_RATE: u32 = 240;
/// Records one publisher may have open ops tracks for at once.
pub const MAX_OPEN_RECORDS: usize = 16;
/// Text preview payload ceiling per frame.
pub const MAX_PREVIEW_TEXT_BYTES: usize = 1024;
/// Text preview operations per frame.
pub const MAX_PREVIEW_OPS: usize = 32;
/// Opaque object identifier ceiling (desktop window, shape, block).
pub const MAX_OBJECT_ID_BYTES: usize = 64;

/// Closed decoding/validation failures; no values or allocations.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LiveWireError {
    /// Malformed, oversized, truncated or noncanonical encoding.
    #[error("invalid live wire encoding")]
    Codec,
    /// Invalid bounded inert field.
    #[error("invalid live wire field")]
    Invalid,
}
type Result<T> = std::result::Result<T, LiveWireError>;

fn nonzero(value: [u8; 32]) -> Result<()> {
    if value == [0; 32] {
        Err(LiveWireError::Invalid)
    } else {
        Ok(())
    }
}
fn object_id(bytes: &[u8]) -> Result<()> {
    if bytes.is_empty() || bytes.len() > MAX_OBJECT_ID_BYTES {
        Err(LiveWireError::Invalid)
    } else {
        Ok(())
    }
}
fn finite(values: &[f32]) -> Result<()> {
    if values.iter().all(|v| v.is_finite()) {
        Ok(())
    } else {
        Err(LiveWireError::Invalid)
    }
}

/// Requested or authorized live directions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveDirections {
    /// Permission to publish presence and ops.
    pub publish: bool,
    /// Permission to subscribe to other members' live tracks.
    pub subscribe: bool,
}

/// Exact live namespace: `(LIVE_NAMESPACE_ROOT, room, publisher)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveNamespace {
    /// blake3 of the native room identifier; never the raw room text.
    pub room: [u8; 32],
    /// Publishing device key.
    pub publisher: [u8; 32],
}
impl LiveNamespace {
    /// Namespace for `room` and `publisher`.
    pub fn new(room: &str, publisher: [u8; 32]) -> Self {
        Self {
            room: room_digest(room),
            publisher,
        }
    }
    /// Broadcast path components in order (joined with `/`).
    pub fn components(&self) -> [String; 3] {
        [
            LIVE_NAMESPACE_ROOT.into(),
            hex::encode(self.room),
            hex::encode(self.publisher),
        ]
    }
}
/// Room component used in live namespaces.
pub fn room_digest(room: &str) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(b"tilo-live-room-v1\0");
    hash.update(room.as_bytes());
    *hash.finalize().as_bytes()
}
/// Ordered ops track name for one record reference.
pub fn ops_track(record: &LiveRecord) -> String {
    format!("{OPS_TRACK_PREFIX}{}", hex::encode(record.digest()))
}

/// Inert grant request sent with a request proof on [`LIVE_GRANT_TARGET`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveGrantRequest {
    /// Exactly one.
    pub version: u8,
    /// Enrolled device key; must equal the authenticated data session device.
    pub device: [u8; 32],
    /// Native room identifier.
    #[serde(deserialize_with = "text128")]
    pub room: String,
    /// Fresh client nonce binding one relay session.
    pub participation_id: [u8; 32],
    /// Requested directions (viewers may subscribe only).
    pub directions: LiveDirections,
}
impl LiveGrantRequest {
    /// Structural bounds only.
    ///
    /// # Errors
    ///
    /// Returns an error if the value fails validation or verification.
    pub fn validate(&self) -> Result<()> {
        if self.version != 1
            || self.room.is_empty()
            || self.room.len() > 128
            || !(self.directions.publish || self.directions.subscribe)
        {
            return Err(LiveWireError::Invalid);
        }
        nonzero(self.device)?;
        nonzero(self.participation_id)
    }
}

/// Grant claims; unverified until the relay checks host signature and roster.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveGrantMetadata {
    /// Participant root key.
    pub root: [u8; 32],
    /// Exact admitted request.
    pub request: LiveGrantRequest,
    /// Governed authorization revision at issue; the relay re-checks roster.
    pub membership_generation: u64,
    /// Publisher namespace this grant may publish into.
    pub namespace: LiveNamespace,
    /// Exact relay audience (origin + [`LIVE_WEBTRANSPORT_PATH`]).
    #[serde(deserialize_with = "text512")]
    pub audience: String,
    /// Host signer device key.
    pub host: [u8; 32],
    /// Host live authority boot identifier.
    pub host_boot: [u8; 32],
    /// Absolute issue time.
    pub issued_at_ns: u64,
    /// Absolute expiry, at most [`GRANT_LIFETIME_NS`] after issue.
    pub expires_at_ns: u64,
}

/// Host-signed live grant; decode is not signature verification.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedLiveGrant {
    /// Exactly one.
    pub version: u8,
    /// Grant claims.
    pub metadata: LiveGrantMetadata,
    /// Ed25519 signature over [`SignedLiveGrant::signing_digest`].
    pub signature: [[u8; 32]; 2],
}
impl SignedLiveGrant {
    /// Structural bounds and lifetime shape, not signature or roster.
    ///
    /// # Errors
    ///
    /// Returns an error if the value fails validation or verification.
    pub fn validate(&self) -> Result<()> {
        let m = &self.metadata;
        m.request.validate()?;
        nonzero(m.root)?;
        nonzero(m.host)?;
        nonzero(m.host_boot)?;
        if self.version != 1
            || m.namespace.publisher != m.request.device
            || m.namespace.room != room_digest(&m.request.room)
            || m.audience.len() > 512
            || !m.audience.ends_with(LIVE_WEBTRANSPORT_PATH)
            || m.expires_at_ns
                .checked_sub(m.issued_at_ns)
                .as_ref()
                .is_none_or(|n| !(*n > 0 && *n <= GRANT_LIFETIME_NS))
        {
            return Err(LiveWireError::Invalid);
        }
        Ok(())
    }
    /// Domain-separated digest the host signs.
    ///
    /// # Errors
    ///
    /// Returns an error if the value cannot be canonically encoded.
    pub fn signing_digest(&self) -> Result<[u8; 32]> {
        self.validate()?;
        signing_digest(LIVE_GRANT_DOMAIN, &(self.version, &self.metadata))
            .map_err(|_| LiveWireError::Codec)
    }
    /// Host signs structurally valid claims.
    ///
    /// # Errors
    ///
    /// Returns an error if the value cannot be canonically encoded.
    pub fn sign(metadata: LiveGrantMetadata, host: &ed25519_dalek::SigningKey) -> Result<Self> {
        use ed25519_dalek::Signer;
        let mut grant = Self {
            version: 1,
            metadata,
            signature: [[0; 32]; 2],
        };
        let signature = host.sign(&grant.signing_digest()?).to_bytes();
        let (low, high) = signature.split_at(32);
        grant.signature = [
            low.try_into().map_err(|_| LiveWireError::Codec)?,
            high.try_into().map_err(|_| LiveWireError::Codec)?,
        ];
        Ok(grant)
    }
    /// Verifies the host signature and that `now_ns` lies within the grant
    /// lifetime. Roster, membership generation and audience remain the relay's checks.
    ///
    /// # Errors
    ///
    /// Returns an error if the value fails validation or verification.
    pub fn verify(&self, host: &ed25519_dalek::VerifyingKey, now_ns: u64) -> Result<()> {
        if host.to_bytes() != self.metadata.host
            || now_ns < self.metadata.issued_at_ns
            || now_ns >= self.metadata.expires_at_ns
        {
            return Err(LiveWireError::Invalid);
        }
        let mut bytes = [0u8; 64];
        bytes[..32].copy_from_slice(&self.signature[0]);
        bytes[32..].copy_from_slice(&self.signature[1]);
        host.verify_strict(
            &self.signing_digest()?,
            &ed25519_dalek::Signature::from_bytes(&bytes),
        )
        .map_err(|_| LiveWireError::Invalid)
    }
}

/// Opaque record reference shared by all members (application, cluster, record).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveRecord {
    /// Application identifier.
    #[serde(deserialize_with = "text128")]
    pub app: String,
    /// Record identifier within the application.
    #[serde(deserialize_with = "text128")]
    pub record: String,
}
impl LiveRecord {
    /// Stable digest naming this record's ops track.
    pub fn digest(&self) -> [u8; 32] {
        let mut hash = blake3::Hasher::new();
        hash.update(b"tilo-live-record-v1\0");
        hash.update(&(self.app.len() as u64).to_le_bytes());
        hash.update(self.app.as_bytes());
        hash.update(self.record.as_bytes());
        *hash.finalize().as_bytes()
    }
    fn validate(&self) -> Result<()> {
        if self.app.is_empty()
            || self.app.len() > 128
            || self.record.is_empty()
            || self.record.len() > 128
        {
            Err(LiveWireError::Invalid)
        } else {
            Ok(())
        }
    }
}

/// Opaque object inside a record or on the desktop (window, shape, block).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveObject {
    /// Owning record, or `None` for desktop-level objects such as windows.
    pub record: Option<LiveRecord>,
    /// Application-defined identifier, 1..=[`MAX_OBJECT_ID_BYTES`] bytes.
    #[serde(with = "object_bytes")]
    pub id: Vec<u8>,
}

/// Scoped text anchor: `[1, uvarint(offset), postcard(scope key)]`.
pub const ANCHOR_TAG_SCOPED: u8 = 1;
/// Same-scope head: `[2, uvarint(offset)]` (reuses the anchor's scope).
pub const ANCHOR_TAG_SAME_SCOPE: u8 = 2;
/// Digest-scoped anchor: `[3, uvarint(offset), 16-byte scope digest]`, sent
/// only when the scoped form exceeds [`MAX_OBJECT_ID_BYTES`] (e.g. 130-byte
/// Writer block ids). The digest is `PresenceKey::scope_digest` (ui-core);
/// receivers resolve it against their own scopes. Additive: the anchor stays
/// opaque bytes on the wire, so no frame encoding changes.
pub const ANCHOR_TAG_DIGEST: u8 = 3;
/// Byte length of an [`ANCHOR_TAG_DIGEST`] scope digest.
pub const ANCHOR_SCOPE_DIGEST_BYTES: usize = 16;

/// Text position as a stable CRDT anchor (opaque, application-encoded) plus offset.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveAnchor {
    /// Opaque CRDT cursor bytes (e.g. an automerge cursor), 1..=64 bytes.
    #[serde(with = "object_bytes")]
    pub cursor: Vec<u8>,
}

/// Drag lifecycle; `End` is followed by the durable change on the durable path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DragPhase {
    /// Pointer captured.
    Start,
    /// Intermediate position.
    Move,
    /// Released; receivers keep the preview until the durable change or a timeout.
    End,
    /// Aborted; receivers restore the durable position.
    Cancel,
}

/// A live drag: the object, its phase, the pointer position and an optional anchor.
pub type LiveDrag = (LiveObject, DragPhase, [f32; 2], Option<[f32; 2]>);
/// Latest-value presence and interaction state (datagram track).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresenceFrame {
    /// Monotonic per-publisher sequence; receivers drop older frames.
    pub seq: u64,
    /// Publisher wall clock, for interpolation/diagnostics only (never ordering).
    pub sent_at_ms: u64,
    /// Record the publisher is focused on, if any.
    pub focus: Option<LiveRecord>,
    /// Pointer position in the focused surface's coordinate space.
    pub pointer: Option<[f32; 2]>,
    /// Text selection as (anchor, head).
    pub selection: Option<(LiveAnchor, LiveAnchor)>,
    /// Object being dragged/resized: object, phase, x, y, optional width/height.
    pub drag: Option<LiveDrag>,
}
impl PresenceFrame {
    fn validate(&self) -> Result<()> {
        if let Some(record) = &self.focus {
            record.validate()?;
        }
        if let Some(pointer) = &self.pointer {
            finite(pointer)?;
        }
        if let Some((anchor, head)) = &self.selection {
            object_id(&anchor.cursor)?;
            object_id(&head.cursor)?;
        }
        if let Some((object, _, position, size)) = &self.drag {
            object_id(&object.id)?;
            if let Some(record) = &object.record {
                record.validate()?;
            }
            finite(position)?;
            if let Some(size) = size {
                finite(size)?;
                if size.iter().any(|v| *v < 0.0) {
                    return Err(LiveWireError::Invalid);
                }
            }
        }
        Ok(())
    }
}

/// Ephemeral text preview operation, superseded by the durable change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PreviewOp {
    /// Insert text at an anchor.
    Insert {
        /// Where the text goes.
        at: LiveAnchor,
        /// The inserted text.
        #[serde(deserialize_with = "preview_text")]
        text: String,
    },
    /// Delete `len` characters starting at an anchor.
    Delete {
        /// Where the deletion starts.
        at: LiveAnchor,
        /// How many characters are deleted.
        len: u32,
    },
}

/// Ordered per-record stream frame.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpsFrame {
    /// Ephemeral preview of local edits not yet durably admitted.
    Preview {
        /// Monotonic per-publisher, per-record sequence.
        seq: u64,
        /// Record the preview applies to.
        record: LiveRecord,
        /// 1..=[`MAX_PREVIEW_OPS`] operations.
        ops: Vec<PreviewOp>,
    },
    /// Early forwarding of a durable, signed change (the exact canonical
    /// delivery item bytes). Receivers verify and admit it exactly like replay,
    /// de-duplicate by digest, and never advance their durable cursor from it.
    SignedChange {
        /// Record the change applies to.
        record: LiveRecord,
        /// blake3 of `canonical`.
        digest: [u8; 32],
        /// Original canonical delivery item bytes.
        #[serde(with = "serde_bytes")]
        canonical: Vec<u8>,
    },
}
impl OpsFrame {
    fn validate(&self) -> Result<()> {
        match self {
            Self::Preview { record, ops, .. } => {
                record.validate()?;
                if ops.is_empty() || ops.len() > MAX_PREVIEW_OPS {
                    return Err(LiveWireError::Invalid);
                }
                for op in ops {
                    match op {
                        PreviewOp::Insert { at, text } => {
                            object_id(&at.cursor)?;
                            if text.is_empty() || text.len() > MAX_PREVIEW_TEXT_BYTES {
                                return Err(LiveWireError::Invalid);
                            }
                        }
                        PreviewOp::Delete { at, len } => {
                            object_id(&at.cursor)?;
                            if *len == 0 {
                                return Err(LiveWireError::Invalid);
                            }
                        }
                    }
                }
                Ok(())
            }
            Self::SignedChange {
                record,
                digest,
                canonical,
            } => {
                record.validate()?;
                if canonical.is_empty() || *blake3::hash(canonical).as_bytes() != *digest {
                    return Err(LiveWireError::Invalid);
                }
                Ok(())
            }
        }
    }
}

macro_rules! codec {
    ($ty:ty, $cap:expr) => {
        impl $ty {
            /// Canonical encoding after checking all structural bounds.
            ///
            /// # Errors
            ///
            /// Returns an error if the value violates its wire bounds or cannot be serialized.
            pub fn encode(&self) -> Result<Vec<u8>> {
                self.validate()?;
                let mut out = vec![0u8; $cap];
                let used = postcard::to_slice(self, &mut out)
                    .map_err(|_| LiveWireError::Codec)?
                    .len();
                out.truncate(used);
                Ok(out)
            }
            /// Reject outer size before parsing and noncanonical encodings.
            ///
            /// # Errors
            ///
            /// Returns an error if the input is not a canonical encoding within this type's bounds.
            pub fn decode(bytes: &[u8]) -> Result<Self> {
                if bytes.is_empty() || bytes.len() > $cap {
                    return Err(LiveWireError::Codec);
                }
                let value: Self = postcard::from_bytes(bytes).map_err(|_| LiveWireError::Codec)?;
                if value.encode()? != bytes {
                    return Err(LiveWireError::Codec);
                }
                Ok(value)
            }
        }
    };
}
codec!(LiveGrantRequest, MAX_GRANT_BYTES);
codec!(SignedLiveGrant, MAX_GRANT_BYTES);
codec!(PresenceFrame, MAX_DATAGRAM_FRAME_BYTES);
codec!(OpsFrame, MAX_STREAM_FRAME_BYTES);

fn bounded_text<'de, D: Deserializer<'de>, const N: usize>(
    d: D,
) -> std::result::Result<String, D::Error> {
    let value = String::deserialize(d)?;
    if value.is_empty() || value.len() > N {
        Err(de::Error::custom("text exceeds bounds"))
    } else {
        Ok(value)
    }
}
fn text128<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<String, D::Error> {
    bounded_text::<D, 128>(d)
}
fn text512<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<String, D::Error> {
    bounded_text::<D, 512>(d)
}
fn preview_text<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<String, D::Error> {
    bounded_text::<D, MAX_PREVIEW_TEXT_BYTES>(d)
}
mod object_bytes {
    use super::{Deserialize, Deserializer, MAX_OBJECT_ID_BYTES, Serialize, de};
    pub fn serialize<S: serde::Serializer>(v: &[u8], s: S) -> std::result::Result<S::Ok, S::Error> {
        serde_bytes::Bytes::new(v).serialize(s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Vec<u8>, D::Error> {
        let value: serde_bytes::ByteBuf = serde_bytes::ByteBuf::deserialize(d)?;
        if value.is_empty() || value.len() > MAX_OBJECT_ID_BYTES {
            return Err(de::Error::custom("object identifier exceeds bounds"));
        }
        Ok(value.into_vec())
    }
}

// ---- Agent live identity and activity ------------------------------------

/// Domain of [`agent_publisher`].
pub const AGENT_PUBLISHER_DOMAIN: &[u8] = b"tilo-live-agent-v1\0";
/// Application of the agent activity live record.
pub const AGENT_ACTIVITY_APP: &str = "so.tilo.agent";
/// Record of the agent activity live record (never a durable record id).
pub const AGENT_ACTIVITY_RECORD: &str = "@tilo:agent-activity";
/// Fixed preview anchor of an activity insert; activity is state, not text.
pub const AGENT_ACTIVITY_ANCHOR: [u8; 1] = [0xA1];
/// Closed set of agent activity states.
pub const AGENT_ACTIVITY_STATES: [&str; 8] = [
    "thinking", "tool", "writing", "replying", "applying", "done", "failed", "idle",
];
/// Agent identifier bound (bytes).
pub const MAX_AGENT_ID_BYTES: usize = 128;
/// Agent display name bound (bytes).
pub const MAX_AGENT_NAME_BYTES: usize = 64;
/// Activity label bound (bytes).
pub const MAX_AGENT_LABEL_BYTES: usize = 120;
/// Activity tool name bound (bytes).
pub const MAX_AGENT_TOOL_BYTES: usize = 64;

/// Live publisher (namespace) id of an agent the host speaks for:
/// `blake3(AGENT_PUBLISHER_DOMAIN ‖ u64le(len room) ‖ room ‖ host ‖ agent_id)`.
/// `room` is the native room identifier (the same text given to
/// [`room_digest`] / [`LiveNamespace::new`]), `host` the room authority (Store)
/// device key. The output is a digest, never an enrolled device key, so no
/// grant can name it: only the host injects into it, and receivers accept
/// agent attribution only for a publisher equal to this recomputed value.
pub fn agent_publisher(room: &str, host: [u8; 32], agent_id: &str) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(AGENT_PUBLISHER_DOMAIN);
    hash.update(&(room.len() as u64).to_le_bytes());
    hash.update(room.as_bytes());
    hash.update(&host);
    hash.update(agent_id.as_bytes());
    *hash.finalize().as_bytes()
}

/// The agent's live namespace in `room`.
pub fn agent_namespace(room: &str, host: [u8; 32], agent_id: &str) -> LiveNamespace {
    LiveNamespace::new(room, agent_publisher(room, host, agent_id))
}

/// The shared agent activity live record.
pub fn agent_activity_record() -> LiveRecord {
    LiveRecord {
        app: AGENT_ACTIVITY_APP.into(),
        record: AGENT_ACTIVITY_RECORD.into(),
    }
}

/// One agent activity value, carried as the JSON text of a single preview
/// insert at [`AGENT_ACTIVITY_ANCHOR`] on [`agent_activity_record`] in the
/// agent's namespace. Inert display state: never authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentActivity {
    /// Exactly one.
    pub v: u8,
    /// Agent identifier, 1..=[`MAX_AGENT_ID_BYTES`].
    pub agent: String,
    /// Display name from the owner's spawn request, 1..=[`MAX_AGENT_NAME_BYTES`].
    pub name: String,
    /// One of [`AGENT_ACTIVITY_STATES`].
    pub state: String,
    /// Human activity line, 0..=[`MAX_AGENT_LABEL_BYTES`].
    pub label: String,
    /// Tool in use, 1..=[`MAX_AGENT_TOOL_BYTES`].
    #[serde(default)]
    pub tool: Option<String>,
    /// Host wall clock of this value (display only, never ordering).
    pub at_ms: u64,
    /// Member-visible agent status, one of [`AGENT_STATUSES`], computed by
    /// the host with the same rule as the owner's agent reads, so every
    /// member shows one status. Absent from older hosts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

/// Closed set of [`AgentActivity::status`] (the agent-reads `AgentStatus`).
pub const AGENT_STATUSES: [&str; 6] = [
    "starting",
    "running",
    "waiting",
    "paused",
    "completed",
    "failed",
];

fn clean(text: &str, min: usize, max: usize) -> Result<()> {
    if text.len() < min || text.len() > max || text.chars().any(char::is_control) {
        Err(LiveWireError::Invalid)
    } else {
        Ok(())
    }
}

/// Longest prefix of `text` without control characters within `max` bytes, on
/// a character boundary.
pub fn bounded_display_text(text: &str, max: usize) -> String {
    let mut out = String::new();
    for ch in text
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
    {
        if out.len().saturating_add(ch.len_utf8()) > max {
            break;
        }
        out.push(ch);
    }
    out
}

impl AgentActivity {
    /// Structural bounds of every field and of the encoded JSON.
    ///
    /// # Errors
    ///
    /// Returns an error if the value fails validation or verification.
    pub fn validate(&self) -> Result<()> {
        if self.v != 1 || !AGENT_ACTIVITY_STATES.contains(&self.state.as_str()) {
            return Err(LiveWireError::Invalid);
        }
        clean(&self.agent, 1, MAX_AGENT_ID_BYTES)?;
        clean(&self.name, 1, MAX_AGENT_NAME_BYTES)?;
        clean(&self.label, 0, MAX_AGENT_LABEL_BYTES)?;
        if let Some(tool) = &self.tool {
            clean(tool, 1, MAX_AGENT_TOOL_BYTES)?;
        }
        if let Some(status) = &self.status
            && !AGENT_STATUSES.contains(&status.as_str())
        {
            return Err(LiveWireError::Invalid);
        }
        Ok(())
    }
    /// Compact JSON within [`MAX_PREVIEW_TEXT_BYTES`].
    ///
    /// # Errors
    ///
    /// Returns an error if the value violates its wire bounds or cannot be serialized.
    pub fn to_json(&self) -> Result<String> {
        self.validate()?;
        let json = serde_json::to_string(self).map_err(|_| LiveWireError::Codec)?;
        if json.len() > MAX_PREVIEW_TEXT_BYTES {
            return Err(LiveWireError::Invalid);
        }
        Ok(json)
    }
    /// Strict bounded parse (unknown fields rejected).
    ///
    /// # Errors
    ///
    /// Returns an error if the input is not a canonical encoding within this type's bounds.
    pub fn from_json(text: &str) -> Result<Self> {
        if text.is_empty() || text.len() > MAX_PREVIEW_TEXT_BYTES {
            return Err(LiveWireError::Codec);
        }
        let value: Self = serde_json::from_str(text).map_err(|_| LiveWireError::Codec)?;
        value.validate()?;
        Ok(value)
    }
    /// The activity preview frame (one insert at the fixed anchor).
    ///
    /// # Errors
    ///
    /// Returns an error if the value violates its wire bounds or cannot be serialized.
    pub fn to_frame(&self, seq: u64) -> Result<OpsFrame> {
        Ok(OpsFrame::Preview {
            seq,
            record: agent_activity_record(),
            ops: vec![PreviewOp::Insert {
                at: LiveAnchor {
                    cursor: AGENT_ACTIVITY_ANCHOR.to_vec(),
                },
                text: self.to_json()?,
            }],
        })
    }
    /// Parse an activity preview frame: exactly one insert at the fixed anchor
    /// on the activity record. Returns `(seq, activity)`.
    ///
    /// # Errors
    ///
    /// Returns an error if the input is not a canonical encoding within this type's bounds.
    pub fn from_frame(frame: &OpsFrame) -> Result<(u64, Self)> {
        let OpsFrame::Preview { seq, record, ops } = frame else {
            return Err(LiveWireError::Invalid);
        };
        match ops.as_slice() {
            [PreviewOp::Insert { at, text }]
                if *record == agent_activity_record() && at.cursor == AGENT_ACTIVITY_ANCHOR =>
            {
                Ok((*seq, Self::from_json(text)?))
            }
            _ => Err(LiveWireError::Invalid),
        }
    }
}

pub mod legacy_window;

#[cfg(test)]
mod tests;
