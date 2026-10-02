//! Live plane: bounded, inert values for ephemeral multiplayer state —
//! presence (focus, pointer, selection anchors, drags) and ordered text
//! previews — carried in [`Envelope::Live`](crate::Envelope::Live).
//!
//! Live frames are never durable and never authority. Documents change only
//! through signed durable deltas; a preview or drag is an overlay a receiver
//! drops once the record's durable change lands.
//!
//! A live frame travels only as a [`SealedLiveFrame`]: the frame sealed with
//! the content keys of the channel of the record it names. Its kind,
//! sequence and record digest are in the clear so the host can rate-limit,
//! order latest-value presence and route a record's previews; the content
//! never is. A frame that names no record has no channel and is never sent.
//! The host checks the sealed form with this codec, rate-limits each
//! publisher, stamps the sender from the authenticated connection, forwards
//! the bytes unchanged and never stores a frame.
//!
//! The vocabulary is `LIVE_V1` from the qOS runtime (PR 541) without its
//! `MoQ` carrier, grants and early-forwarded durable changes.

use collab_types::{
    ArtifactId, ChannelId, ContentRetention, EncryptionMetadata, EncryptionScheme, live_artifact_id,
};
use serde::{Deserialize, Deserializer, Serialize, de};

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
/// Presence frames per publisher per second the host forwards.
pub const MAX_PRESENCE_RATE: u32 = 120;
/// Ordered frames per publisher per second the host forwards.
pub const MAX_OPS_RATE: u32 = 240;
/// Text preview payload ceiling per operation.
pub const MAX_PREVIEW_TEXT_BYTES: usize = 1024;
/// Text preview operations per frame.
pub const MAX_PREVIEW_OPS: usize = 32;
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

fn finite(values: &[f32]) -> Result<()> {
    if values.iter().all(|value| value.is_finite()) {
        Ok(())
    } else {
        Err(LiveWireError::Invalid)
    }
}

/// Record reference shared by every member: application and record id.
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

/// Object inside a record or on the desktop (window, shape, block).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveObject {
    /// Owning record, or `None` for desktop-level objects such as windows.
    pub record: Option<LiveRecord>,
    /// Application-defined identifier, 1..=[`MAX_OBJECT_ID_BYTES`] bytes.
    #[serde(with = "object_bytes")]
    pub id: Vec<u8>,
}

/// Text position as a stable, application-encoded CRDT anchor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveAnchor {
    /// Opaque cursor bytes, 1..=[`MAX_OBJECT_ID_BYTES`].
    #[serde(with = "object_bytes")]
    pub cursor: Vec<u8>,
}

/// Drag lifecycle; `End` is followed by the durable change.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DragPhase {
    /// Pointer captured.
    Start,
    /// Intermediate position.
    Move,
    /// Released; receivers keep the preview until the durable change or a
    /// timeout.
    End,
    /// Aborted; receivers restore the durable position.
    Cancel,
}

/// A live drag: object, phase, position and optional size.
pub type LiveDrag = (LiveObject, DragPhase, [f32; 2], Option<[f32; 2]>);

/// Largest participant label.
pub const MAX_PARTICIPANT_LABEL_BYTES: usize = 64;
/// Largest participant activity line.
pub const MAX_PARTICIPANT_ACTIVITY_BYTES: usize = 96;

/// What kind of participant publishes a presence frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ParticipantKind {
    /// A person's device (a browser tab, a desktop session).
    Person,
    /// A delegated agent acting under a person's root.
    Agent,
}

/// How a publisher asks to be drawn: kind, label, cursor colour and what it
/// is doing now.
///
/// Self-asserted and cosmetic. Who a publisher is comes from the host-stamped
/// device; that a device is an agent, and on whose behalf, comes from its
/// root-signed [`AgentDelegation`](crate::agents::AgentDelegation).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LiveParticipant {
    /// Person or agent.
    pub kind: ParticipantKind,
    /// Display name, 1..=[`MAX_PARTICIPANT_LABEL_BYTES`] bytes.
    pub label: String,
    /// Cursor colour, sRGB.
    pub color: [u8; 3],
    /// Short activity line ("Editing Note"), at most
    /// [`MAX_PARTICIPANT_ACTIVITY_BYTES`] bytes.
    pub activity: Option<String>,
}

impl LiveParticipant {
    fn validate(&self) -> Result<()> {
        if self.label.is_empty()
            || self.label.len() > MAX_PARTICIPANT_LABEL_BYTES
            || self
                .activity
                .as_ref()
                .is_some_and(|activity| activity.len() > MAX_PARTICIPANT_ACTIVITY_BYTES)
        {
            Err(LiveWireError::Invalid)
        } else {
            Ok(())
        }
    }
}

