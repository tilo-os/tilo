//! Local mirror of the legacy `ui::collaboration` wire types
//! (`CollabRequest` / `CollabEvent` / `PresencePayload` / `PresenceKey` /
//! `Collaborator`).
//!
//! We intentionally do **not** depend on the `ui` crate — that would drag
//! in wgpu/vello/cosmic-text and balloon the bridge daemon into a
//! windowing dep tree. These enums are redefined here with the same
//! serde-JSON layout so the wire round-trips identically; `tests/roundtrip.rs`
//! guards that. `PresenceKey` is repeated verbatim so `keyBytes` postcard
//! round-trips end-to-end through the shell broker.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use collab_types::{ArtifactId, SignedOp};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum CollabRequest {
    Hello {
        app_id: String,
        cluster_id: Uuid,
        /// Which record this connection carries, mirroring
        /// `ui::collaboration::CollabRequest::Hello`; must exist here even though
        /// `hello_from_join` always sends `None`, to keep the mirrored JSON shape
        /// identical. `#[serde(default)]` so an older peer's frame parses as
        /// `None` instead of being rejected.
        #[serde(default)]
        record: Option<String>,
        /// Presence handle (see `ui::collaboration::CollabRequest::Hello`).
        #[serde(default)]
        handle: Option<String>,
    },
    SyncCRDT {
        data: Vec<u8>,
    },
    SyncIncremental {
        data: Vec<u8>,
    },
    Presence {
        key: PresenceKey,
        payload: PresencePayload,
    },
    SignedSyncIncremental {
        envelope: Box<SignedOp>,
    },
    FetchScrollback {
        artifact: ArtifactId,
        before_seq: u64,
        max: u16,
    },
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum CollabEvent {
    Context {
        collaborators: Vec<Collaborator>,
        permissions: Vec<String>,
    },
    RemoteCursorMoved {
        user_id: String,
        x: f32,
        y: f32,
    },
    CRDTUpdate {
        data: Vec<u8>,
    },
    CRDTIncremental {
        data: Vec<u8>,
    },
    RemotePresenceUpdate {
        user_id: String,
        key: PresenceKey,
        payload: PresencePayload,
    },
    SignedCRDTIncremental {
        envelope: Box<SignedOp>,
        from_user_id: String,
    },
    ScrollbackBatch {
        artifact: ArtifactId,
        ops: Vec<Box<SignedOp>>,
        exhausted: bool,
    },
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub enum PresencePayload {
    Pointer { x: f32, y: f32 },
    Caret { pos: usize },
    Selection { start: usize, end: usize },
    Arrived,
    Left,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Collaborator {
    pub id: String,
    pub name: String,
    pub color: [f32; 3],
    /// Whether this member is an agent rather than a person.
    ///
    /// Defaulted on the wire so an older peer that omits the field reads as a
    /// person — a peer wrongly shown as human is a smaller lie than a roster
    /// that fails to load.
    #[serde(default)]
    pub is_agent: bool,
    /// Whether this row is the receiving machine's own participant.
    ///
    /// Not derivable from `id` — a machine never gets its own participant
    /// announced to itself, so `id` alone can't identify "the local one".
    /// Defaulted on the wire so a peer predating this field reads as somebody else.
    #[serde(default)]
    pub is_self: bool,
}

// PresenceKey mirror: SDK-side `keyBytes` is postcard-encoded
// `ui::PresenceKey`; the bridge decodes into this local mirror to ship a
// `Presence` request upstream. Shape must match
// `ui::presence::PresenceKey` byte-for-byte under postcard.

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum PathSegment {
    Field(String),
    Index(u32),
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum PresenceKey {
    Widget {
        path: Vec<PathSegment>,
    },
    Tab {
        id: String,
    },
    Workspace {
        id: String,
    },
    App {
        app: AppKind,
        sub: String,
    },
    /// A participant's broadcast view subset, as its postcard encoding — opaque here, the bridge only routes it.
    View {
        data: Vec<u8>,
    },
    /// A pointer position together with the space it was measured in.
    Pointer {
        space: PointerSpace,
    },
}

/// Postcard mirror of `ui::presence::PointerSpace`; variant order must match upstream byte-for-byte (same rule as [`AppKind`]).
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum PointerSpace {
    Desktop { room: String },
    Surface { surface: String },
}

/// Postcard mirror of `ui::permissions::AppKind`. Postcard tags variants
/// by ordinal, so this order MUST match `ui::permissions::AppKind`
/// byte-for-byte — a new upstream variant needs one here at the same ordinal.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum AppKind {
    Terminal,
    FileExplorer,
    Browser,
    Settings,
    Unknown,
}
