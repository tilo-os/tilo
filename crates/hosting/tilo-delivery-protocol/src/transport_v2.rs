//! The relay stream's frames: every value a device and its relay exchange
//! after the certified handshake, on the relay ALPN
//! (`tilo_relay_protocol::transport::ALPN`). There is one framing, at the
//! peer protocol version the handshake names
//! (docs/architecture/compatibility.md): state is [`EnvelopeV2::State`],
//! except that a zero-signature delta travels as
//! [`EnvelopeV2::UnsignedDelta`] or coded against the session's previous
//! delta from the same author ([`EnvelopeV2::CodedDelta`]), and forwarded
//! presence arrives batched and slot-coded ([`EnvelopeV2::PresenceBatch`]).
//! [`SessionReader`] turns all three back into `State` values, so nothing
//! past the session boundary sees them.
#![allow(
    missing_docs,
    reason = "wire frame types, described as a whole by the module documentation above"
)]
use collab_types::WorkspaceId;
use serde::{Deserialize, Serialize};
use tilo_relay_client::frame::FrameError;
use tilo_relay_protocol::transport::{Envelope, MAX_FRAME_BYTES};

/// Discriminant of [`EnvelopeV2::UnsignedDelta`].
const UNSIGNED_DELTA_KIND: u8 = 11;
/// Discriminant of [`EnvelopeV2::PresenceBatch`].
const PRESENCE_BATCH_KIND: u8 = 12;
/// Discriminant of [`EnvelopeV2::CodedDelta`].
const CODED_DELTA_KIND: u8 = 13;

mod delta;
mod presence;
pub use delta::{
    DELTA_ROUTE_BYTES, DeltaEncoder, DeltaSegment, DeltaSlotKey, MAX_CODED_DELTA_BYTES,
    MAX_DELTA_SLOTS, encode_coded_delta,
};
pub use presence::{
    MAX_PRESENCE_SLOTS, MAX_SLOTTED_PRESENCE_BYTES, PresenceItem, encode_presence_batch,
};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum EnvelopeV2 {
    State(Envelope),
    /// Retired: a device opened the broker private lane in a workspace. Actions
    /// travel to another device on its own connection now; nothing sends
    /// this, and its place is kept so stored bytes still decode.
    RetiredEnableActions,
    /// Retired: the host's answer to [`Self::RetiredEnableActions`].
    RetiredActionsReady,
    /// Retired: an action a source device sent unsigned, before a delivery
    /// carried its signature.
    RetiredDirected,
    /// Retired: the host refused to deliver an action.
    RetiredUnavailable,
    /// Retired: [`Self::RetiredDirected`] with the source user the relay
    /// attested.
    RetiredDelivered,
    /// Retired: a lane spoke the broker's protocol extensions.
    RetiredEnableBrokerClient,
    /// Retired: a lane named the durable broker host of its workspace.
    RetiredRegisterBroker,
    /// Retired: [`Self::RetiredDelivered`] carrying the source's certificate.
    RetiredCertifiedDelivered,
    /// Retired: an action its source device signed.
    RetiredSignedDirected,
    /// Retired: [`Self::RetiredSignedDirected`] with the signing device's
    /// certificate.
    RetiredCertifiedSignedDelivered,
    /// `State(Envelope::Delta { signature: [0; 64], .. })` without the
    /// signature. [`SessionReader`] returns it as that `State` value, so
    /// nothing past the session boundary sees this variant.
    UnsignedDelta {
        workspace_id: WorkspaceId,
        author_device_public_key: [u8; 32],
        authored_at_ns: u64,
        #[serde(with = "serde_bytes")]
        delta: Vec<u8>,
    },
    /// Presence payloads for one workspace, each coded against the session's
    /// presence slots ([`PresenceItem`]). [`SessionReader`] returns one
    /// `State(Envelope::RetiredPresence)` per item, in order.
    PresenceBatch {
        workspace_id: WorkspaceId,
        items: Vec<PresenceItem>,
    },
    /// An [`Self::UnsignedDelta`] payload coded against the session's delta
    /// slot `slot` ([`DeltaSegment`]). [`SessionReader`] rebuilds it and
    /// returns it as `State(Envelope::Delta)`.
    CodedDelta {
        slot: u16,
        segments: Vec<DeltaSegment>,
    },
}