/// Latest-value presence of one publisher.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresenceFrame {
    /// Monotonic per-publisher sequence; receivers drop older frames.
    pub seq: u64,
    /// Publisher wall clock, for interpolation only (never ordering).
    pub sent_at_ms: u64,
    /// Record the publisher is focused on.
    pub focus: Option<LiveRecord>,
    /// Pointer in the focused surface's coordinate space.
    pub pointer: Option<[f32; 2]>,
    /// Text selection as (anchor, head).
    pub selection: Option<(LiveAnchor, LiveAnchor)>,
    /// Object being dragged or resized.
    pub drag: Option<LiveDrag>,
    /// How the publisher asks to be drawn.
    pub participant: Option<LiveParticipant>,
}

impl PresenceFrame {
    fn validate(&self) -> Result<()> {
        if let Some(participant) = &self.participant {
            participant.validate()?;
        }
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
                if size.iter().any(|value| *value < 0.0) {
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
        /// Record the preview applies to.
        record: LiveRecord,
        /// 1..=[`MAX_PREVIEW_OPS`] operations.
        ops: Vec<PreviewOp>,
    },
    /// The publisher's previews for `record` are over.
    End {
        /// Record whose previews end.
        record: LiveRecord,
    },
}

impl OpsFrame {
    /// The record this frame concerns.
    pub fn record(&self) -> &LiveRecord {
        match self {
            Self::Preview { record, .. } | Self::End { record } => record,
        }
    }

    fn validate(&self) -> Result<()> {
        self.record().validate()?;
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

/// One live frame, the payload of [`Envelope::Live`](crate::Envelope::Live).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum LiveFrame {
    /// Latest-value presence; a receiver keeps only the newest per publisher.
    Presence(PresenceFrame),
    /// Ordered per-record frame.
    Ops(OpsFrame),
}

impl LiveFrame {
    fn validate(&self) -> Result<()> {
        match self {
            Self::Presence(frame) => frame.validate(),
            Self::Ops(frame) => frame.validate(),
        }
    }

    fn cap(&self) -> usize {
        match self {
            Self::Presence(_) => MAX_PRESENCE_FRAME_BYTES,
            Self::Ops(_) => MAX_OPS_FRAME_BYTES,
        }
    }

    /// Canonical encoding after checking every bound.
    ///
    /// # Errors
    ///
    /// Returns an error when the value violates its bounds.
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let bytes = postcard::to_allocvec(self).map_err(|_| LiveWireError::Codec)?;
        if bytes.len() > self.cap() {
            return Err(LiveWireError::Codec);
        }
        Ok(bytes)
    }

    /// Decode, rejecting oversize and noncanonical input.
    ///
    /// # Errors
    ///
    /// Returns an error when `bytes` is not a canonical encoding within
    /// bounds.
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
    /// Latest-value presence.
    Presence,
    /// An ordered per-record frame.
    Ops,
}

impl LiveKind {
    const fn tag(self) -> u8 {
        match self {
            Self::Presence => 0,
            Self::Ops => 1,
        }
    }

    const fn cap(self) -> usize {
        match self {
            Self::Presence => MAX_SEALED_PRESENCE_FRAME_BYTES,
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
            Self::Presence(presence) => presence
                .focus
                .as_ref()
                .map(|record| (LiveKind::Presence, presence.seq, record)),
            Self::Ops(OpsFrame::Preview { seq, record, .. }) => Some((LiveKind::Ops, *seq, record)),
            Self::Ops(OpsFrame::End { record }) => Some((LiveKind::Ops, 0, record)),
        }
    }
}

impl SealedLiveFrame {
    /// The channel that sealed the frame.
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
    ///
    /// # Errors
    ///
    /// Returns an error when the encoding exceeds the bound.
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
    ///
    /// # Errors
    ///
    /// Returns an error when `bytes` is not a canonical sealed frame within
    /// bounds.
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

    fn focused(seq: u64) -> LiveFrame {
        LiveFrame::Presence(PresenceFrame {
            seq,
            sent_at_ms: 1,
            focus: Some(record()),
            pointer: Some([1.0, 2.0]),
            selection: None,
            drag: None,
            participant: None,
        })
    }

    #[test]
    fn a_live_frame_travels_sealed_and_opens_only_under_its_tags() {
        let frame = focused(9);
        let cleartext = frame.encode().unwrap();
        let bytes = SealedLiveFrame::seal(&frame, seal).unwrap();
        let sealed = SealedLiveFrame::decode(&bytes).unwrap();
        assert_eq!(
            (sealed.kind, sealed.seq, sealed.record, sealed.channel_id()),
            (LiveKind::Presence, 9, record().digest(), channel())
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
                kind: LiveKind::Ops,
                ..sealed
            },
        ] {
            assert!(tampered.open(open).is_err());
        }

