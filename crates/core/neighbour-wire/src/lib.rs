//! Peer-sync wire protocol: a single tagged enum ([`NeighbourMsg`]) carrying
//! every cross-host payload of the state link (CRDT sync, governance,
//! actions, content). What streams between devices (a call, a shared window's
//! pictures, its viewers' input) travels the workspace's live session
//! (`tilo_live`), never this link. Transport bindings (a direct device
//! connection, a durable host, an in-process hub) operate on it without inspecting the payload.
//!
//! The encoding is postcard over serde's default externally tagged enum
//! representation, so the variant order is the wire discriminant: new
//! variants are appended, never inserted, and new fields go last with
//! `#[serde(default)]`. The golden-byte tests in this crate pin every
//! variant so a peer running an older build keeps decoding what this one
//! sends.
//!
//! The crate is pure data plus pure helpers ([`mod@retention`]); it owns no
//! transport, runtime or state machine.

/// One action sent to another device of the workspace, signed by its sender.
pub mod actions;
pub mod colorimetry;
/// Content a record references, asked of a workspace by hash and answered in
/// pieces: what every key holder of a workspace speaks, host or page.
pub mod content;
pub mod hdr_metadata;
mod key_scope;
pub mod placement;
pub mod retention;
pub use key_scope::KeyScope;
pub mod unit_chain;

pub use retention::{Retention, app_document_group, audience, retention, standalone};
pub use tilo_actions::document_sync::{
    DocumentSync, chain_head, document_chunk, document_part, document_sync, on_demand_document,
};

use serde::{Deserialize, Serialize};

/// The device ↔ device interface's version: the certified handshake
/// (`tilo_server_protocol::auth`), the server session's frames and the
/// peer-sync messages they carry. Two devices sync when theirs are at most
/// one apart ([`works_with_peer_version`]), so a person's servers updating
/// one at a time keep serving each other and their devices while the
/// rollout goes on; there is no negotiation and nothing kept for any older
/// version (docs/architecture/compatibility.md). `interface.toml` beside this
/// crate holds the schema, and the repository policy fails a change to it
/// that keeps the number.
///
/// [`NeighbourMsg::IdentityHello`] carries it first, at a variant index that
/// never changes ([`IDENTITY_HELLO_INDEX`]), so every version reads another
/// device's number before anything else it says.
pub const PEER_PROTOCOL_VERSION: u32 = 72;

/// The oldest peer version this build still works with: the one before
/// [`PEER_PROTOCOL_VERSION`] when this build reads and writes the frames of
/// its main flows (the handshake, sync, an identity announcement, a lease)
/// exactly as that version did, otherwise this one. The frames that version
/// wrote are kept beside this crate (`previous-peer-frames.txt`), and a test
/// fails a build whose file names another version.
pub const OLDEST_PEER_PROTOCOL_VERSION: u32 = 71;

// A version bump moves `OLDEST_PEER_PROTOCOL_VERSION` in the same change, and
// `previous-peer-frames.txt` with it: the window is one version wide, so a
// bump that leaves it behind fails here, in the bumping crate's own build,
// rather than later in a test of another crate (docs/architecture/compatibility.md,
// "Two versions at once").
const _: () = assert!(
    OLDEST_PEER_PROTOCOL_VERSION + 1 >= PEER_PROTOCOL_VERSION
        && OLDEST_PEER_PROTOCOL_VERSION <= PEER_PROTOCOL_VERSION,
    "a peer version bump also moves OLDEST_PEER_PROTOCOL_VERSION and previous-peer-frames.txt"
);

#[cfg(test)]
mod frozen_frames {
    /// The frozen frames are the oldest version's this build works with, so a
    /// bump that moves the window without writing them fails in this crate.
    #[test]
    fn the_frozen_frames_are_the_oldest_versions() {
        let version = include_str!("../previous-peer-frames.txt")
            .lines()
            .find_map(|line| line.strip_prefix("version "))
            .and_then(|version| version.trim().parse::<u32>().ok());
        assert_eq!(version, Some(super::OLDEST_PEER_PROTOCOL_VERSION));
    }
}

/// Whether a device at peer version `theirs` and this one work together:
/// the same version, the one before when this build still speaks it
/// ([`OLDEST_PEER_PROTOCOL_VERSION`]), or the one after, which speaks this
/// one's by the same rule. The window is exactly one version wide.
#[must_use]
pub const fn works_with_peer_version(theirs: u32) -> bool {
    theirs >= OLDEST_PEER_PROTOCOL_VERSION && theirs <= PEER_PROTOCOL_VERSION.saturating_add(1)
}

/// [`NeighbourMsg::IdentityHello`]'s postcard variant index: the one message
/// every version decodes far enough to learn the sender's
/// [`PEER_PROTOCOL_VERSION`].
pub const IDENTITY_HELLO_INDEX: u8 = 9;

/// The [`PEER_PROTOCOL_VERSION`] an encoded [`NeighbourMsg`] says it is
/// from, when it is an identity announcement: read from its frozen prefix,
/// whatever the rest of a message from another version looks like.
#[must_use]
pub fn identity_hello_version(bytes: &[u8]) -> Option<u32> {
    let (&index, rest) = bytes.split_first()?;
    if index != IDENTITY_HELLO_INDEX {
        return None;
    }
    let mut version: u32 = 0;
    // A postcard varint: up to five 7-bit groups, least significant first.
    for (shift, byte) in (0_u32..35).step_by(7).zip(rest) {
        version |= u32::from(byte & 0x7f).checked_shl(shift)?;
        if byte & 0x80 == 0 {
            return Some(version);
        }
    }
    None
}

/// Peer identifier — opaque numeric handle assigned per session.
pub type PeerId = u64;

/// The [`PeerId`] a device's messages carry as their publisher: FNV-1a over
/// its Ed25519 device key. A receiving shell drops a message whose publisher
/// is not the handle of the device it authenticated the message from, so
/// every sender (shell, browser, a host signing a compaction) derives it here.
pub fn peer_handle(device_public_key: &[u8; 32]) -> PeerId {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h = FNV_OFFSET;
    for b in device_public_key {
        h ^= u64::from(*b);
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// Wayland surface identifier — local handle used for streaming/input routing.
pub type SurfaceId = u64;

/// One window a viewer shows: the workspace tab holding it (the same id on
/// every device of the workspace) and the tile's size in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceView {
    pub tab: u64,
    pub width: u32,
    pub height: u32,
}

/// The largest picture a viewer's hardware decoder for `codec` (a codec
/// label, `"h264"`, `"vp9"` …) takes, where it has one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceDecodeLimit {
    pub codec: String,
    pub max: Option<(u32, u32)>,
}

/// Presence channel key — namespaces presence updates (e.g. cursor, selection).
pub type PresenceKey = String;

/// The colour the person at the publishing session wears for everyone in
/// the workspace: one of the twelve presence hues by its stored name (`"red"` …
/// `"pink"`, as `identity.colour` spells it) — their choice, or the one their
/// stable identity derives, so every member paints them the same. Self-
/// asserted and cosmetic, like `user`. `None` when the sender did not say; a
/// receiver then derives one from the sender's handle. A name the receiver
/// does not know is treated as unsaid.
pub type PresenceColour = Option<String>;

/// Why a device refused one of another device's writes: the classes the
/// person is shown, as the store and the workspace decide them.
///
/// Closed, so a refusing device of a later build never makes an older one
/// show a class it does not know as something it is not: the reason travels
/// as this enum and the device's own words travel beside it
/// ([`WriteRefusal::detail`]), shown only where they are the person's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum RefusalReason {
    /// The person revoked the writing device, so nothing it writes is kept.
    Revoked,
    /// The writing device's certificate has run out.
    Expired,
    /// The receiving device holds no certificate for the writer: it was
    /// never enrolled, or the enrolment did not reach this device.
    NotCertified,
    /// The writer may not change this: the workspace's governance and the
    /// writer's role withhold it.
    Forbidden,
    /// The write is not the shape the store admits.
    Malformed,
    /// A write it depends on has not arrived; the same write is admitted
    /// once it has.
    MissingDependency,
    /// Another write already holds its place.
    Conflict,
    /// The receiving device is over what it keeps of the writer's.
    Full,
    /// The receiving device said something else it has no class for.
    Other,
}

impl RefusalReason {
    /// Whether a write refused for this reason is refused for good: no later
    /// attempt of the same write is admitted, so its author is told once
    /// rather than left waiting.
    #[must_use]
    pub const fn permanent(self) -> bool {
        match self {
            Self::Revoked
            | Self::Expired
            | Self::NotCertified
            | Self::Forbidden
            | Self::Malformed => true,
            Self::MissingDependency | Self::Conflict | Self::Full | Self::Other => false,
        }
    }