impl EnvelopeV2 {
    /// Whether this frame is one of the retired private-lane frames: a
    /// device's action lane went with the host that routed it, so a peer
    /// sending one speaks a protocol this build does not serve.
    pub const fn is_retired(&self) -> bool {
        matches!(
            self,
            Self::RetiredEnableActions
                | Self::RetiredActionsReady
                | Self::RetiredDirected
                | Self::RetiredUnavailable
                | Self::RetiredDelivered
                | Self::RetiredEnableBrokerClient
                | Self::RetiredRegisterBroker
                | Self::RetiredCertifiedDelivered
                | Self::RetiredSignedDirected
                | Self::RetiredCertifiedSignedDelivered
        )
    }
}

/// # Errors
///
/// Returns `postcard::Error` if the value violates its wire bounds or cannot be serialized.
pub fn encode(envelope: &EnvelopeV2) -> Result<Vec<u8>, postcard::Error> {
    let payload = postcard::to_allocvec(envelope)?;
    if payload.len() > MAX_FRAME_BYTES + 1 {
        return Err(postcard::Error::SerializeBufferFull);
    }
    let len = u32::try_from(payload.len()).map_err(|_| postcard::Error::SerializeBufferFull)?;
    let mut out = len.to_le_bytes().to_vec();
    out.extend_from_slice(&payload);
    Ok(out)
}

/// # Errors
///
/// Returns `postcard::Error` if the input is not a canonical encoding within this type's bounds.
pub fn decode(payload: &[u8]) -> Result<EnvelopeV2, postcard::Error> {
    if payload.is_empty() || payload.len() > MAX_FRAME_BYTES + 1 {
        return Err(postcard::Error::DeserializeBadEncoding);
    }
    let (value, rest) = postcard::take_from_bytes(payload)?;
    if !rest.is_empty() {
        return Err(postcard::Error::DeserializeBadEncoding);
    }
    Ok(value)
}

