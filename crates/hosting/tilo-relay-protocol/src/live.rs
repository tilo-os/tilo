//! Live plane: bounded, inert values for ephemeral multiplayer state —
//! pointer anchors and an app's ordered record previews — as a device
//! publishes them on its own live track.
//!
//! Live frames are never durable and never authority. Documents change only
//! through signed durable deltas; a preview is an overlay a receiver drops
//! once the record's durable change lands.
//!
//! A live frame travels only as a [`SealedLiveFrame`]: the frame sealed with
//! the content keys of the channel of the record it names. Its kind,
//! sequence and record digest are in the clear so a carrier can rate-limit
//! and route a record's previews; the content never is. A frame that names
//! no record has no channel and is never sent.

use collab_types::{
    ArtifactId, ChannelId, ContentRetention, EncryptionMetadata, EncryptionScheme, live_artifact_id,
};
use serde::{Deserialize, Deserializer, Serialize, de};

mod layout;
mod pointer;
pub use layout::{LAYOUT_UNITS, LiveLayout, LiveWindow, MAX_LAYOUT_APP_BYTES, MAX_LAYOUT_WINDOWS};
pub use pointer::{LivePointer, PointerLayout, ShownStrip};

/// Largest encoded presence frame. Kept at datagram size so presence can
/// move to QUIC datagrams without a format change.
pub const MAX_PRESENCE_FRAME_BYTES: usize = 1100;
/// Largest encoded ordered frame.
pub const MAX_OPS_FRAME_BYTES: usize = 64 * 1024;
/// What sealing adds to a frame: the clear tags, the channel and epoch, the
/// envelope's nonce and authentication tag.
pub const SEALED_LIVE_OVERHEAD_BYTES: usize = 256;
/// Largest sealed presence frame.
pub const MAX_SEALED_PRESENCE_FRAME_BYTES: usize =
    MAX_PRESENCE_FRAME_BYTES + SEALED_LIVE_OVERHEAD_BYTES;
/// Largest sealed ordered frame.
pub const MAX_SEALED_OPS_FRAME_BYTES: usize = MAX_OPS_FRAME_BYTES + SEALED_LIVE_OVERHEAD_BYTES;
/// Text preview payload ceiling per operation.
pub const MAX_PREVIEW_TEXT_BYTES: usize = 1024;
/// Text preview operations per frame.
pub const MAX_PREVIEW_OPS: usize = 32;
/// Canonical action-interaction JSON ceiling, matching the action protocol.
pub const MAX_SEMANTIC_INTERACTION_BYTES: usize = 32 * 1024;

/// Broker-reported semantic work on one exact record. This is an inert wire
/// value, never physical input or proof of a document mutation. Receivers must
/// authenticate the transport publisher, validate the canonical interaction
/// and bind its principal and resource before presenting it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticFrame {
    /// Version of this semantic envelope; currently 1.
    pub version: u16,
    /// Monotonic source sequence on the publisher's ordered record stream.
    pub seq: u64,
    /// Source wall-clock timestamp, not an ordering authority.
    pub sent_at_ms: u64,
    /// Host-minted execution correlation under the authenticated device.
    pub execution: uuid::Uuid,
    /// Exact source application instance, never an app-id selector.
    pub peer: u64,
    /// Canonical `tilo_actions::SemanticInteraction` JSON. The wire crate stays
    /// independent of the action executor; its consumer validates that schema.
    pub interaction_json: String,
    /// The run (its `agent-run:` id) the workspace's host narrates this for:
    /// an agent's or a key's run the host executes. A reader takes it only
    /// from the device the workspace's governance pins as its host, as the
    /// member the host speaks for that run. `None` for a device narrating
    /// its own agent.
    #[serde(default)]
    pub on_behalf: Option<String>,
}