        let preview = LiveFrame::Ops(OpsFrame::End { record: record() });
        let sealed =
            SealedLiveFrame::decode(&SealedLiveFrame::seal(&preview, seal).unwrap()).unwrap();
        assert_eq!((sealed.kind, sealed.seq), (LiveKind::Ops, 0));
        assert_eq!(sealed.open(open).unwrap(), preview);
    }

    #[test]
    fn a_live_frame_without_a_record_or_cipher_is_never_sealed() {
        let unfocused = PresenceFrame {
            seq: 1,
            sent_at_ms: 1,
            focus: None,
            pointer: Some([1.0, 2.0]),
            selection: None,
            drag: None,
            participant: None,
        };
        assert_eq!(
            SealedLiveFrame::seal(&LiveFrame::Presence(unfocused), seal),
            Err(LiveSealError::NoRecord)
        );
        let refused = SealedLiveFrame::seal(&focused(1), |_, _, _| {
            Err::<(EncryptionMetadata, Vec<u8>), _>("no key")
        });
        assert!(matches!(refused, Err(LiveSealError::Crypto(_))));
        let mls = SealedLiveFrame::seal(&focused(1), |_, _, cleartext: &[u8]| {
            Ok::<_, String>((
                EncryptionMetadata::mls_application(1, channel()),
                cleartext.to_vec(),
            ))
        });
        assert!(matches!(mls, Err(LiveSealError::Crypto(_))));
    }

    #[test]
    fn a_cleartext_live_frame_is_not_a_sealed_one() {
        for frame in [
            focused(1),
            LiveFrame::Ops(OpsFrame::End { record: record() }),
        ] {
            assert!(SealedLiveFrame::decode(&frame.encode().unwrap()).is_err());
        }
    }

    #[test]
    fn frames_round_trip_canonically() {
        let presence = LiveFrame::Presence(PresenceFrame {
            seq: 3,
            sent_at_ms: 10,
            focus: Some(record()),
            pointer: Some([1.5, 2.0]),
            selection: Some((
                LiveAnchor {
                    cursor: vec![ANCHOR_TAG_SCOPED, 4, 0],
                },
                LiveAnchor {
                    cursor: vec![ANCHOR_TAG_SAME_SCOPE, 6],
                },
            )),
            drag: None,
            participant: Some(LiveParticipant {
                kind: ParticipantKind::Agent,
                label: "Writer helper".into(),
                color: [0x7c, 0x3a, 0xed],
                activity: Some("Editing Note".into()),
            }),
        });
        let preview = LiveFrame::Ops(OpsFrame::Preview {
            seq: 1,
            record: record(),
            ops: vec![PreviewOp::Insert {
                at: LiveAnchor { cursor: vec![1] },
                text: "hi".into(),
            }],
        });
        for frame in [
            presence,
            preview,
            LiveFrame::Ops(OpsFrame::End { record: record() }),
        ] {
            let bytes = frame.encode().unwrap();
            assert_eq!(LiveFrame::decode(&bytes).unwrap(), frame);
        }
    }

    #[test]
    fn out_of_bounds_frames_are_refused() {
        let nan = LiveFrame::Presence(PresenceFrame {
            seq: 1,
            sent_at_ms: 0,
            focus: None,
            pointer: Some([f32::NAN, 0.0]),
            selection: None,
            drag: None,
            participant: None,
        });
        assert_eq!(nan.encode(), Err(LiveWireError::Invalid));
        let unnamed = LiveFrame::Presence(PresenceFrame {
            seq: 1,
            sent_at_ms: 0,
            focus: None,
            pointer: None,
            selection: None,
            drag: None,
            participant: Some(LiveParticipant {
                kind: ParticipantKind::Agent,
                label: String::new(),
                color: [0; 3],
                activity: None,
            }),
        });
        assert_eq!(unnamed.encode(), Err(LiveWireError::Invalid));
        let empty = LiveFrame::Ops(OpsFrame::Preview {
            seq: 1,
            record: record(),
            ops: Vec::new(),
        });
        assert_eq!(empty.encode(), Err(LiveWireError::Invalid));
        let mut trailing = LiveFrame::Ops(OpsFrame::End { record: record() })
            .encode()
            .unwrap();
        trailing.push(0);
        assert_eq!(LiveFrame::decode(&trailing), Err(LiveWireError::Codec));
        assert_eq!(LiveFrame::decode(&[]), Err(LiveWireError::Codec));
    }
}