/// Stateful framing survives cancellation by a concurrent writer/access audit.
pub struct SessionReader<R> {
    inner: R,
    prefix: [u8; 4],
    prefix_read: usize,
    payload: Vec<u8>,
    payload_read: usize,
    presence: presence::PresenceSlots,
    /// The last delta frame this session received in each delta slot.
    deltas: delta::DeltaSlots,
    /// The rest of the last [`EnvelopeV2::PresenceBatch`], returned before
    /// anything is read again. Taken without awaiting, so a cancelled read
    /// loses none of it.
    expanded: std::collections::VecDeque<EnvelopeV2>,
}
impl<R: tokio::io::AsyncRead + Unpin> SessionReader<R> {
    /// A reader of the relay stream `inner`, after its handshake.
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            prefix: [0; 4],
            prefix_read: 0,
            payload: Vec::new(),
            payload_read: 0,
            presence: presence::PresenceSlots::default(),
            deltas: delta::DeltaSlots::default(),
            expanded: std::collections::VecDeque::new(),
        }
    }
    /// The next complete envelope, or `None` at a clean end of stream.
    pub async fn read_next(&mut self) -> Result<Option<EnvelopeV2>, FrameError> {
        loop {
            if let Some(next) = self.expanded.pop_front() {
                return Ok(Some(next));
            }
            let Some(value) = self.read_frame().await? else {
                return Ok(None);
            };
            match value {
                EnvelopeV2::PresenceBatch {
                    workspace_id,
                    items,
                } => {
                    for item in items {
                        let payload = self.presence.expand(item)?;
                        self.expanded
                            .push_back(EnvelopeV2::State(Envelope::RetiredPresence {
                                workspace_id,
                                payload,
                            }));
                    }
                }
                value => return Ok(Some(value)),
            }
        }
    }
    /// The next frame as it was written: a [`EnvelopeV2::PresenceBatch`] is
    /// still whole.
    async fn read_frame(&mut self) -> Result<Option<EnvelopeV2>, FrameError> {
        use tokio::io::AsyncReadExt;
        while self.prefix_read < 4 {
            let n = self
                .inner
                .read(self.prefix.get_mut(self.prefix_read..).unwrap_or_default())
                .await?;
            if n == 0 {
                if self.prefix_read == 0 {
                    return Ok(None);
                }
                return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
            }
            self.prefix_read = self.prefix_read.saturating_add(n);
        }
        let len = u32::from_le_bytes(self.prefix) as usize;
        if len == 0 {
            return Err(FrameError::EmptyFrame);
        }
        if len > MAX_FRAME_BYTES.saturating_add(1) {
            return Err(FrameError::TooLarge(len));
        }
        if self.payload.is_empty() {
            self.payload.resize(1, 0);
        }
        if self.payload_read == 0 {
            if self
                .inner
                .read(self.payload.get_mut(..1).unwrap_or_default())
                .await?
                == 0
            {
                return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
            }
            self.payload_read = 1;
        }
        self.payload.resize(len, 0);
        while self.payload_read < len {
            let n = self
                .inner
                .read(
                    self.payload
                        .get_mut(self.payload_read..)
                        .unwrap_or_default(),
                )
                .await?;
            if n == 0 {
                return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
            }
            self.payload_read = self.payload_read.saturating_add(n);
        }
        let value = decode(&self.payload).map_err(FrameError::Decode)?;
        let value = match value {
            EnvelopeV2::CodedDelta { slot, segments } => {
                coded_delta(&self.deltas.expand(slot, &segments)?)?
            }
            EnvelopeV2::UnsignedDelta {
                workspace_id,
                author_device_public_key,
                authored_at_ns,
                delta,
            } => EnvelopeV2::State(Envelope::Delta {
                workspace_id,
                author_device_public_key,
                authored_at_ns,
                delta,
                signature: [0; 64],
            }),
            value => value,
        };
        self.prefix_read = 0;
        self.payload_read = 0;
        self.payload.clear();
        Ok(Some(value))
    }
}

/// Encode one state envelope as the relay stream carries it: a delta whose
/// signature is all zero becomes [`EnvelopeV2::UnsignedDelta`], every other
/// envelope [`EnvelopeV2::State`].
///
/// # Errors
///
/// Returns a postcard error if the envelope cannot be encoded within the frame bound.
pub fn encode_state(env: &Envelope) -> Result<Vec<u8>, postcard::Error> {
    let payload = match env {
        Envelope::Delta {
            workspace_id,
            author_device_public_key,
            authored_at_ns,
            delta,
            signature,
        } if *signature == [0; 64] => unsigned_delta_payload(
            *workspace_id,
            author_device_public_key,
            *authored_at_ns,
            delta,
        )?,
        _ => state_payload(env)?,
    };
    if payload.len() > MAX_FRAME_BYTES + 1 {
        return Err(postcard::Error::SerializeBufferFull);
    }
    let mut out = Vec::with_capacity(payload.len().saturating_add(4));
    let len = u32::try_from(payload.len()).map_err(|_| postcard::Error::SerializeBufferFull)?;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

/// Same zero-valued State discriminant as `EnvelopeV2`, borrowing its payload so
/// the replay allocation permit covers every live representation during writes.
///
/// # Errors
///
/// Returns a postcard error if the envelope cannot be encoded.
pub fn state_payload(env: &Envelope) -> Result<Vec<u8>, postcard::Error> {
    #[derive(serde::Serialize)]
    enum BorrowedState<'a> {
        State(&'a Envelope),
    }
    postcard::to_allocvec(&BorrowedState::State(env))
}

/// [`EnvelopeV2::UnsignedDelta`]'s payload, borrowing the delta.
fn unsigned_delta_payload(
    workspace_id: WorkspaceId,
    author_device_public_key: &[u8; 32],
    authored_at_ns: u64,
    delta: &[u8],
) -> Result<Vec<u8>, postcard::Error> {
    #[derive(serde::Serialize)]
    struct Fields<'a> {
        workspace_id: WorkspaceId,
        author_device_public_key: &'a [u8; 32],
        authored_at_ns: u64,
        #[serde(with = "serde_bytes")]
        delta: &'a [u8],
    }
    // Discriminant, three varints of at most 10 bytes, the key and the delta.
    let mut out = Vec::with_capacity(delta.len().saturating_add(64));
    out.push(UNSIGNED_DELTA_KIND);
    postcard::to_extend(
        &Fields {
            workspace_id,
            author_device_public_key,
            authored_at_ns,
            delta,
        },
        out,
    )
}