impl SemanticFrame {
    fn validate(&self) -> Result<()> {
        if self.version != 1
            || self.seq == 0
            || self.sent_at_ms == 0
            || self.execution.is_nil()
            || self.peer == 0
            || self.interaction_json.len() > MAX_SEMANTIC_INTERACTION_BYTES
            || self
                .on_behalf
                .as_ref()
                .is_some_and(|run| run.is_empty() || run.len() > MAX_OBJECT_ID_BYTES * 2)
            || !serde_json::from_str::<serde_json::Value>(&self.interaction_json)
                .is_ok_and(|value| value.is_object())
        {
            return Err(LiveWireError::Invalid);
        }
        Ok(())
    }
}
/// Opaque object identifier and anchor ceiling (window, shape, block).
pub const MAX_OBJECT_ID_BYTES: usize = 64;
/// Scoped text anchor: `[1, uvarint(offset), postcard(scope key)]`.
pub const ANCHOR_TAG_SCOPED: u8 = 1;
/// Same-scope head: `[2, uvarint(offset)]`, reusing the anchor's scope.
pub const ANCHOR_TAG_SAME_SCOPE: u8 = 2;
/// Digest-scoped anchor: `[3, uvarint(offset), 16-byte scope digest]`, used
/// when the scoped form exceeds [`MAX_OBJECT_ID_BYTES`].
pub const ANCHOR_TAG_DIGEST: u8 = 3;

/// Closed decoding/validation failures.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LiveWireError {
    /// Malformed, oversized, truncated or noncanonical encoding.
    #[error("invalid live wire encoding")]
    Codec,
    /// Invalid bounded field.
    #[error("invalid live wire field")]
    Invalid,
}
type Result<T> = std::result::Result<T, LiveWireError>;

fn object_id(bytes: &[u8]) -> Result<()> {
    if bytes.is_empty() || bytes.len() > MAX_OBJECT_ID_BYTES {
        Err(LiveWireError::Invalid)
    } else {
        Ok(())
    }
}

/// Record reference shared by every member: application and record id.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveRecord {
    #[serde(deserialize_with = "text128")]
    pub app: String,
    /// Record identifier within the application; empty names its package-primary record.
    #[serde(deserialize_with = "record128")]
    pub record: String,
}

impl LiveRecord {
    /// Stable digest naming this record's ordered stream.
    pub fn digest(&self) -> [u8; 32] {
        let mut hash = blake3::Hasher::new();
        hash.update(b"tilo-live-record-v1\0");
        hash.update(&(self.app.len() as u64).to_le_bytes());
        hash.update(self.app.as_bytes());
        hash.update(self.record.as_bytes());
        *hash.finalize().as_bytes()
    }

    fn validate(&self) -> Result<()> {
        if self.app.is_empty() || self.app.len() > 128 || self.record.len() > 128 {
            Err(LiveWireError::Invalid)
        } else {
            Ok(())
        }
    }
}

/// Text position as a stable, application-encoded CRDT anchor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveAnchor {
    /// Opaque cursor bytes, 1..=[`MAX_OBJECT_ID_BYTES`].
    #[serde(with = "object_bytes")]
    pub cursor: Vec<u8>,
}

/// The editor a selection anchor names: its postcard scope key, or the key's
/// 16-byte scope digest when the scoped form would not fit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnchorScope<'a> {
    Key(&'a [u8]),
    /// The 16-byte digest of the editor's scope key.
    Digest([u8; 16]),
}

/// A selection from `start` to `end` in the editor whose postcard scope key
/// is `key`, as an anchor and a same-scope head. `digest` is asked for only
/// when the scoped anchor exceeds [`MAX_OBJECT_ID_BYTES`]; `None` when even
/// the digest form does not fit.
pub fn selection_anchors(
    key: &[u8],
    digest: impl FnOnce() -> Option<[u8; 16]>,
    start: u64,
    end: u64,
) -> Option<(LiveAnchor, LiveAnchor)> {
    let mut anchor = vec![ANCHOR_TAG_SCOPED];
    push_uvarint(&mut anchor, start);
    anchor.extend_from_slice(key);
    if anchor.len() > MAX_OBJECT_ID_BYTES {
        anchor = vec![ANCHOR_TAG_DIGEST];
        push_uvarint(&mut anchor, start);
        anchor.extend_from_slice(&digest()?);
    }
    let mut head = vec![ANCHOR_TAG_SAME_SCOPE];
    push_uvarint(&mut head, end);
    (anchor.len() <= MAX_OBJECT_ID_BYTES)
        .then_some((LiveAnchor { cursor: anchor }, LiveAnchor { cursor: head }))
}