    /// How a person reads this reason, as the write's author is told it.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Revoked => "the person revoked the device",
            Self::Expired => "the device's certificate expired",
            Self::NotCertified => "the device is not a member here",
            Self::Forbidden => "no permission",
            Self::Malformed => "the write is malformed",
            Self::MissingDependency => "it waits for earlier writes",
            Self::Conflict => "another write holds its place",
            Self::Full => "the device is full",
            Self::Other => "refused",
        }
    }
}

/// One device's verdict on one write of another's: a signed fact the
/// refusing device keeps in its own synced state (beside the revocations of a
/// personal store, in its own slice of a workspace document), so it reaches
/// the write's author live and on catch-up, survives restarts, and can be
/// shown with the history.
///
/// Signed by the refusing device's own key over
/// [`Self::signed_bytes`]; a receiver keeps it only when that signature
/// verifies against a certificate it already holds. Never a global verdict:
/// a refusal is one device's, and another may hold the write.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteRefusal {
    /// The store or workspace key the refused write was routed in.
    pub workspace: String,
    /// The app of the refused write's record, empty for a workspace change.
    pub app: String,
    /// The record, empty for a workspace change.
    pub record: String,
    /// The device that wrote the refused write.
    pub author: [u8; 32],
    /// The sequence number the refused write carried, `0` where it has none.
    pub seq: u64,
    /// The refused write's operation id, as the receiving device decided it.
    pub operation: String,
    /// The automerge changes the refused write carried, as their hashes: what
    /// the author's history joins it to. Empty when the write carried none.
    pub changes: Vec<[u8; 32]>,
    pub reason: RefusalReason,
    /// What the refusing device said in its own words, for the person's own
    /// devices; never shown for another person's.
    pub detail: String,
    /// The refusing device's key, whose signature this fact carries.
    pub by: [u8; 32],
    /// When it refused, in nanoseconds since the Unix epoch.
    pub at_ns: u64,
    /// The refusing device's signature over [`Self::signed_bytes`].
    pub signature: Vec<u8>,
}

impl WriteRefusal {
    /// The bytes `by`'s signature covers: the fact without its signature.
    ///
    /// # Errors
    /// The fact does not encode.
    pub fn signed_bytes(&self) -> Result<Vec<u8>, String> {
        let mut unsigned = self.clone();
        unsigned.signature = Vec::new();
        postcard::to_allocvec(&unsigned).map_err(|error| error.to_string())
    }

    /// Sign this fact with the refusing device's `signer`, which answers at
    /// once ([`collab_types::signer::sign_now`]), as the device that made the
    /// verdict does.
    ///
    /// # Errors
    /// The fact does not encode, or the signer did not sign.
    pub fn sign(&mut self, signer: &dyn collab_types::signer::DeviceSigner) -> Result<(), String> {
        self.by = signer.device_public_key();
        let signature = collab_types::signer::sign_now(signer, self.signed_bytes()?)
            .map_err(|error| error.to_string())?;
        self.signature = signature.to_vec();
        Ok(())
    }

    /// Whether this fact carries `device`'s signature over exactly its
    /// contents.
    #[must_use]
    pub fn signed_by(&self, device: &[u8; 32]) -> bool {
        use ed25519_dalek::Verifier as _;
        if &self.by != device {
            return false;
        }
        let Ok(key) = ed25519_dalek::VerifyingKey::from_bytes(device) else {
            return false;
        };
        let Ok(bytes): Result<[u8; 64], _> = self.signature.as_slice().try_into() else {
            return false;
        };
        let signature = ed25519_dalek::Signature::from_bytes(&bytes);
        let Ok(signed) = self.signed_bytes() else {
            return false;
        };
        key.verify(&signed, &signature).is_ok()
    }
}

