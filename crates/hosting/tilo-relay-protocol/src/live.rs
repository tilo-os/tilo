//! Live plane: bounded, inert values for ephemeral multiplayer state —
//! presence (focus, pointer, selection anchors, drags) and ordered text
//! previews — carried in [`Envelope::Live`](crate::Envelope::Live).
//!
//! Live frames are never durable and never authority. Documents change only
//! through signed durable deltas; a preview or drag is an overlay a receiver
//! drops once the record's durable change lands. The host validates every
//! frame with this codec, rate-limits each publisher, stamps the sender from
//! the authenticated connection and never stores a frame.
//!
//! The vocabulary is `LIVE_V1` from the qOS runtime (PR 541) without its
//! `MoQ` carrier, grants and early-forwarded durable changes.

use serde::{Deserialize, Deserializer, Serialize, de};

/// Largest encoded presence frame. Kept at datagram size so presence can
/// move to QUIC datagrams without a format change.
pub const MAX_PRESENCE_FRAME_BYTES: usize = 1100;
/// Largest encoded ordered frame.
pub const MAX_OPS_FRAME_BYTES: usize = 64 * 1024;
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