/// The editor scope and range of a pair made by [`selection_anchors`].
pub fn read_selection_anchors<'a>(
    anchor: &'a LiveAnchor,
    head: &LiveAnchor,
) -> Option<(AnchorScope<'a>, u64, u64)> {
    let (&tag, rest) = anchor.cursor.split_first()?;
    let (start, key) = read_uvarint(rest)?;
    let scope = match tag {
        ANCHOR_TAG_SCOPED => AnchorScope::Key(key),
        ANCHOR_TAG_DIGEST => AnchorScope::Digest(key.try_into().ok()?),
        _ => return None,
    };
    let (&tag, rest) = head.cursor.split_first()?;
    if tag != ANCHOR_TAG_SAME_SCOPE {
        return None;
    }
    let (end, _) = read_uvarint(rest)?;
    Some((scope, start, end))
}

/// A caret (`start == end`) or selection in the editor `key`, as the live plane's selection
/// anchors. A key too long for the anchor (a framework resource key naming a long item id) travels
/// as its scope digest, which the receiving editor resolves against its own keys
/// ([`PresenceKey::digest_form`]); `None` when even that does not fit.
#[must_use]
pub fn presence_anchors(
    key: &collab_types::presence::PresenceKey,
    start: usize,
    end: usize,
) -> Option<(LiveAnchor, LiveAnchor)> {
    selection_anchors(
        &postcard::to_stdvec(key).ok()?,
        || key.scope_digest(),
        u64::try_from(start).ok()?,
        u64::try_from(end).ok()?,
    )
}

/// The editor key and range of anchors made by [`presence_anchors`].
#[must_use]
pub fn read_presence_anchors(
    anchor: &LiveAnchor,
    head: &LiveAnchor,
) -> Option<(collab_types::presence::PresenceKey, usize, usize)> {
    let (scope, start, end) = read_selection_anchors(anchor, head)?;
    let key = match scope {
        AnchorScope::Key(key) => postcard::from_bytes(key).ok()?,
        AnchorScope::Digest(digest) => {
            collab_types::presence::PresenceKey::from_scope_digest(digest)
        }
    };
    Some((
        key,
        usize::try_from(start).ok()?,
        usize::try_from(end).ok()?,
    ))
}

fn push_uvarint(out: &mut Vec<u8>, mut value: u64) {
    loop {
        let byte = u8::try_from(value & 0x7f).unwrap_or(0);
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn read_uvarint(bytes: &[u8]) -> Option<(u64, &[u8])> {
    let mut value = 0u64;
    for (index, (byte, shift)) in bytes.iter().zip((0u32..64).step_by(7)).enumerate() {
        value |= u64::from(byte & 0x7f).checked_shl(shift)?;
        if byte & 0x80 == 0 {
            return Some((value, bytes.get(index.checked_add(1)?..)?));
        }
    }
    None
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
        /// Characters deleted.
        len: u32,
    },
}

/// Ordered per-record frame.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpsFrame {
    /// Preview of local edits not yet durably committed.
    Preview {
        /// Monotonic per-publisher, per-record sequence.
        seq: u64,
        record: LiveRecord,
        /// 1..=[`MAX_PREVIEW_OPS`] operations.
        ops: Vec<PreviewOp>,
    },
    /// The publisher's previews for `record` are over.
    End {
        /// Record whose previews end.
        record: LiveRecord,
    },
    /// Semantic invocation activity, separate from pointers and text previews.
    /// Appended for wire compatibility: older receivers reject this variant.
    Semantic {
        /// Explicit audience and presentation record.
        record: LiveRecord,
        /// Inert event that still requires authenticated consumer admission.
        event: SemanticFrame,
    },
}