/// Tagged envelope for everything that crosses the neighbour boundary.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, Clone)]
pub enum NeighbourMsg {
    /// Request: replay every Automerge change to the workspace named by `workspace`
    /// after the included clock.
    ///
    /// `workspace` is the workspace id as text that scopes the
    /// answer — an empty/unparseable value names no workspace (not a wildcard) and gets
    /// no document back. The transport emits one on peer-join, before it knows
    /// what workspace the peer is in. A device on a direct link sends one when
    /// a frame the link brought leaves a change it cannot apply, and the other
    /// end answers on that link with the state it holds.
    SyncReq {
        since: Vec<u8>,
        workspace: String,
        /// Hash of the exact signed bearer invite being redeemed. Ordinary
        /// anti-entropy requests omit it; an authority uses it once to bind
        /// the authenticated transport identity to that invite's role.
        #[serde(default)]
        invite_id: Option<[u8; 32]>,
        /// Fresh per request. Transports that deduplicate by content would
        /// otherwise discard a repeated anti-entropy request for unchanged
        /// heads as already seen, and a workspace that missed its first
        /// answer would never catch up.
        #[serde(default)]
        nonce: u64,
    },
    /// Response: a batch of opaque Automerge change blobs for the workspace named by
    /// `workspace`. The blobs carry the workspace identity too; the receiver checks the two
    /// agree before merging.
    SyncResp {
        changes: Vec<Vec<u8>>,
        workspace: String,
    },
    /// Retired: a shared window travels the workspace's live session
    /// (`tilo_live::share`). No build produces it and receivers refuse it
    /// ([`NeighbourMsg::is_retired`]); it stays only so every later Postcard
    /// discriminant keeps its value.
    ///
    /// One encoded access unit (H.264/HEVC), frame (VP9) or temporal unit
    /// (AV1) for a shared surface, in the codec the preceding
    /// [`Self::SurfaceStreamStart`] named. `encoded_h264` keeps its historical
    /// name; the field carries whichever codec was announced.
    ///
    /// `ts` is the owner's monotonic-clock timestamp in nanoseconds; receivers
    /// diff against their own monotonic-now for motion-to-photon latency, since
    /// wall-clock (`SystemTime`) would drift under NTP on the WAN case.
    SurfaceFrame {
        surface: SurfaceId,
        /// Hashed Iroh endpoint of the stream owner. Bound to the
        /// authenticated publisher before encoded bytes are dispatched.
        #[serde(default)]
        source_peer: PeerId,
        encoded_h264: Vec<u8>,
        ts: u64,
        /// The unit's place in the owner's encoded sequence, one more than
        /// the unit it follows. Delta units reference earlier ones, so a
        /// receiver that sees a gap (a unit lost on the way) must not feed
        /// the next delta to its decoder: it waits for the next keyframe.
        seq: u64,
    },
    /// Retired: a shared window travels the workspace's live session
    /// (`tilo_live::share`). No build produces it and receivers refuse it
    /// ([`NeighbourMsg::is_retired`]); it stays only so every later Postcard
    /// discriminant keeps its value.
    ///
    /// Explicit start-of-stream announcement for a shared surface.
    ///
    /// Distinct from `SurfaceFrame` so late joiners can size a decoder /
    /// receive-side view before the first NALU arrives,
    /// avoiding a dropped frame. The codec is chosen by the sender from the
    /// receivers' [`Self::MediaCapabilities`]: `"av1"` or `"vp9"` when every
    /// receiver decodes it in hardware, `"h264"` otherwise (and always for a
    /// receiver that never advertised capabilities), `"hevc-main10"` for HDR.
    SurfaceStreamStart {
        surface: SurfaceId,
        /// Hashed Iroh `PeerId` of the owner; distinguishes concurrent shares in the
        /// same workspace.
        source_peer: PeerId,
        width: u32,
        height: u32,
        /// Codec label: `"h264"`, `"hevc-main10"`, `"vp9"` or `"av1"`.
        /// A receiver ignores (and logs) a label it does not know, which is
        /// why a sender only picks a codec every receiver advertised.
        codec: String,
        ts: u64,
        /// Sender's monotonic-clock reading when the message left the encode task
        /// (same origin as [`Self::SurfaceFrame::ts`]).
        ///
        /// The receiver computes `offset = receiver_now_ns - sender_monotonic_ns_at_send`
        /// on decode and applies it to every later `ts` via
        /// the receiver's latency metrics to get
        /// real cross-host motion-to-photon numbers despite unrelated monotonic
        /// origins.
        ///
        /// A value of `0` means the sender cannot provide a clock offset;
        /// the receiver then skips
        /// cross-host latency instead of reporting a bogus value.
        sender_monotonic_ns_at_send: u64,
        /// Colorimetry tag; `None` means the receiver assumes SDR BT.709.
        colorimetry: Option<crate::colorimetry::Colorimetry>,
        /// HDR10 static metadata (SMPTE ST 2086 + CTA-861.3) for HDR pipelines;
        /// `None` on SDR streams. Used by the receiver's tone-mapper to size
        /// compression correctly on SDR displays.
        hdr_static_metadata: Option<crate::hdr_metadata::Hdr10Metadata>,
        /// Codec target bit depth; `0` means unspecified — receivers infer 8-bit
        /// from `codec = "h264"`, else 8 or 10 (HEVC Main10).
        bit_depth: u8,
        /// Desktop app-id of the shared legacy window. Kept last so the
        /// serde default accepts pre-field postcard payloads.
        #[serde(default)]
        app_id: String,
        /// The workspace tab holding the window, the same id on every device
        /// of the workspace, so a viewer shows the stream in exactly that tab
        /// even when several windows of one app are open. `0` when the
        /// sender does not name one.
        #[serde(default)]
        tab: u64,
    },
    /// Retired: a shared window travels the workspace's live session
    /// (`tilo_live::share`). No build produces it and receivers refuse it
    /// ([`NeighbourMsg::is_retired`]); it stays only so every later Postcard
    /// discriminant keeps its value.
    ///
    /// Explicit end-of-stream (owner tore down the share, or the surface
    /// unmapped). Receivers drop their `PeerView` + decoder on receipt, instead of
    /// waiting out the 15s heartbeat-eviction path that implicit end-of-stream
    /// (owner vanishes from the workspace) still relies on.
    SurfaceStreamEnd {
        surface: SurfaceId,
        source_peer: PeerId,
        ts: u64,
    },
    /// Retired: a shared window travels the workspace's live session
    /// (`tilo_live::share`). No build produces it and receivers refuse it
    /// ([`NeighbourMsg::is_retired`]); it stays only so every later Postcard
    /// discriminant keeps its value.
    ///
    /// Serialized input event from `peer` targeting `surface`.
    RemoteInput {
        peer: PeerId,
        surface: SurfaceId,
        event: Vec<u8>,
    },
    /// Retired: `peer` would like permission `capability`. No build produces
    /// it and receivers refuse it ([`NeighbourMsg::is_retired`]); it stays
    /// only so every later Postcard discriminant keeps its value.
    PermissionRequest { peer: PeerId, capability: String },
    /// `peer` was granted permission `capability`.
    PermissionGrant { peer: PeerId, capability: String },
    /// A liveness beacon. Liveness is the transport's, so no build sends one
    /// on its own; a receiver reads one only as a sign that its sender is
    /// here. `peer` is the hashed Iroh `PeerId`.
    Heartbeat { peer: PeerId, ts: u64 },
    /// Authenticated identity chain for the transport peer on this workspace.
    ///
    /// The certificate binds the Iroh endpoint/device key to a user root. The
    /// additive, root-signed revocations let receivers reject a lost device
    /// before any of its collaboration payload reaches an app or replica.
    ///
    /// `protocol` is the sender's [`PEER_PROTOCOL_VERSION`], first, at the
    /// frozen [`IDENTITY_HELLO_INDEX`]: a receiver at another version admits
    /// nothing from the sender and knows which of the two is behind.
    IdentityHello {
        protocol: u32,
        cert: Box<tilo_user_identity::DeviceCert>,
        #[serde(default)]
        revocations: Vec<tilo_user_identity::RevocationEntry>,
    },
    /// One typed, signed document mutation crossing the Iroh boundary.
    /// Recipients authenticate the transport device, validate the envelope and
    /// decode its [`collab_types::DocumentMutation`] before local fan-out.
    /// Boxed so this variant does not bloat `NeighbourMsg`'s stack size.
    AppDocument {
        /// Hashed Iroh `PeerId` of the publisher; used to filter self-published
        /// echoes.
        peer: PeerId,
        /// App-id of the originating app, first routing-key component.
        app_id: String,
        /// Cluster id — second half of the routing key.
        cluster_id: String,
        /// Wire name of the originating document's record — third routing-key
        /// component; empty means the sender announced no record. A local
        /// record id that embeds a filesystem path is never sent: it is
        /// replaced by its sealed, workspace-scoped name (the compositor's
        /// `network::wire_document`), which receivers map back to their own
        /// local id.
        document_id: String,
        /// The signed-op envelope; the receiving shell's validator decides whether
        /// to fan it to local apps or drop it with an audit-log entry.
        envelope: Box<collab_types::SignedOp>,
        /// The reader set of the resource store's document this writes, as
        /// [`collab_types::Readers`] writes it: only its people's devices are
        /// sent it. In the clear, so a server that cannot read the document, nor
        /// tell the set from the digest its id carries, routes it. `None` for
        /// an app's own document, which every member reads.
        readers: Option<String>,
    },
    /// A page of a record's signed history, asked of the workspace's other devices. No device
    /// sends it any more, and a receiver acts on none: it keeps its place because the variant
    /// order is the wire discriminant.
    ScrollbackRequest {
        /// The artifact whose log to fetch.
        artifact: collab_types::ArtifactId,
        /// Exclusive upper bound on `seq`, same semantics as
        /// `CollabRequest::FetchScrollback::before_seq`.
        before_seq: u64,
        /// Page size cap, matching `CollabRequest::FetchScrollback::max` so the
        /// cooperative path doesn't re-cap.
        max: u16,
        /// Hashed Iroh peer id of the requester; receivers use it to skip their
        /// own echoed requests.
        requester: PeerId,
    },
    /// The answer to a [`NeighbourMsg::ScrollbackRequest`]. No device sends it any more, and a
    /// receiver acts on none: it keeps its place because the variant order is the wire
    /// discriminant.
    ScrollbackResponse {
        /// Same as the request's `artifact`, repeated so the receiver can route
        /// without matching on a request-id.
        artifact: collab_types::ArtifactId,
        /// The page of validated ops the responder is sharing; empty means "I don't
        /// have the range either" and is treated as a non-answer.
        ops: Vec<Box<collab_types::SignedOp>>,
        /// Hashed Iroh peer id of the answering broker.
        responder: PeerId,
    },
    /// Retired first generation of [`Self::IdentityAnnouncementV2`]: a
    /// root-signed device certificate announced on one workspace, without the
    /// revocations that keep a lost device out. No build produces it and
    /// receivers refuse it ([`NeighbourMsg::is_retired`]); it stays only so
    /// every later Postcard discriminant keeps its value.
    IdentityAnnouncement {
        workspace: String,
        device_cert: Box<tilo_user_identity::DeviceCert>,
    },
    /// V2 identity announcement carrying additive root-signed revocations.
    /// Appended rather than changing V1's Postcard payload layout.
    IdentityAnnouncementV2 {
        workspace: String,
        device_cert: Box<tilo_user_identity::DeviceCert>,
        /// Additive root-signed revocations for the certificate's human root.
        /// The workspace's host verifies and retains them independently of the
        /// announcing device, so omission can never resurrect a device.
        revocations: Vec<tilo_user_identity::RevocationEntry>,
    },
    /// One remote-action frame (`tilo_actions::remote::RemoteActionFrame`
    /// bytes) for exactly one authenticated device: a request to list,
    /// invoke, observe or cancel its actions, or the answer to one.
    ///
    /// It travels only on an exact-target route, never workspace-wide
    /// fan-out, so arguments and results are not disclosed to the rest of
    /// the workspace. The receiver takes the caller from the authenticated
    /// transport; nothing in the frame names it. Appended to preserve all
    /// existing Postcard discriminants.
    RemoteAction { frame: Vec<u8> },
    /// Retired: a shared window travels the workspace's live session
    /// (`tilo_live::share`). No build produces it and receivers refuse it
    /// ([`NeighbourMsg::is_retired`]); it stays only so every later Postcard
    /// discriminant keeps its value.
    ///
    /// Which video codecs a peer can encode and decode, split by whether
    /// hardware does it. Broadcast with the heartbeat so a screen-share sender
    /// can pick a codec every viewer takes: a peer that never sends this is
    /// assumed to decode H.264 only. Labels are the
    /// [`Self::SurfaceStreamStart::codec`] vocabulary; unknown ones are
    /// ignored. Appended to preserve every existing Postcard discriminant.
    MediaCapabilities {
        peer: PeerId,
        hardware_decode: Vec<String>,
        software_decode: Vec<String>,
        hardware_encode: Vec<String>,
        software_encode: Vec<String>,
    },
    /// One governance frame of a channel in `workspace`, published by the
    /// key holder that wrote its commit. Its Welcome, commit and key-history
    /// bytes are opaque MLS material: sealed to their recipients, never
    /// content. Receivers apply it only when its signer is a key holder and
    /// its changes match the membership, so replaying or reordering it is
    /// harmless. Appended
    /// to preserve every existing Postcard discriminant.
    MlsGovernance {
        workspace: String,
        frame: Box<collab_types::MlsGovernanceFrame>,
        /// Chosen by the sender for each delivery attempt. Redelivering the
        /// same content with a new value keeps transports that drop
        /// byte-identical messages from swallowing the retry; receivers
        /// ignore it.
        delivery: u64,
    },
    /// A device asking a channel's key holders for membership, with a key
    /// package signed by the device's certified key. Appended to preserve
    /// every existing Postcard discriminant.
    MlsAccessRequest {
        workspace: String,
        request: Box<collab_types::MlsAccessRequestV1>,
        /// Chosen by the sender for each delivery attempt. Redelivering the
        /// same content with a new value keeps transports that drop
        /// byte-identical messages from swallowing the retry; receivers
        /// ignore it.
        delivery: u64,
    },
    /// Retired: content is asked for many at a time
    /// ([`Self::DocumentContentWants`]). No build produces it and receivers
    /// refuse it ([`NeighbourMsg::is_retired`]); it stays only so every later
    /// Postcard discriminant keeps its value.
    DocumentContentWant {
        requester: PeerId,
        app_id: String,
        cluster_id: String,
        hash: String,
        have: Vec<content::Span>,
    },
    /// Retired: content is answered many pieces to a frame
    /// ([`Self::DocumentContentPieces`]). No build produces it and receivers
    /// refuse it ([`NeighbourMsg::is_retired`]); it stays only so every later
    /// Postcard discriminant keeps its value.
    DocumentContentPiece {
        responder: PeerId,
        requester: PeerId,
        app_id: String,
        cluster_id: String,
        hash: String,
        total: u64,
        offset: u64,
        bytes: Vec<u8>,
    },
    /// Retired: a shared window travels the workspace's live session
    /// (`tilo_live::share`). No build produces it and receivers refuse it
    /// ([`NeighbourMsg::is_retired`]); it stays only so every later Postcard
    /// discriminant keeps its value.
    ///
    /// Viewer `peer` cannot continue decoding `surface`: a unit before
    /// [`Self::SurfaceFrame::seq`] `seq` never reached it, or its decoder
    /// rejected one, and the deltas after reference what it lacks. The owner
    /// answers with a keyframe instead of leaving the viewer frozen until its
    /// periodic one. `seq` is the unit the viewer could not use; the owner
    /// ignores a request its last keyframe already answers. Appended to
    /// preserve every existing Postcard discriminant.
    SurfaceKeyframeRequest {
        peer: PeerId,
        surface: SurfaceId,
        seq: u64,
    },
    /// One of session `peer`'s live presence frames in the workspace
    /// (`tilo_server_protocol::live`, sealed with the channel keys of the
    /// record it names), on the workspace's own topic: how the frame travels
    /// when no workspace host's live session reaches the workspace. It is the same
    /// frame, read the same way as one the live session delivers, so what
    /// rides it (a wave) reaches the workspace's people with or without a host.
    /// Appended to preserve every existing Postcard discriminant.
    LivePresence { peer: PeerId, frame: Vec<u8> },
    /// Retired: a shared window travels the workspace's live session
    /// (`tilo_live::share`). No build produces it and receivers refuse it
    /// ([`NeighbourMsg::is_retired`]); it stays only so every later Postcard
    /// discriminant keeps its value.
    ///
    /// Viewer `peer`, a member of the workspace `workspace` names, shows these of the
    /// receiving device's windows (host and unconfined programs' windows, which cross
    /// a workspace that shares its layout as hosted on that device): it streams
    /// each to the viewer while the list keeps arriving and names it, read
    /// only. Sent to that device alone over the authenticated direct route,
    /// never to the workspace; an empty list ends every stream at once. Appended
    /// to preserve every existing Postcard discriminant.
    SurfaceViewRequest {
        peer: PeerId,
        workspace: String,
        views: Vec<SurfaceView>,
        decode: Vec<SurfaceDecodeLimit>,
    },
    /// `responder`'s answer to `requester`'s [`Self::DocumentContentWants`]
    /// for content `hash` when it keeps none of the content: the requester stops counting on
    /// it, and once every device it is connected to that could hold the
    /// content said so, the content is unavailable rather than on its way.
    /// Appended to preserve every existing Postcard discriminant.
    DocumentContentLacks {
        responder: PeerId,
        requester: PeerId,
        app_id: String,
        cluster_id: String,
        hash: String,
    },
    /// Every write `refusals[..].by` refused, as that device keeps them: sent
    /// as its whole set when one is filed, and as part of its catch-up, so a
    /// refusal reaches its author live and after an absence, and a set that
    /// no longer names a write is how one is cleared. One refusal is one
    /// device's verdict ([`WriteRefusal`]), never the store's or the
    /// workspace's. Appended to preserve every existing Postcard
    /// discriminant.
    WriteRefusals { refusals: Vec<WriteRefusal> },
    /// One of the person's devices holds a write of `workspace` sealed under
    /// a key it lacks: it asks the others for the frames of the workspace's
    /// group from `from_epoch`, the epoch it holds, on. A device that is in
    /// the group answers with [`NeighbourMsg::MlsGovernance`] frames when the
    /// workspace admits the asker. Travels only on the session between two of
    /// the person's devices, which proves who asks. Appended to preserve every
    /// existing Postcard discriminant.
    MlsGovernanceWanted { workspace: String, from_epoch: u64 },
    /// `responder`'s answer, for each content it named, to a
    /// [`Self::DocumentContentWants`] that named another device than the one
    /// that sent it: the ask cannot be answered
    /// as addressed, so it goes to the device that actually sent it, which
    /// logs it and stops counting on this responder instead of waiting on an
    /// answer that never comes. `requester` is that sending device, never the
    /// one the ask named. Appended to preserve every existing Postcard
    /// discriminant.
    DocumentContentRefused {
        responder: PeerId,
        requester: PeerId,
        app_id: String,
        cluster_id: String,
        hash: String,
    },
    /// A device asking workspace `cluster_id` for content its records refer
    /// to, each by its BLAKE3 hash ([`content::Wanted`]). Rides the
    /// workspace's topic, so only peers admitted to read the workspace see
    /// it; any of them holding some of the content answers with
    /// [`Self::DocumentContentPieces`], and with
    /// [`Self::DocumentContentLacks`] for each it keeps none of. What the
    /// asker already holds of each is left out of an answer: a device asks
    /// again while it is missing pieces, and every answer carries the next
    /// of them rather than all of them, so a large piece of content arrives
    /// without one device filling its peers' outbound queues. Appended to
    /// preserve every existing Postcard discriminant.
    DocumentContentWants {
        requester: PeerId,
        cluster_id: String,
        wants: Vec<content::Wanted>,
    },
    /// Bounded pieces of content of workspace `cluster_id`, answering
    /// `requester`'s [`Self::DocumentContentWants`], up to
    /// [`content::ANSWER_BYTES`] of them to a frame: the many small files of
    /// a tree travel together rather than one to a frame. The receiver keeps
    /// a content only once every piece of it arrived and its bytes hash to
    /// its address. Appended to preserve every existing Postcard
    /// discriminant.
    DocumentContentPieces {
        responder: PeerId,
        requester: PeerId,
        cluster_id: String,
        pieces: Vec<content::Piece>,
    },
    /// `responder`, a host of workspace `cluster_id`, keeps every byte of `hash` that
    /// `requester` pushed to it for a record of `app_id`: the push is answered, and the host holds
    /// the content for `requester` until it is released ([`Self::DocumentContentReleased`]).
    /// Appended to preserve every existing Postcard discriminant.
    DocumentContentKept {
        responder: PeerId,
        requester: PeerId,
        app_id: String,
        cluster_id: String,
        hash: String,
    },
    /// `requester`'s own change of a record of `app_id` dropped its last reference to `hash`:
    /// the hosts of workspace `cluster_id` hold it for that device no more, and let it go once
    /// no member holds it. Appended to preserve every existing Postcard discriminant.
    DocumentContentReleased {
        requester: PeerId,
        app_id: String,
        cluster_id: String,
        hash: String,
    },
}

