//! Local mirror of the legacy `ui::collaboration` wire types
//! (`CollabRequest` / `CollabEvent` / `PresencePayload` /
//! `PresenceKey` / `Collaborator`).
//!
//! We intentionally do **not** depend on the `ui` crate, which drags
//! in wgpu / vello / cosmic-text and would balloon the bridge daemon
//! into a windowing dep tree. Instead we redefine the enums here with
//! the same serde-JSON-compatible layout so the wire round-trips
//! identically. If `ui::collaboration` ever changes shape, this
//! module needs to move in lockstep — the round-trip test in
//! `tests/roundtrip.rs` guards the JSON representation.
//!
//! The `PresenceKey` enum is repeated verbatim so `keyBytes` postcard
//! round-trips end-to-end (SDK-side `postcard::to_stdvec(&PresenceKey)`
//! → wire → shell broker → wire → SDK-side `postcard::from_bytes()`).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use collab_types::{ArtifactId, SignedOp};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum CollabRequest {
    Hello {
        app_id: String,
        cluster_id: Uuid,
        /// Which record this connection carries, mirroring
        /// `ui::collaboration::CollabRequest::Hello`. A bridged SDK
        /// client has no record of its own, so `hello_from_join` sends
        /// `None` — but the field has to exist here or the mirror is no
        /// longer a mirror and the JSON the broker parses differs from
        /// the JSON this crate emits.
        ///
        /// `#[serde(default)]` for the same reason as upstream: a frame
        /// from a peer predating records parses as `None` instead of
        /// being dropped as malformed.
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
    /// The room already knows -- an agent is admitted as one, and
    /// `RosterRow::is_agent` records it. This projection dropped it,
    /// so anything downstream that needed to tell an agent from a
    /// person had to read the id and hope, which is the inference the
    /// marking exists to make unnecessary.
    ///
    /// Defaulted on the wire so an older peer that does not send the
    /// field is read as "a person" rather than failing to decode.
    /// A peer wrongly shown as a person is a smaller lie than a
    /// roster that will not load.
    #[serde(default)]
    pub is_agent: bool,
}

// ---- PresenceKey mirror -------------------------------------------------
//
// The SDK-side `keyBytes` is a postcard-encoded `ui::PresenceKey`. The
// bridge decodes it into this local mirror so a `Presence` request can
// be shipped upstream as a full JSON `CollabRequest::Presence`. The
// shape must match `ui::presence::PresenceKey` byte-for-byte under
// postcard.

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum PathSegment {
    Field(String),
    Index(u32),
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum PresenceKey {
    Widget { path: Vec<PathSegment> },
    Tab { id: String },
    Workspace { id: String },
    App { app: AppKind, sub: String },
}

/// Postcard mirror of `ui::permissions::AppKind`. Postcard tags enum
/// variants by ordinal, so the variant order below MUST match
/// `ui::permissions::AppKind` byte-for-byte. If upstream grows a
/// variant, the bridge must grow one at the same ordinal.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum AppKind {
    Terminal,
    FileExplorer,
    Browser,
    Settings,
    Unknown,
}