impl OpsFrame {
    /// The record this frame concerns.
    pub fn record(&self) -> &LiveRecord {
        match self {
            Self::Preview { record, .. } | Self::End { record } | Self::Semantic { record, .. } => {
                record
            }
        }
    }

    fn validate(&self) -> Result<()> {
        self.record().validate()?;
        if let Self::Semantic { event, .. } = self {
            return event.validate();
        }
        let Self::Preview { ops, .. } = self else {
            return Ok(());
        };
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
}

/// One sealed live frame's content: an ordered per-record frame, or nothing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum LiveFrame {
    /// Retired: latest-value presence. Presence travels per device on the
    /// workspace's live plane; the variant stays only so every later one
    /// keeps its postcard position, and a frame written under it is dropped.
    RetiredPresence,
    /// Ordered per-record frame.
    Ops(OpsFrame),
}

impl LiveFrame {
    fn validate(&self) -> Result<()> {
        match self {
            Self::RetiredPresence => Ok(()),
            Self::Ops(frame) => frame.validate(),
        }
    }

    fn cap(&self) -> usize {
        match self {
            Self::RetiredPresence => MAX_PRESENCE_FRAME_BYTES,
            Self::Ops(_) => MAX_OPS_FRAME_BYTES,
        }
    }

    /// Canonical encoding after checking every bound.
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let bytes = postcard::to_allocvec(self).map_err(|_| LiveWireError::Codec)?;
        if bytes.len() > self.cap() {
            return Err(LiveWireError::Codec);
        }
        Ok(bytes)
    }

    /// Decode, rejecting oversize and noncanonical input.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() || bytes.len() > MAX_OPS_FRAME_BYTES {
            return Err(LiveWireError::Codec);
        }
        let value: Self = postcard::from_bytes(bytes).map_err(|_| LiveWireError::Codec)?;
        if value.encode()? != bytes {
            return Err(LiveWireError::Codec);
        }
        Ok(value)
    }
}

/// What a sealed live frame says in the clear about its kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LiveKind {
    /// Retired: latest-value presence. It stays only so `Ops` keeps its
    /// postcard position.
    RetiredPresence,
    /// An ordered per-record frame.
    Ops,
}

impl LiveKind {
    const fn tag(self) -> u8 {
        match self {
            Self::RetiredPresence => 0,
            Self::Ops => 1,
        }
    }

    const fn cap(self) -> usize {
        match self {
            Self::RetiredPresence => MAX_SEALED_PRESENCE_FRAME_BYTES,
            Self::Ops => MAX_SEALED_OPS_FRAME_BYTES,
        }
    }
}

/// Why a live frame was not sealed or opened.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LiveSealError {
    /// The frame names no record, so no channel's keys may seal it.
    #[error("the live frame names no record")]
    NoRecord,
    /// The frame or its sealed form violates the wire format.
    #[error(transparent)]
    Wire(#[from] LiveWireError),
    /// Sealing or opening failed: no key for the channel, or a payload that
    /// does not authenticate.
    #[error("the live frame could not be sealed or opened: {0}")]
    Crypto(String),
    /// The opened frame disagrees with the tags it was sealed under.
    #[error("the sealed live frame's tags do not match its content")]
    Mismatch,
}

/// A live frame sealed with the content keys of the channel of the record it
/// names, the only form a live frame travels in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SealedLiveFrame {
    /// Presence or ordered frame.
    pub kind: LiveKind,
    /// The presence sequence, or the preview's per-record sequence (0 when
    /// previews end).
    pub seq: u64,
    /// [`LiveRecord::digest`] of the record whose channel seals the frame.
    pub record: [u8; 32],
    /// The record's channel.
    pub channel: [u8; 16],
    /// The channel content-key epoch the frame is sealed under.
    pub epoch: u64,
    /// The sealed canonical [`LiveFrame`] encoding.
    #[serde(with = "serde_bytes")]
    pub payload: Vec<u8>,
}