/// The state value of a rebuilt [`EnvelopeV2::CodedDelta`] payload, which
/// must be a whole [`EnvelopeV2::UnsignedDelta`].
fn coded_delta(payload: &[u8]) -> Result<EnvelopeV2, FrameError> {
    match decode(payload).map_err(FrameError::Decode)? {
        EnvelopeV2::UnsignedDelta {
            workspace_id,
            author_device_public_key,
            authored_at_ns,
            delta,
        } => Ok(EnvelopeV2::State(Envelope::Delta {
            workspace_id,
            author_device_public_key,
            authored_at_ns,
            delta,
            signature: [0; 64],
        })),
        _ => Err(FrameError::Decode(postcard::Error::DeserializeBadEncoding)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keystroke(signature: [u8; 64]) -> Envelope {
        Envelope::Delta {
            workspace_id: WorkspaceId::from_u128(300),
            author_device_public_key: [5; 32],
            authored_at_ns: 1_800_000_000_000_000_000,
            delta: vec![0xab; 425],
            signature,
        }
    }

    #[test]
    fn a_zero_signature_delta_travels_without_its_signature() {
        let unsigned = keystroke([0; 64]);
        let whole = encode(&EnvelopeV2::State(unsigned.clone())).unwrap();
        let compact = encode_state(&unsigned).unwrap();
        // The State and Delta discriminants become one, and the signature goes
        // with its length prefix.
        assert_eq!(whole.len().checked_sub(compact.len()), Some(66));
        assert_eq!(compact[4], UNSIGNED_DELTA_KIND);
        let owned = decode(&compact[4..]).unwrap();
        assert!(matches!(owned, EnvelopeV2::UnsignedDelta { .. }));
        assert_eq!(
            encode(&owned).unwrap(),
            compact,
            "the borrowed encoding is the variant's own encoding",
        );
        let signed = keystroke([7; 64]);
        assert_eq!(
            encode_state(&signed).unwrap(),
            encode(&EnvelopeV2::State(signed)).unwrap(),
        );
        assert_eq!(
            encode_state(&Envelope::Ping).unwrap(),
            encode(&EnvelopeV2::State(Envelope::Ping)).unwrap(),
        );
    }

    #[tokio::test]
    async fn the_reader_restores_the_state_value_of_an_unsigned_delta() {
        let unsigned = keystroke([0; 64]);
        let compact = encode_state(&unsigned).unwrap();
        let mut reader = SessionReader::new(std::io::Cursor::new(compact));
        assert_eq!(
            reader.read_next().await.unwrap(),
            Some(EnvelopeV2::State(unsigned.clone()))
        );
        let mut reader = SessionReader::new(std::io::Cursor::new(
            encode(&EnvelopeV2::State(unsigned.clone())).unwrap(),
        ));
        assert_eq!(
            reader.read_next().await.unwrap(),
            Some(EnvelopeV2::State(unsigned))
        );
    }

    #[tokio::test]
    async fn a_large_unsigned_delta_takes_the_state_bound() {
        let large = Envelope::Delta {
            workspace_id: WorkspaceId::from_u128(1),
            author_device_public_key: [1; 32],
            authored_at_ns: 1,
            delta: vec![0; 128 * 1024],
            signature: [0; 64],
        };
        let wire = encode_state(&large).unwrap();
        let mut reader = SessionReader::new(std::io::Cursor::new(wire));
        assert_eq!(
            reader.read_next().await.unwrap(),
            Some(EnvelopeV2::State(large))
        );
    }

    /// One whole-payload presence item, as this relay writes it.
    fn presence_item(payload: &[u8]) -> PresenceItem {
        PresenceItem {
            slot: None,
            keep_prefix: 0,
            keep_suffix: 0,
            middle: payload.to_vec(),
        }
    }

    #[tokio::test]
    async fn a_presence_batch_reads_as_one_presence_per_item_in_order() {
        let pointer = |x: u8| [&[5; 32][..], b"user key ", &[x, x], b" teal"].concat();
        let first = vec![presence_item(&pointer(1)), presence_item(b"heartbeat")];
        let second = vec![presence_item(&pointer(2))];
        let wire = [
            encode_presence_batch(WorkspaceId::from_u128(300), &first).unwrap(),
            encode_state(&Envelope::Ping).unwrap(),
            encode_presence_batch(WorkspaceId::from_u128(300), &second).unwrap(),
        ]
        .concat();
        let mut reader = SessionReader::new(std::io::Cursor::new(wire));
        let presence = |payload: &[u8]| {
            Some(EnvelopeV2::State(Envelope::RetiredPresence {
                workspace_id: WorkspaceId::from_u128(300),
                payload: payload.to_vec(),
            }))
        };
        assert_eq!(reader.read_next().await.unwrap(), presence(&pointer(1)));
        assert_eq!(reader.read_next().await.unwrap(), presence(b"heartbeat"));
        assert_eq!(
            reader.read_next().await.unwrap(),
            Some(EnvelopeV2::State(Envelope::Ping))
        );
        assert_eq!(reader.read_next().await.unwrap(), presence(&pointer(2)));
        assert_eq!(reader.read_next().await.unwrap(), None);
    }

    #[tokio::test]
    async fn coded_deltas_read_as_the_unsigned_delta() {
        let mut encoder = DeltaEncoder::default();
        let keystrokes: Vec<Envelope> = (0..5u8)
            .map(|n| Envelope::Delta {
                workspace_id: WorkspaceId::from_u128(300),
                author_device_public_key: [5; 32],
                authored_at_ns: 1_800_000_000_000_000_000 + u64::from(n),
                delta: [vec![0xab; 300], vec![n; 4], vec![0xcd; 100]].concat(),
                signature: [0; 64],
            })
            .collect();
        let mut wire = Vec::new();
        for (n, keystroke) in keystrokes.iter().enumerate() {
            let coded = encoder.encode(keystroke).unwrap().unwrap();
            assert_eq!(coded[4], CODED_DELTA_KIND);
            if n > 0 {
                let plain = encode_state(keystroke).unwrap();
                assert!(
                    coded.len() * 4 < plain.len(),
                    "{} vs {}",
                    coded.len(),
                    plain.len()
                );
            }
            wire.extend(coded);
            wire.extend(encode_state(&Envelope::Ping).unwrap());
        }
        let mut reader = SessionReader::new(std::io::Cursor::new(wire));
        for keystroke in &keystrokes {
            assert_eq!(
                reader.read_next().await.unwrap(),
                Some(EnvelopeV2::State(keystroke.clone()))
            );
            assert_eq!(
                reader.read_next().await.unwrap(),
                Some(EnvelopeV2::State(Envelope::Ping))
            );
        }
        assert_eq!(reader.read_next().await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_coded_delta_that_rebuilds_anything_but_an_unsigned_delta_fails_the_session() {
        let presence = state_payload(&Envelope::Ping).unwrap();
        for segments in [
            vec![DeltaSegment::Literal(presence)],
            vec![DeltaSegment::Copy { offset: 0, len: 4 }],
            // An unsigned-delta kind byte with nothing that decodes after it.
            vec![DeltaSegment::Literal(vec![UNSIGNED_DELTA_KIND, 0xff])],
        ] {
            let wire = encode_coded_delta(0, &segments).unwrap();
            let mut reader = SessionReader::new(std::io::Cursor::new(wire));
            assert!(matches!(
                reader.read_next().await,
                Err(FrameError::Decode(_))
            ));
        }
    }

    #[tokio::test]
    async fn a_presence_batch_that_does_not_fit_its_slots_fails_the_session() {
        let bad = PresenceItem {
            slot: Some(0),
            keep_prefix: 4,
            keep_suffix: 0,
            middle: Vec::new(),
        };
        let wire = encode_presence_batch(WorkspaceId::from_u128(1), &[bad]).unwrap();
        let mut reader = SessionReader::new(std::io::Cursor::new(wire));
        assert!(matches!(
            reader.read_next().await,
            Err(FrameError::Decode(_))
        ));
    }

    #[test]
    fn retired_private_frames_keep_their_places() {
        // The private lane's ten frames are gone; each keeps its
        // discriminant so a frame a peer of the old build sends still
        // decodes as the tombstone in its place.
        for (kind, frame) in [
            (1, EnvelopeV2::RetiredEnableActions),
            (2, EnvelopeV2::RetiredActionsReady),
            (3, EnvelopeV2::RetiredDirected),
            (4, EnvelopeV2::RetiredUnavailable),
            (5, EnvelopeV2::RetiredDelivered),
            (6, EnvelopeV2::RetiredEnableBrokerClient),
            (7, EnvelopeV2::RetiredRegisterBroker),
            (8, EnvelopeV2::RetiredCertifiedDelivered),
            (9, EnvelopeV2::RetiredSignedDirected),
            (10, EnvelopeV2::RetiredCertifiedSignedDelivered),
        ] {
            let payload = [kind];
            assert_eq!(
                encode(&frame).unwrap(),
                [vec![1, 0, 0, 0], payload.to_vec()].concat()
            );
            assert_eq!(decode(&payload).unwrap(), frame);
            assert!(frame.is_retired());
        }
        assert!(!EnvelopeV2::State(Envelope::Ping).is_retired());
        assert!(
            !EnvelopeV2::UnsignedDelta {
                workspace_id: WorkspaceId::from_u128(1),
                author_device_public_key: [0; 32],
                authored_at_ns: 1,
                delta: Vec::new(),
            }
            .is_retired()
        );
    }

    #[tokio::test]
    async fn a_frame_longer_than_the_bound_is_rejected_before_its_body() {
        let data = [
            u32::try_from(MAX_FRAME_BYTES + 2)
                .unwrap()
                .to_le_bytes()
                .to_vec(),
            vec![3],
        ]
        .concat();
        let mut reader = SessionReader::new(std::io::Cursor::new(data));
        assert!(matches!(
            reader.read_next().await,
            Err(FrameError::TooLarge(_))
        ));
        assert_eq!(reader.payload.len(), 0);
        let state = EnvelopeV2::State(Envelope::RetiredPresence {
            workspace_id: WorkspaceId::from_u128(7),
            payload: vec![0; 64 * 1024],
        });
        let mut reader = SessionReader::new(std::io::Cursor::new(encode(&state).unwrap()));
        assert_eq!(reader.read_next().await.unwrap(), Some(state));
    }

    #[tokio::test]
    async fn interrupted_read_preserves_partial_prefix_and_body() {
        use tokio::io::AsyncWriteExt;
        let (mut writer, read) = tokio::io::duplex(1024);
        let mut reader = SessionReader::new(read);
        let frame = EnvelopeV2::State(Envelope::RetiredPresence {
            workspace_id: WorkspaceId::from_u128(7),
            payload: vec![9; 32],
        });
        let wire = encode(&frame).unwrap();
        // Each read takes in what has arrived, finds the frame incomplete
        // and is dropped, as a cancelled read is.
        let interrupted = |reader: &mut SessionReader<_>| {
            let read = std::pin::pin!(reader.read_next());
            read.poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
                .is_pending()
        };
        writer.write_all(&wire[..2]).await.unwrap();
        assert!(interrupted(&mut reader));
        writer.write_all(&wire[2..8]).await.unwrap();
        assert!(interrupted(&mut reader));
        writer.write_all(&wire[8..]).await.unwrap();
        assert_eq!(reader.read_next().await.unwrap(), Some(frame));
    }
}