impl NeighbourMsg {
    /// The frame `bytes` [`Self::decode`] reads back: what a peer sends or a store keeps of a
    /// message.
    ///
    /// # Errors
    ///
    /// The message carries more than a frame holds.
    pub fn encode(&self) -> Result<Vec<u8>, postcard::Error> {
        postcard::to_stdvec(self)
    }

    /// The frame of a message this build made itself ([`Self::encode`]): postcard writes every
    /// field these messages hold into a buffer that grows as it needs, which never fails.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "postcard writes these messages into a growing buffer, which never fails"
    )]
    pub fn frame(&self) -> Vec<u8> {
        self.encode()
            .expect("a message encodes into a growing buffer")
    }

    /// The message a frame `bytes` holds, whole: the one reader of a message that came from
    /// a peer, a server or a store.
    ///
    /// # Errors
    ///
    /// `bytes` are not one message, or hold more after it.
    pub fn decode(bytes: &[u8]) -> Result<Self, postcard::Error> {
        let (message, rest) = postcard::take_from_bytes(bytes)?;
        if rest.is_empty() {
            Ok(message)
        } else {
            Err(postcard::Error::DeserializeBadEncoding)
        }
    }

    /// Postcard variant index of [`NeighbourMsg::AppDocument`]. Variant order
    /// is wire-frozen (peers and hosts persist and exchange these frames), and
    /// a unit test pins this value to a real encoding.
    pub const APP_DOCUMENT_TAG: u8 = 10;

    /// Whether `bytes` encode an [`NeighbourMsg::AppDocument`], reading only
    /// the postcard variant tag and not the variant's fields.
    ///
    /// Postcard writes a variant index as a varint `u32` before the fields.
    /// An index below `0x80` is that single byte with its continuation bit
    /// clear, so a first byte equal to [`Self::APP_DOCUMENT_TAG`] is exactly
    /// that index and nothing longer. The fields are not checked: a frame
    /// tagged `AppDocument` whose fields do not decode still answers `true`.
    pub fn is_app_document(bytes: &[u8]) -> bool {
        const _: () = assert!(NeighbourMsg::APP_DOCUMENT_TAG < 0x80);
        bytes.first() == Some(&Self::APP_DOCUMENT_TAG)
    }

    /// Postcard variant index of [`NeighbourMsg::MlsGovernance`], pinned to
    /// a real encoding by a unit test as [`Self::APP_DOCUMENT_TAG`] is.
    pub const MLS_GOVERNANCE_TAG: u8 = 17;
    /// Postcard variant index of [`NeighbourMsg::MlsAccessRequest`].
    pub const MLS_ACCESS_REQUEST_TAG: u8 = 18;

    /// Whether `bytes` encode a governance frame
    /// ([`NeighbourMsg::MlsGovernance`]), reading only the variant tag as
    /// [`Self::is_app_document`] does.
    pub fn is_governance(bytes: &[u8]) -> bool {
        const _: () = assert!(NeighbourMsg::MLS_GOVERNANCE_TAG < 0x80);
        bytes.first() == Some(&Self::MLS_GOVERNANCE_TAG)
    }

    /// Whether `bytes` encode a device's request to be admitted to a
    /// workspace's group, reading only the variant tag as
    /// [`Self::is_app_document`] does.
    pub fn is_access_request(bytes: &[u8]) -> bool {
        const _: () = assert!(NeighbourMsg::MLS_ACCESS_REQUEST_TAG < 0x80);
        matches!(bytes.first(), Some(&Self::MLS_ACCESS_REQUEST_TAG))
    }

    /// Whether this is a retired wire generation that no current build
    /// produces. Receivers refuse such a message once, where it enters,
    /// before any handler or authorization sees it; the variant itself stays
    /// only because removing it would shift every later Postcard
    /// discriminant.
    pub fn is_retired(&self) -> bool {
        match self {
            // Superseded by `IdentityAnnouncementV2`, whose revocations a
            // receiver must see before trusting the certificate.
            Self::IdentityAnnouncement { .. }
            // Never acted on; control consent rides `PermissionGrant`.
            | Self::PermissionRequest { .. }
            // A shared window's stream, its viewers' asks and input and what each decodes travel
            // the workspace's live session (`tilo_live::share`).
            | Self::SurfaceFrame { .. }
            | Self::SurfaceStreamStart { .. }
            | Self::SurfaceStreamEnd { .. }
            | Self::RemoteInput { .. }
            | Self::MediaCapabilities { .. }
            | Self::SurfaceKeyframeRequest { .. }
            | Self::SurfaceViewRequest { .. }
            // Content is asked for and answered many at a time.
            | Self::DocumentContentWant { .. }
            | Self::DocumentContentPiece { .. } => true,
            Self::SyncReq { .. }
            | Self::SyncResp { .. }
            | Self::PermissionGrant { .. }
            | Self::Heartbeat { .. }
            | Self::IdentityHello { .. }
            | Self::AppDocument { .. }
            | Self::ScrollbackRequest { .. }
            | Self::ScrollbackResponse { .. }
            | Self::IdentityAnnouncementV2 { .. }
            | Self::RemoteAction { .. }
            | Self::MlsGovernance { .. }
            | Self::MlsAccessRequest { .. }
            | Self::DocumentContentLacks { .. }
            | Self::LivePresence { .. }
            | Self::WriteRefusals { .. }
            | Self::MlsGovernanceWanted { .. }
            | Self::DocumentContentRefused { .. }
            | Self::DocumentContentWants { .. }
            | Self::DocumentContentPieces { .. }
            | Self::DocumentContentKept { .. }
            | Self::DocumentContentReleased { .. } => false,
        }
    }

    /// The workspace a governance message names: a frame of the workspace's
    /// group, or a device asking to join it. `None` for any other message.
    #[must_use]
    pub fn governance_workspace(&self) -> Option<&str> {
        match self {
            Self::MlsGovernance { workspace, .. } | Self::MlsAccessRequest { workspace, .. } => {
                Some(
                    workspace
                        .split_once('/')
                        .map_or(workspace, |(workspace, _)| workspace),
                )
            }
            _ => None,
        }
    }

    /// The variant's name, for diagnostics and traffic tallies. Never a
    /// wire value: the postcard discriminant is.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::SyncReq { .. } => "SyncReq",
            Self::SyncResp { .. } => "SyncResp",
            Self::SurfaceFrame { .. } => "SurfaceFrame",
            Self::SurfaceStreamStart { .. } => "SurfaceStreamStart",
            Self::SurfaceStreamEnd { .. } => "SurfaceStreamEnd",
            Self::RemoteInput { .. } => "RemoteInput",
            Self::PermissionRequest { .. } => "PermissionRequest",
            Self::PermissionGrant { .. } => "PermissionGrant",
            Self::Heartbeat { .. } => "Heartbeat",
            Self::IdentityHello { .. } => "IdentityHello",
            Self::AppDocument { .. } => "AppDocument",
            Self::ScrollbackRequest { .. } => "ScrollbackRequest",
            Self::ScrollbackResponse { .. } => "ScrollbackResponse",
            Self::IdentityAnnouncement { .. } => "IdentityAnnouncement",
            Self::IdentityAnnouncementV2 { .. } => "IdentityAnnouncementV2",
            Self::RemoteAction { .. } => "RemoteAction",
            Self::MediaCapabilities { .. } => "MediaCapabilities",
            Self::MlsGovernance { .. } => "MlsGovernance",
            Self::MlsAccessRequest { .. } => "MlsAccessRequest",
            Self::DocumentContentWant { .. } => "DocumentContentWant",
            Self::DocumentContentPiece { .. } => "DocumentContentPiece",
            Self::SurfaceKeyframeRequest { .. } => "SurfaceKeyframeRequest",
            Self::LivePresence { .. } => "LivePresence",
            Self::SurfaceViewRequest { .. } => "SurfaceViewRequest",
            Self::DocumentContentLacks { .. } => "DocumentContentLacks",
            Self::WriteRefusals { .. } => "WriteRefusals",
            Self::MlsGovernanceWanted { .. } => "MlsGovernanceWanted",
            Self::DocumentContentRefused { .. } => "DocumentContentRefused",
            Self::DocumentContentWants { .. } => "DocumentContentWants",
            Self::DocumentContentPieces { .. } => "DocumentContentPieces",
            Self::DocumentContentKept { .. } => "DocumentContentKept",
            Self::DocumentContentReleased { .. } => "DocumentContentReleased",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Syntactically valid but unsigned; only the postcard framing needs to
    /// round-trip.
    fn dummy_envelope() -> Box<collab_types::SignedOp> {
        use collab_types::{AttestationId, Ed25519PublicKey, OpKind, SignedOp};
        use std::time::SystemTime;
        Box::new(SignedOp::new_unsigned(
            vec![0xDE, 0xAD, 0xBE, 0xEF],
            artifact(0x11),
            Ed25519PublicKey::from_bytes([7u8; 32]),
            AttestationId(uuid::Uuid::from_bytes([0x22; 16])),
            OpKind::Change,
            SystemTime::UNIX_EPOCH,
            42,
        ))
    }

    /// Shells in the field check publishers against this exact fold, so it
    /// is pinned like the encodings.
    #[test]
    fn peer_handle_is_fnv1a_of_the_device_key() {
        assert_eq!(peer_handle(&[7; 32]), 0xf160_fd08_b6d9_aac5);
    }

    /// Workspace id shared by every fixture that names one.
    const WORKSPACE: &str = "6b3f9a10-0000-4000-8000-00000000000c";

    /// Fixed artifact ids keep the golden encodings reproducible.
    fn artifact(fill: u8) -> collab_types::ArtifactId {
        collab_types::ArtifactId(uuid::Uuid::from_bytes([fill; 16]))
    }

    fn device_cert() -> Box<tilo_user_identity::DeviceCert> {
        Box::new(tilo_user_identity::DeviceCert {
            user_public_key: [1; 32],
            device_public_key: [2; 32],
            device_label: "Laptop".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 10,
            expires_at_ns: None,
            signature: [3; 64],
            issuer: None,
        })
    }

    /// Every version reads another device's version from its identity
    /// announcement: the variant index and the leading field never move.
    #[test]
    fn an_identity_hello_says_its_version_first_at_a_frozen_index() {
        for protocol in [PEER_PROTOCOL_VERSION, PEER_PROTOCOL_VERSION + 1, 300] {
            let bytes = postcard::to_allocvec(&NeighbourMsg::IdentityHello {
                protocol,
                cert: device_cert(),
                revocations: Vec::new(),
            })
            .unwrap();
            assert_eq!(bytes[0], IDENTITY_HELLO_INDEX);
            assert_eq!(identity_hello_version(&bytes), Some(protocol));
        }
        let beat = postcard::to_allocvec(&NeighbourMsg::Heartbeat { peer: 1, ts: 2 }).unwrap();
        assert_eq!(identity_hello_version(&beat), None);
        assert_eq!(identity_hello_version(&[]), None);
    }

    /// Every [`NeighbourMsg`] variant, split into thematic groups so no one
    /// function runs long — the round-trip test below exercises the
    /// concatenation.
    fn fixtures() -> Vec<(&'static str, NeighbourMsg)> {
        let mut all = fixtures_sync_presence_and_streams();
        all.extend(fixtures_window_views());
        all.extend(fixtures_input_permissions_and_identity());
        all.extend(fixtures_app_crdt_and_scrollback());
        all.extend(fixtures_governance());
        all.push((
            "live presence",
            NeighbourMsg::LivePresence {
                peer: 3,
                frame: vec![1, 2, 3],
            },
        ));
        all.push((
            "record content lacks",
            NeighbourMsg::DocumentContentLacks {
                responder: 3,
                requester: 4,
                app_id: "so.tilo.slides".into(),
                cluster_id: "workspace".into(),
                hash: "ab".into(),
            },
        ));
        all.push((
            "write refusals",
            NeighbourMsg::WriteRefusals {
                refusals: vec![WriteRefusal {
                    workspace: "workspace".into(),
                    app: "so.tilo.notes".into(),
                    record: "daily".into(),
                    author: [0x11; 32],
                    seq: 7,
                    operation: "op-7".into(),
                    changes: vec![[0x22; 32]],
                    reason: RefusalReason::Forbidden,
                    detail: "no permission".into(),
                    by: [0x33; 32],
                    at_ns: 9,
                    signature: vec![0x44; 4],
                }],
            },
        ));
        all.push((
            "content request refused",
            NeighbourMsg::DocumentContentRefused {
                responder: 3,
                requester: 4,
                app_id: "so.tilo.slides".into(),
                cluster_id: "workspace".into(),
                hash: "ab".into(),
            },
        ));
        all.push((
            "record content wants",
            NeighbourMsg::DocumentContentWants {
                requester: 0x00AB_CDEF,
                cluster_id: "workspace".into(),
                wants: vec![content::Wanted {
                    app_id: "so.tilo.slides".into(),
                    hash: "ab".into(),
                    have: vec![content::Span { offset: 0, len: 2 }],
                }],
            },
        ));
        all.push((
            "record content pieces",
            NeighbourMsg::DocumentContentPieces {
                responder: 0x123_456,
                requester: 0x00AB_CDEF,
                cluster_id: "workspace".into(),
                pieces: vec![content::Piece {
                    app_id: "so.tilo.slides".into(),
                    hash: "ab".into(),
                    total: 3,
                    offset: 0,
                    bytes: vec![1, 2, 3],
                }],
            },
        ));
        all.push((
            "record content kept",
            NeighbourMsg::DocumentContentKept {
                responder: 3,
                requester: 4,
                app_id: "so.tilo.photos".into(),
                cluster_id: "workspace".into(),
                hash: "ab".into(),
            },
        ));
        all.push((
            "record content released",
            NeighbourMsg::DocumentContentReleased {
                requester: 4,
                app_id: "so.tilo.photos".into(),
                cluster_id: "workspace".into(),
                hash: "ab".into(),
            },
        ));
        all.push((
            "governance wanted",
            NeighbourMsg::MlsGovernanceWanted {
                workspace: "workspace".into(),
                from_epoch: 2,
            },
        ));
        all
    }

    /// A member asking a host to stream it the host's windows it shows.
    fn fixtures_window_views() -> Vec<(&'static str, NeighbourMsg)> {
        vec![(
            "surface view request",
            NeighbourMsg::SurfaceViewRequest {
                peer: 3,
                workspace: "07848ffe-1898-4f72-8419-0b54c93e6dad".into(),
                views: vec![SurfaceView {
                    tab: 0x5EED_0000_0000_0007,
                    width: 1280,
                    height: 720,
                }],
                decode: vec![SurfaceDecodeLimit {
                    codec: "h264".into(),
                    max: Some((1920, 1080)),
                }],
            },
        )]
    }

    /// Channel governance, with fixed ids and signatures so the golden
    /// bytes stay reproducible (these fixtures are not verified).
    fn fixtures_governance() -> Vec<(&'static str, NeighbourMsg)> {
        use collab_types::{
            Ed25519PublicKey, MlsAccessRequestV1, MlsGovernanceAction, MlsGovernanceFrame,
            Signature, WorkspaceId,
        };
        let workspace = WorkspaceId::from_u128(0x5555_5555_5555_5555_5555_5555_5555_5555);
        vec![
            (
                "governance frame",
                NeighbourMsg::MlsGovernance {
                    workspace: WORKSPACE.into(),
                    frame: Box::new(MlsGovernanceFrame {
                        version: 2,
                        workspace,
                        previous_frame_digest: [7; 32],
                        epoch: 3,
                        action: MlsGovernanceAction::Commit {
                            removed: vec![Ed25519PublicKey::from_bytes([8; 32])],
                            added: Vec::new(),
                            commit: vec![0xC0, 0x33],
                            joining: None,
                            history: None,
                        },
                        signed_by: Ed25519PublicKey::from_bytes([9; 32]),
                        signature: Signature::from_bytes([0xAB; 64]),
                    }),
                    delivery: 7,
                },
            ),
            (
                "access request",
                NeighbourMsg::MlsAccessRequest {
                    workspace: WORKSPACE.into(),
                    request: Box::new(MlsAccessRequestV1 {
                        version: 1,
                        workspace,
                        device: Ed25519PublicKey::from_bytes([2; 32]),
                        key_package: vec![0x4B, 0x50],
                        issued_at: 1_700_000_000,
                        signature: Signature::from_bytes([0xCD; 64]),
                    }),
                    delivery: 8,
                },
            ),
        ]
    }

    fn fixtures_sync_presence_and_streams() -> Vec<(&'static str, NeighbourMsg)> {
        use crate::colorimetry::Colorimetry;
        use crate::hdr_metadata::Hdr10Metadata;

        vec![
            (
                "sync request",
                NeighbourMsg::SyncReq {
                    since: vec![1, 2, 3],
                    workspace: WORKSPACE.into(),
                    invite_id: Some([9; 32]),
                    nonce: 7,
                },
            ),
            (
                "sync response",
                NeighbourMsg::SyncResp {
                    changes: vec![vec![0xAA, 0xBB], vec![0xCC]],
                    workspace: WORKSPACE.into(),
                },
            ),
            (
                "surface frame",
                NeighbourMsg::SurfaceFrame {
                    surface: 7,
                    source_peer: 0xABCD,
                    encoded_h264: vec![0, 0, 0, 1, 0x67],
                    ts: 1_700_000_001,
                    seq: 3,
                },
            ),
            (
                "SDR stream start",
                NeighbourMsg::SurfaceStreamStart {
                    surface: 7,
                    source_peer: 0xABCD,
                    app_id: "org.example.Editor".into(),
                    width: 1280,
                    height: 720,
                    codec: "h264".into(),
                    ts: 1_700_000_002,
                    sender_monotonic_ns_at_send: 1_700_000_002,
                    colorimetry: None,
                    hdr_static_metadata: None,
                    bit_depth: 0,
                    tab: 0,
                },
            ),
            (
                "HDR stream start",
                NeighbourMsg::SurfaceStreamStart {
                    surface: 8,
                    source_peer: 0xABCD,
                    app_id: "org.example.HdrEditor".into(),
                    width: 1920,
                    height: 1080,
                    codec: "hevc-main10".into(),
                    ts: 1_700_000_003,
                    sender_monotonic_ns_at_send: 1_700_000_003,
                    colorimetry: Some(Colorimetry::hdr10()),
                    hdr_static_metadata: Some(Hdr10Metadata::reference_bt2020()),
                    bit_depth: 10,
                    tab: 0,
                },
            ),
            (
                "stream end",
                NeighbourMsg::SurfaceStreamEnd {
                    surface: 7,
                    source_peer: 0xABCD,
                    ts: 1_700_000_004,
                },
            ),
            (
                "surface keyframe request",
                NeighbourMsg::SurfaceKeyframeRequest {
                    peer: 3,
                    surface: 7,
                    seq: 300,
                },
            ),
        ]
    }

    fn fixtures_input_permissions_and_identity() -> Vec<(&'static str, NeighbourMsg)> {
        vec![
            (
                "remote input",
                NeighbourMsg::RemoteInput {
                    peer: 3,
                    surface: 7,
                    event: vec![0xDE, 0xAD, 0xBE, 0xEF],
                },
            ),
            (
                "permission request",
                NeighbourMsg::PermissionRequest {
                    peer: 5,
                    capability: "share-screen".into(),
                },
            ),
            (
                "permission grant",
                NeighbourMsg::PermissionGrant {
                    peer: 5,
                    capability: "share-screen".into(),
                },
            ),
            (
                "heartbeat",
                NeighbourMsg::Heartbeat {
                    peer: 0xDEAD_BEEF,
                    ts: 1_700_000_005,
                },
            ),
            (
                "identity hello",
                NeighbourMsg::IdentityHello {
                    protocol: crate::PEER_PROTOCOL_VERSION,
                    cert: device_cert(),
                    revocations: Vec::new(),
                },
            ),
            (
                "identity announcement",
                NeighbourMsg::IdentityAnnouncement {
                    workspace: WORKSPACE.into(),
                    device_cert: device_cert(),
                },
            ),
            (
                "identity announcement v2",
                NeighbourMsg::IdentityAnnouncementV2 {
                    workspace: WORKSPACE.into(),
                    device_cert: device_cert(),
                    revocations: Vec::new(),
                },
            ),
            (
                "remote action",
                NeighbourMsg::RemoteAction {
                    frame: b"{}".to_vec(),
                },
            ),
            (
                "media capabilities",
                NeighbourMsg::MediaCapabilities {
                    peer: 0xABCD,
                    hardware_decode: vec!["av1".into(), "vp9".into()],
                    software_decode: vec!["h264".into()],
                    hardware_encode: vec!["av1".into()],
                    software_encode: vec!["vp9".into()],
                },
            ),
        ]
    }

    fn fixtures_app_crdt_and_scrollback() -> Vec<(&'static str, NeighbourMsg)> {
        vec![
            (
                "signed app document",
                NeighbourMsg::AppDocument {
                    peer: 0x00AB_CDEF,
                    app_id: "notes".into(),
                    cluster_id: WORKSPACE.into(),
                    document_id: "uuid:1c2f".into(),
                    envelope: dummy_envelope(),
                    readers: None,
                },
            ),
            (
                "record content want",
                NeighbourMsg::DocumentContentWant {
                    requester: 0x00AB_CDEF,
                    app_id: "so.tilo.slides".into(),
                    cluster_id: "workspace".into(),
                    hash: "ab".into(),
                    have: Vec::new(),
                },
            ),
            (
                "record content piece",
                NeighbourMsg::DocumentContentPiece {
                    responder: 0x123_456,
                    requester: 0x00AB_CDEF,
                    app_id: "so.tilo.slides".into(),
                    cluster_id: "workspace".into(),
                    hash: "ab".into(),
                    total: 3,
                    offset: 0,
                    bytes: vec![1, 2, 3],
                },
            ),
            (
                "scrollback request",
                NeighbourMsg::ScrollbackRequest {
                    artifact: artifact(0x33),
                    before_seq: 42,
                    max: 50,
                    requester: 0x00AB_CDEF,
                },
            ),
            (
                "scrollback response",
                NeighbourMsg::ScrollbackResponse {
                    artifact: artifact(0x44),
                    ops: vec![dummy_envelope(), dummy_envelope()],
                    responder: 0x123_456,
                },
            ),
        ]
    }

    /// Every message round-trips over the production codec and every variant encodes to its
    /// pinned bytes.
    /// Runs each phase below in order; every phase builds its own fixture.
    #[test]
    fn every_message_round_trips_and_encodes_to_its_pinned_bytes() {
        every_message_round_trips_over_the_production_codec();
        every_variant_encodes_to_its_pinned_bytes();
    }

    fn every_message_round_trips_over_the_production_codec() {
        for (case, message) in fixtures() {
            let bytes = postcard::to_allocvec(&message)
                .unwrap_or_else(|error| panic!("{case} failed to encode: {error}"));
            let decoded: NeighbourMsg = postcard::from_bytes(&bytes)
                .unwrap_or_else(|error| panic!("{case} failed to decode: {error}"));
            assert_eq!(message, decoded, "{case} changed in transit");
        }
    }

    /// A frame holds one message: more after it is refused.
    #[test]
    fn a_frame_reads_as_one_whole_message() {
        let (_, message) = fixtures_app_crdt_and_scrollback()
            .into_iter()
            .find(|(case, _)| *case == "signed app document")
            .unwrap();
        let today = postcard::to_allocvec(&message).unwrap();
        assert_eq!(NeighbourMsg::decode(&today).unwrap(), message);
        let mut longer = today.clone();
        longer.push(0);
        assert!(NeighbourMsg::decode(&longer).is_err());
        let mut shorter = today;
        shorter.pop();
        assert!(NeighbourMsg::decode(&shorter).is_err());
    }

    /// The tag peek agrees with a full decode on a real encoding of every
    /// variant, and the pinned tag is the varint postcard writes for
    /// `AppDocument`.
    #[test]
    fn app_document_tag_matches_encoding() {
        let mut app_documents = 0;
        for (case, message) in fixtures() {
            let bytes = postcard::to_allocvec(&message).unwrap();
            let is_app_document = matches!(message, NeighbourMsg::AppDocument { .. });
            assert_eq!(
                NeighbourMsg::is_app_document(&bytes),
                is_app_document,
                "{case}"
            );
            if is_app_document {
                app_documents += 1;
                let (tag, _) = postcard::take_from_bytes::<u32>(&bytes).unwrap();
                assert_eq!(tag, u32::from(NeighbourMsg::APP_DOCUMENT_TAG), "{case}");
            }
        }
        assert!(app_documents > 0, "no AppDocument fixture was classified");
        assert!(!NeighbourMsg::is_app_document(&[]));
    }

    /// The channel-governance tag peeks agree with a full decode on a real
    /// encoding of every variant.
    #[test]
    fn governance_tags_match_encoding() {
        let (mut governance, mut requests) = (0, 0);
        for (case, message) in fixtures() {
            let bytes = postcard::to_allocvec(&message).unwrap();
            let is_governance = matches!(message, NeighbourMsg::MlsGovernance { .. });
            let is_request = matches!(message, NeighbourMsg::MlsAccessRequest { .. });
            assert_eq!(NeighbourMsg::is_governance(&bytes), is_governance, "{case}");
            assert_eq!(
                NeighbourMsg::is_access_request(&bytes),
                is_request,
                "{case}"
            );
            governance += usize::from(is_governance);
            requests += usize::from(is_request);
        }
        assert!(
            governance > 0 && requests > 0,
            "no channel governance fixture"
        );
        assert!(!NeighbourMsg::is_governance(&[]));
        assert!(!NeighbourMsg::is_access_request(&[]));
    }

    /// Only generations no build produces any more are retired: the identity
    /// predecessor, the permission request control consent replaced, and a
    /// shared window's messages, which travel the live session. Every other
    /// variant, including the identity successor, stays live.
    #[test]
    fn only_the_unproduced_generations_are_retired() {
        let mut retired: Vec<&str> = fixtures()
            .iter()
            .filter(|(_, message)| message.is_retired())
            .map(|(_, message)| variant(message))
            .collect();
        retired.sort_unstable();
        retired.dedup();
        assert_eq!(
            retired,
            [
                "DocumentContentPiece",
                "DocumentContentWant",
                "IdentityAnnouncement",
                "MediaCapabilities",
                "PermissionRequest",
                "RemoteInput",
                "SurfaceFrame",
                "SurfaceKeyframeRequest",
                "SurfaceStreamEnd",
                "SurfaceStreamStart",
                "SurfaceViewRequest",
            ]
        );
    }

    /// Names every variant through an exhaustive match, so adding a variant
    /// fails to compile until it has a fixture and a golden encoding.
    fn variant(message: &NeighbourMsg) -> &'static str {
        message.kind()
    }

    const VARIANT_COUNT: usize = 32;

    /// Postcard encodings pinned byte for byte. Peers running older builds
    /// decode these frames, so a change here is a wire break, not a refactor.
    const GOLDEN: &[(&str, &str)] = &[
        (
            "sync request",
            concat!(
                "00030102032436623366396131302d303030302d343030302d383030302d3030303030303030",
                "3030306301090909090909090909090909090909090909090909090909090909090909090907",
            ),
        ),
        (
            "sync response",
            concat!(
                "010202aabb01cc2436623366396131302d303030302d343030302d383030302d303030303030",
                "303030303063",
            ),
        ),
        ("surface frame", "0207cdd70205000000016781e2cfaa0603"),
        (
            "SDR stream start",
            concat!(
                "0307cdd702800ad005046832363482e2cfaa0682e2cfaa06000000126f72672e6578616d706c",
                "652e456469746f7200",
            ),
        ),
        (
            "HDR stream start",
            concat!(
                "0308cdd702800fb8080b686576632d6d61696e313083e2cfaa0683e2cfaa06010101010001c8",
                "94028872b442aab7029633fc11937ac2800180ade204f40300000a156f72672e6578616d706c",
                "652e486472456469746f7200",
            ),
        ),
        ("stream end", "0407cdd70284e2cfaa06"),
        ("surface keyframe request", "150307ac02"),
        (
            "surface view request",
            concat!(
                "17032430373834386666652d313839382d346637322d383431392d3062353463393365366461",
                "6401878080808080c0f65e800ad00501046832363401800fb808",
            ),
        ),
        ("remote input", "05030704deadbeef"),
        ("permission request", "06050c73686172652d73637265656e"),
        ("permission grant", "07050c73686172652d73637265656e"),
        ("heartbeat", "08effdb6f50d85e2cfaa06"),
        (
            "identity hello",
            concat!(
                "0948010101010101010101010101010101010101010101010101010101010101010102020202",
                "02020202020202020202020202020202020202020202020202020202064c6170746f70010000",
                "000a004003030303030303030303030303030303030303030303030303030303030303030303",
                "03030303030303030303030303030303030303030303030303030303030300",
            ),
        ),
        (
            "identity announcement",
            concat!(
                "0d2436623366396131302d303030302d343030302d383030302d303030303030303030303063",
                "0101010101010101010101010101010101010101010101010101010101010101020202020202",
                "0202020202020202020202020202020202020202020202020202064c6170746f70010000000a",
                "0040030303030303030303030303030303030303030303030303030303030303030303030303",
                "03030303030303030303030303030303030303030303030303030303",
            ),
        ),
        (
            "identity announcement v2",
            concat!(
                "0e2436623366396131302d303030302d343030302d383030302d303030303030303030303063",
                "0101010101010101010101010101010101010101010101010101010101010101020202020202",
                "0202020202020202020202020202020202020202020202020202064c6170746f70010000000a",
                "0040030303030303030303030303030303030303030303030303030303030303030303030303",
                "0303030303030303030303030303030303030303030303030303030300",
            ),
        ),
        ("remote action", "0f027b7d"),
        (
            "media capabilities",
            "10cdd70202036176310376703901046832363401036176310103767039",
        ),
        (
            "signed app document",
            concat!(
                "0aef9baf05056e6f7465732436623366396131302d303030302d343030302d383030302d3030",
                "3030303030303030306309757569643a3163326604deadbeef10111111111111111111111111",
                "1111111107070707070707070707070707070707070707070707070707070707070707071022",
                "2222222222222222222222222222220000002a00000000400000000000000000000000000000",
                "0000000000000000000000000000000000000000000000000000000000000000000000000000",
                "00000000000000000000000000",
            ),
        ),
        (
            "record content want",
            "13ef9baf050e736f2e74696c6f2e736c6964657309776f726b737061636502616200",
        ),
        (
            "record content piece",
            concat!(
                "14d6e848ef9baf050e736f2e74696c6f2e736c6964657309776f726b73706163650261620300",
                "03010203",
            ),
        ),
        (
            "scrollback request",
            "0b10333333333333333333333333333333332a32ef9baf05",
        ),
        (
            "scrollback response",
            concat!(
                "0c10444444444444444444444444444444440204deadbeef1011111111111111111111111111",
                "1111110707070707070707070707070707070707070707070707070707070707070707102222",
                "22222222222222222222222222220000002a0000000040000000000000000000000000000000",
                "0000000000000000000000000000000000000000000000000000000000000000000000000000",
                "000000000000000000000004deadbeef10111111111111111111111111111111110707070707",
                "0707070707070707070707070707070707070707070707070707071022222222222222222222",
                "2222222222220000002a00000000400000000000000000000000000000000000000000000000",
                "0000000000000000000000000000000000000000000000000000000000000000000000000000",
                "000000d6e848",
            ),
        ),
        (
            "governance frame",
            concat!(
                "112436623366396131302d303030302d343030302d383030302d303030303030303030303063",
                "0210555555555555555555555555555555550707070707070707070707070707070707070707",
                "0707070707070707070707070301010808080808080808080808080808080808080808080808",
                "0808080808080808080002c03300000909090909090909090909090909090909090909090909",
                "09090909090909090940abababababababababababababababababababababababababababab",
                "abababababababababababababababababababababababababababababababababababab07",
            ),
        ),
        (
            "access request",
            concat!(
                "122436623366396131302d303030302d343030302d383030302d303030303030303030303063",
                "0110555555555555555555555555555555550202020202020202020202020202020202020202",
                "020202020202020202020202024b5080e2cfaa0640cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd",
                "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd",
                "cdcdcdcdcdcdcdcdcd08",
            ),
        ),
        ("live presence", "160303010203"),
        (
            "record content lacks",
            "1803040e736f2e74696c6f2e736c6964657309776f726b7370616365026162",
        ),
        (
            "write refusals",
            concat!(
                "190109776f726b73706163650d736f2e74696c6f2e6e6f746573056461696c79111111111111",
                "111111111111111111111111111111111111111111111111111107046f702d37012222222222",
                "222222222222222222222222222222222222222222222222222222030d6e6f207065726d6973",
                "73696f6e33333333333333333333333333333333333333333333333333333333333333330904",
                "44444444",
            ),
        ),
        (
            "content request refused",
            "1b03040e736f2e74696c6f2e736c6964657309776f726b7370616365026162",
        ),
        (
            "record content wants",
            "1cef9baf0509776f726b7370616365010e736f2e74696c6f2e736c69646573026162010002",
        ),
        (
            "record content pieces",
            "1dd6e848ef9baf0509776f726b7370616365010e736f2e74696c6f2e736c69646573026162030003010203",
        ),
        (
            "record content kept",
            "1e03040e736f2e74696c6f2e70686f746f7309776f726b7370616365026162",
        ),
        (
            "record content released",
            "1f040e736f2e74696c6f2e70686f746f7309776f726b7370616365026162",
        ),
        ("governance wanted", "1a09776f726b737061636502"),
    ];

    /// A refusal is only kept by a receiver that can check it: the signature
    /// covers every field, so a changed reason or device is no refusal.
    #[test]
    fn a_refusal_is_signed_over_its_contents() {
        let key = ed25519_dalek::SigningKey::from_bytes(&[0x51; 32]);
        let other = ed25519_dalek::SigningKey::from_bytes(&[0x52; 32]);
        let mut refusal = WriteRefusal {
            workspace: "workspace".into(),
            app: "so.tilo.notes".into(),
            record: "daily".into(),
            author: [0x11; 32],
            seq: 7,
            operation: "op-7".into(),
            changes: vec![[0x22; 32]],
            reason: RefusalReason::Forbidden,
            detail: "no permission".into(),
            by: [0; 32],
            at_ns: 9,
            signature: Vec::new(),
        };
        refusal.sign(&key).expect("a refusal signs");
        assert!(refusal.signed_by(&key.verifying_key().to_bytes()));
        assert!(!refusal.signed_by(&other.verifying_key().to_bytes()));
        let mut changed = refusal.clone();
        changed.reason = RefusalReason::Revoked;
        assert!(!changed.signed_by(&key.verifying_key().to_bytes()));
        let mut moved = refusal.clone();
        moved.at_ns = 10;
        assert!(!moved.signed_by(&key.verifying_key().to_bytes()));
    }

    fn every_variant_encodes_to_its_pinned_bytes() {
        let fixtures = fixtures();
        let covered: std::collections::BTreeSet<_> = fixtures
            .iter()
            .map(|(_, message)| variant(message))
            .collect();
        assert_eq!(
            covered.len(),
            VARIANT_COUNT,
            "every variant needs a fixture"
        );
        let mut actual = Vec::new();
        for (case, message) in &fixtures {
            let bytes = postcard::to_allocvec(message).expect("fixture encodes");
            actual.push((*case, hex::encode(&bytes)));
        }
        let expected: Vec<_> = GOLDEN
            .iter()
            .map(|(case, bytes)| (*case, (*bytes).to_owned()))
            .collect();
        assert_eq!(actual, expected);
    }
}