impl LiveFrame {
    /// The kind, sequence and record a sealed form names in the clear, or
    /// `None` when the frame names no record.
    pub fn seal_tags(&self) -> Option<(LiveKind, u64, &LiveRecord)> {
        match self {
            Self::RetiredPresence => None,
            Self::Ops(OpsFrame::Preview { seq, record, .. }) => Some((LiveKind::Ops, *seq, record)),
            Self::Ops(OpsFrame::End { record }) => Some((LiveKind::Ops, 0, record)),
            Self::Ops(OpsFrame::Semantic { record, event }) => {
                Some((LiveKind::Ops, event.seq, record))
            }
        }
    }
}

impl SealedLiveFrame {
    /// Open only for an explicitly observed record and its canonical channel.
    /// Holding a different channel key never authorizes a claim about this
    /// record. Publisher identity and current membership remain the caller's
    /// responsibility; decryption alone does not authenticate semantic work.
    ///
    /// # Errors
    ///
    /// Refuses a different record digest or channel before opening, and any
    /// authentication or canonical-frame failure from [`Self::open`].
    pub fn open_in_record<E: std::fmt::Display>(
        &self,
        workspace: &str,
        record: &LiveRecord,
        open: impl FnOnce(ArtifactId, &EncryptionMetadata, &[u8]) -> std::result::Result<Vec<u8>, E>,
    ) -> std::result::Result<LiveFrame, LiveSealError> {
        if self.record != record.digest()
            || self.channel_id()
                != collab_types::document_channel_id(&record.app, workspace, &record.record)
        {
            return Err(LiveSealError::Mismatch);
        }
        self.open(open)
    }

    pub fn channel_id(&self) -> ChannelId {
        ChannelId(uuid::Uuid::from_bytes(self.channel))
    }

    fn artifact(&self) -> ArtifactId {
        live_artifact_id(&self.record, self.kind.tag(), self.seq)
    }

    /// Seal `frame` with `seal`, the content cipher of the channel of the
    /// record the frame names, and encode the sealed form.
    ///
    /// # Errors
    ///
    /// [`LiveSealError::NoRecord`] for a frame naming no record, and wire or
    /// sealing failures.
    pub fn seal<E: std::fmt::Display>(
        frame: &LiveFrame,
        seal: impl FnOnce(
            ArtifactId,
            ContentRetention,
            &[u8],
        ) -> std::result::Result<(EncryptionMetadata, Vec<u8>), E>,
    ) -> std::result::Result<Vec<u8>, LiveSealError> {
        let (kind, seq, record) = frame.seal_tags().ok_or(LiveSealError::NoRecord)?;
        let cleartext = frame.encode()?;
        let record = record.digest();
        let (metadata, payload) = seal(
            live_artifact_id(&record, kind.tag(), seq),
            ContentRetention::Delta,
            &cleartext,
        )
        .map_err(|error| LiveSealError::Crypto(error.to_string()))?;
        if metadata.scheme
            != (EncryptionScheme::ContentKey {
                retention: ContentRetention::Delta,
            })
        {
            return Err(LiveSealError::Crypto(
                "live frames are sealed with channel content keys only".into(),
            ));
        }
        Ok(Self {
            kind,
            seq,
            record,
            channel: metadata.channel.0.into_bytes(),
            epoch: metadata.epoch,
            payload,
        }
        .encode()?)
    }

    /// Open the frame with `open`, the content cipher of
    /// [`Self::channel_id`], and check that it is the frame its tags name.
    ///
    /// # Errors
    ///
    /// Opening failures, a malformed frame, and tags that disagree with it.
    pub fn open<E: std::fmt::Display>(
        &self,
        open: impl FnOnce(ArtifactId, &EncryptionMetadata, &[u8]) -> std::result::Result<Vec<u8>, E>,
    ) -> std::result::Result<LiveFrame, LiveSealError> {
        let metadata =
            EncryptionMetadata::content_key(self.epoch, self.channel_id(), ContentRetention::Delta);
        let cleartext = open(self.artifact(), &metadata, &self.payload)
            .map_err(|error| LiveSealError::Crypto(error.to_string()))?;
        let frame = LiveFrame::decode(&cleartext)?;
        match frame.seal_tags() {
            Some((kind, seq, record))
                if kind == self.kind && seq == self.seq && record.digest() == self.record =>
            {
                Ok(frame)
            }
            _ => Err(LiveSealError::Mismatch),
        }
    }

    /// Canonical encoding within the kind's bound.
    pub fn encode(&self) -> Result<Vec<u8>> {
        if self.payload.is_empty() {
            return Err(LiveWireError::Invalid);
        }
        let bytes = postcard::to_allocvec(self).map_err(|_| LiveWireError::Codec)?;
        if bytes.len() > self.kind.cap() {
            return Err(LiveWireError::Codec);
        }
        Ok(bytes)
    }

    /// Decode, rejecting oversize, noncanonical and unsealed input. A
    /// cleartext [`LiveFrame`] never decodes as a sealed one.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty() || bytes.len() > MAX_SEALED_OPS_FRAME_BYTES {
            return Err(LiveWireError::Codec);
        }
        let value: Self = postcard::from_bytes(bytes).map_err(|_| LiveWireError::Codec)?;
        if value.encode()? != bytes {
            return Err(LiveWireError::Codec);
        }
        Ok(value)
    }
}

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

fn record128<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<String, D::Error> {
    let value = String::deserialize(d)?;
    if value.len() > 128 {
        Err(de::Error::custom("record exceeds bounds"))
    } else {
        Ok(value)
    }
}

fn preview_text<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<String, D::Error> {
    bounded_text::<D, MAX_PREVIEW_TEXT_BYTES>(d)
}

mod object_bytes {
    use super::{Deserialize, Deserializer, MAX_OBJECT_ID_BYTES, Serialize, de};

    pub fn serialize<S: serde::Serializer>(
        value: &[u8],
        s: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serde_bytes::Bytes::new(value).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<Vec<u8>, D::Error> {
        let value = serde_bytes::ByteBuf::deserialize(d)?;
        if value.is_empty() || value.len() > MAX_OBJECT_ID_BYTES {
            return Err(de::Error::custom("object identifier exceeds bounds"));
        }
        Ok(value.into_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> LiveRecord {
        LiveRecord {
            app: "so.tilo.notes".into(),
            record: "r1".into(),
        }
    }

    fn channel() -> ChannelId {
        ChannelId(uuid::Uuid::from_bytes([7; 16]))
    }

    fn semantic() -> LiveFrame {
        LiveFrame::Ops(OpsFrame::Semantic {
            record: record(),
            event: SemanticFrame {
                version: 1,
                seq: 42,
                sent_at_ms: 123,
                execution: uuid::Uuid::from_bytes([1; 16]),
                peer: 7,
                interaction_json: "{}".into(),
                on_behalf: None,
            },
        })
    }

    #[test]
    fn semantic_events_are_versioned_bounded_record_ops_not_pointers() {
        let frame = semantic();
        assert_eq!(frame.seal_tags(), Some((LiveKind::Ops, 42, &record())));
        assert_eq!(LiveFrame::decode(&frame.encode().unwrap()).unwrap(), frame);
        let bytes = SealedLiveFrame::seal(&frame, seal).unwrap();
        assert_eq!(
            SealedLiveFrame::decode(&bytes).unwrap().open(open).unwrap(),
            frame
        );
        let LiveFrame::Ops(OpsFrame::Semantic { mut event, .. }) = frame else {
            panic!("semantic fixture");
        };
        event.version = 2;
        assert!(event.validate().is_err());
        event.version = 1;
        event.execution = uuid::Uuid::nil();
        assert!(event.validate().is_err());
        event.execution = uuid::Uuid::from_bytes([1; 16]);
        event.interaction_json = "null".into();
        assert!(event.validate().is_err());
        event.interaction_json = " ".repeat(MAX_SEMANTIC_INTERACTION_BYTES + 1);
        assert!(event.validate().is_err());
        assert_eq!(
            LiveFrame::Ops(OpsFrame::End { record: record() })
                .encode()
                .unwrap(),
            b"\x01\x01\x0dso.tilo.notes\x02r1"
        );
    }

    #[test]
    fn opening_for_a_mounted_record_refuses_a_different_record_or_channel() {
        let expected = record();
        let workspace = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        let channel = collab_types::document_channel_id(&expected.app, workspace, &expected.record);
        let bytes = SealedLiveFrame::seal(&semantic(), |_, retention, cleartext: &[u8]| {
            Ok::<_, String>((
                EncryptionMetadata::content_key(3, channel, retention),
                cleartext.to_vec(),
            ))
        })
        .unwrap();
        let sealed = SealedLiveFrame::decode(&bytes).unwrap();
        let opened = sealed.open_in_record(workspace, &expected, |_, _, bytes| {
            Ok::<_, String>(bytes.to_vec())
        });
        assert_eq!(opened.unwrap(), semantic());
        let other = LiveRecord {
            record: "other".into(),
            ..expected.clone()
        };
        for (workspace, record) in [(workspace, other), ("another-room", expected.clone())] {
            assert!(matches!(
                sealed.open_in_record(
                    workspace,
                    &record,
                    |_, _, _| -> std::result::Result<Vec<u8>, String> {
                        panic!("a different mounted audience must be refused before opening")
                    }
                ),
                Err(LiveSealError::Mismatch)
            ));
        }
        // A valid record claim sealed under another record's held key is not
        // authority for this record, even when its clear digest agrees.
        let mut wrong_channel = sealed;
        wrong_channel.channel = [9; 16];
        assert!(
            wrong_channel
                .open_in_record(workspace, &expected, |_, _, _| {
                    Err::<Vec<u8>, String>("must not open".into())
                })
                .is_err()
        );
    }

    /// A stand-in content cipher that authenticates the artifact it was
    /// sealed for, as the channel's content keys do. Fallible like them.
    #[allow(
        clippy::unnecessary_wraps,
        reason = "it stands in for a cipher whose sealing can fail"
    )]
    fn seal(
        artifact: ArtifactId,
        retention: ContentRetention,
        cleartext: &[u8],
    ) -> std::result::Result<(EncryptionMetadata, Vec<u8>), String> {
        let mut payload = artifact.0.as_bytes().to_vec();
        payload.extend(cleartext.iter().map(|byte| byte ^ 0x5a));
        Ok((
            EncryptionMetadata::content_key(3, channel(), retention),
            payload,
        ))
    }

    fn open(
        artifact: ArtifactId,
        metadata: &EncryptionMetadata,
        payload: &[u8],
    ) -> std::result::Result<Vec<u8>, String> {
        let (bound, sealed) = payload.split_at_checked(16).ok_or("short")?;
        if bound != artifact.0.as_bytes() || metadata.channel != channel() || metadata.epoch != 3 {
            return Err("does not authenticate".into());
        }
        Ok(sealed.iter().map(|byte| byte ^ 0x5a).collect())
    }

    /// A frame that names [`record`], the way a preview does.
    fn named() -> LiveFrame {
        LiveFrame::Ops(OpsFrame::End { record: record() })
    }

    #[test]
    fn a_live_frame_is_sealed_and_bound_to_its_exact_record() {
        let primary = LiveRecord {
            app: "so.tilo.chat".into(),
            record: String::new(),
        };
        let workspace = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
        let channel = collab_types::document_channel_id(&primary.app, workspace, &primary.record);
        let frame = LiveFrame::Ops(OpsFrame::Preview {
            seq: 1,
            record: primary.clone(),
            ops: vec![PreviewOp::Insert {
                at: LiveAnchor {
                    cursor: vec![ANCHOR_TAG_SCOPED, 4, 0],
                },
                text: "hi".into(),
            }],
        });
        let bytes = SealedLiveFrame::seal(&frame, |_, retention, cleartext: &[u8]| {
            Ok::<_, String>((
                EncryptionMetadata::content_key(3, channel, retention),
                cleartext.to_vec(),
            ))
        })
        .unwrap();
        let sealed = SealedLiveFrame::decode(&bytes).unwrap();
        assert_eq!(
            sealed
                .open_in_record(workspace, &primary, |_, _, bytes| {
                    Ok::<_, String>(bytes.to_vec())
                })
                .unwrap(),
            frame
        );
        let named = LiveRecord {
            record: "other".into(),
            ..primary
        };
        assert!(matches!(
            sealed.open_in_record(
                workspace,
                &named,
                |_, _, _| -> std::result::Result<Vec<u8>, String> {
                    panic!("a different record must be refused before opening")
                }
            ),
            Err(LiveSealError::Mismatch)
        ));
        assert!(
            LiveRecord {
                app: String::new(),
                record: String::new()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn a_live_frame_travels_sealed_and_opens_only_under_its_tags() {
        let frame = named();
        let cleartext = frame.encode().unwrap();
        let bytes = SealedLiveFrame::seal(&frame, seal).unwrap();
        let sealed = SealedLiveFrame::decode(&bytes).unwrap();
        assert_eq!(
            (sealed.kind, sealed.seq, sealed.record, sealed.channel_id()),
            (LiveKind::Ops, 0, record().digest(), channel())
        );
        assert!(
            !bytes
                .windows(cleartext.len())
                .any(|window| window == cleartext),
            "the frame's content is never in the clear"
        );
        assert_eq!(sealed.open(open).unwrap(), frame);

        // Tags are bound into the seal: another sequence, record or kind
        // does not open.
        for tampered in [
            SealedLiveFrame {
                seq: 10,
                ..sealed.clone()
            },
            SealedLiveFrame {
                record: [1; 32],
                ..sealed.clone()
            },
            SealedLiveFrame {
                kind: LiveKind::RetiredPresence,
                ..sealed
            },
        ] {
            assert!(tampered.open(open).is_err());
        }
    }

    #[test]
    fn a_live_frame_without_a_record_or_cipher_is_never_sealed() {
        // A presence frame is retired: it names no record, so it is dropped.
        assert_eq!(
            SealedLiveFrame::seal(&LiveFrame::RetiredPresence, seal),
            Err(LiveSealError::NoRecord)
        );
        let refused = SealedLiveFrame::seal(&named(), |_, _, _| {
            Err::<(EncryptionMetadata, Vec<u8>), _>("no key")
        });
        assert!(matches!(refused, Err(LiveSealError::Crypto(_))));
        let mls = SealedLiveFrame::seal(&named(), |_, _, cleartext: &[u8]| {
            Ok::<_, String>((
                EncryptionMetadata::mls_application(1, channel()),
                cleartext.to_vec(),
            ))
        });
        assert!(matches!(mls, Err(LiveSealError::Crypto(_))));
    }

    #[test]
    fn a_cleartext_live_frame_is_not_a_sealed_one() {
        assert!(SealedLiveFrame::decode(&named().encode().unwrap()).is_err());
    }

    #[test]
    fn frames_round_trip_canonically() {
        for frame in [
            named(),
            LiveFrame::Ops(OpsFrame::Preview {
                seq: 1,
                record: record(),
                ops: vec![PreviewOp::Insert {
                    at: LiveAnchor { cursor: vec![1] },
                    text: "hi".into(),
                }],
            }),
            LiveFrame::RetiredPresence,
        ] {
            let bytes = frame.encode().unwrap();
            assert_eq!(LiveFrame::decode(&bytes).unwrap(), frame);
        }
    }

    #[test]
    fn out_of_bounds_frames_are_refused() {
        let empty = LiveFrame::Ops(OpsFrame::Preview {
            seq: 1,
            record: record(),
            ops: Vec::new(),
        });
        assert_eq!(empty.encode(), Err(LiveWireError::Invalid));
        let mut trailing = named().encode().unwrap();
        trailing.push(0);
        assert_eq!(LiveFrame::decode(&trailing), Err(LiveWireError::Codec));
        assert_eq!(LiveFrame::decode(&[]), Err(LiveWireError::Codec));
    }
}
