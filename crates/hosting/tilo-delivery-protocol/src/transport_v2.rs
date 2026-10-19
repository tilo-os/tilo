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
//! past the session boundary sees them. Private frames never enter the state
//! codec or the durable relay store.
#![allow(missing_docs)]
use crate::peer_actions::{
    DirectedAction, MAX_PRIVATE_BYTES, SignedDirectedAction, UnavailableReason,
};
use serde::{Deserialize, Serialize};
use tilo_actions::InvocationId;
use tilo_relay_client::frame::FrameError;
use tilo_relay_protocol::transport::{Envelope, MAX_FRAME_BYTES};
use uuid::Uuid;

/// Discriminant of [`EnvelopeV2::UnsignedDelta`]: a state frame, so it takes
/// the state lane's size bound rather than the private one.
const UNSIGNED_DELTA_KIND: u8 = 11;
/// Discriminant of [`EnvelopeV2::PresenceBatch`], a state frame.
const PRESENCE_BATCH_KIND: u8 = 12;
/// Discriminant of [`EnvelopeV2::CodedDelta`], a state frame.
const CODED_DELTA_KIND: u8 = 13;

mod delta;
mod presence;
pub use delta::{
    DELTA_ROUTE_BYTES, DeltaEncoder, DeltaSegment, DeltaSlotKey, MAX_CODED_DELTA_BYTES,
    MAX_DELTA_SLOTS, encode_coded_delta,
};
pub use presence::{
    MAX_PRESENCE_SLOTS, MAX_SLOTTED_PRESENCE_BYTES, PresenceEncoder, PresenceItem, PresenceSlotKey,
    encode_presence_batch,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionCapability {
    /// Private routing exists; durable effect admission is not available.
    TransportOnly,
    DurableBroker {
        host: Uuid,
    },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum EnvelopeV2 {
    State(Envelope),
    EnableActions {
        room: Uuid,
    },
    ActionsReady {
        room: Uuid,
        capability: ActionCapability,
    },
    Directed(DirectedAction),
    Unavailable {
        room: Uuid,
        request_id: Uuid,
        target: [u8; 32],
        invocation: Option<InvocationId>,
        reason: UnavailableReason,
    },
    /// Relay-attested source identity, derived only from the mutual handshake.
    /// A receiving broker still revalidates room/action permission on admission.
    Delivered {
        action: DirectedAction,
        source_user: [u8; 32],
    },
    EnableBrokerClient {
        room: Uuid,
    },
    RegisterBroker {
        room: Uuid,
        host: Uuid,
    },
    CertifiedDelivered {
        action: DirectedAction,
        source_certificate: Vec<u8>,
    },
    /// A private action its source device signed; the host checks the
    /// signature against the authenticated session's device and routes it
    /// like [`Self::Directed`].
    SignedDirected(SignedDirectedAction),
    /// [`Self::CertifiedDelivered`] carrying the source's signed bytes
    /// unchanged; the target verifies the signature against the
    /// certificate's device key.
    CertifiedSignedDelivered {
        action: SignedDirectedAction,
        source_certificate: Vec<u8>,
    },
    /// `State(Envelope::Delta { signature: [0; 64], .. })` without the
    /// signature. [`SessionReader`] returns it as that `State` value, so
    /// nothing past the session boundary sees this variant.
    UnsignedDelta {
        workspace_id: u64,
        author_device_public_key: [u8; 32],
        authored_at_ns: u64,
        #[serde(with = "serde_bytes")]
        delta: Vec<u8>,
    },
    /// Presence payloads for one room, each coded against the session's
    /// presence slots ([`PresenceItem`]). [`SessionReader`] returns one
    /// `State(Envelope::Presence)` per item, in order.
    PresenceBatch {
        workspace_id: u64,
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
    pub fn unavailable(action: &DirectedAction, reason: UnavailableReason) -> Self {
        Self::Unavailable {
            room: action.room,
            request_id: action.request_id,
            target: action.target,
            invocation: action.message.invocation(),
            reason,
        }
    }
}

/// # Errors
///
/// Returns `postcard::Error` if the value violates its wire bounds or cannot be serialized.
pub fn encode(envelope: &EnvelopeV2) -> Result<Vec<u8>, postcard::Error> {
    let payload = postcard::to_allocvec(envelope)?;
    let limit = if matches!(
        envelope,
        EnvelopeV2::State(_)
            | EnvelopeV2::UnsignedDelta { .. }
            | EnvelopeV2::PresenceBatch { .. }
            | EnvelopeV2::CodedDelta { .. }
    ) {
        MAX_FRAME_BYTES + 1
    } else {
        MAX_PRIVATE_BYTES
    };
    // Delivered appends a certified 32-byte root; reserve it at ingress so a
    // legal Directed frame cannot disconnect its target during re-encoding.
    let reserved = usize::from(matches!(
        envelope,
        EnvelopeV2::Directed(_) | EnvelopeV2::SignedDirected(_)
    ))
    .saturating_mul(32);
    if payload.len().saturating_add(reserved) > limit {
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
    let Some(&kind) = payload.first() else {
        return Err(postcard::Error::DeserializeBadEncoding);
    };
    let limit = if is_state_kind(kind) {
        MAX_FRAME_BYTES + 1
    } else {
        MAX_PRIVATE_BYTES
    };
    if payload.len() > limit {
        return Err(postcard::Error::DeserializeBadEncoding);
    }
    let (value, rest) = postcard::take_from_bytes(payload)?;
    if !rest.is_empty() {
        return Err(postcard::Error::DeserializeBadEncoding);
    }
    Ok(value)
}

/// Whether a v2 discriminant names a state-lane frame.
const fn is_state_kind(kind: u8) -> bool {
    kind == 0
        || kind == UNSIGNED_DELTA_KIND
        || kind == PRESENCE_BATCH_KIND
        || kind == CODED_DELTA_KIND
}

/// Stateful framing survives cancellation by a concurrent writer/access audit.
/// Read the discriminant before allocating so private frames have a small cap.
pub struct SessionReader<R> {
    inner: R,
    prefix: [u8; 4],
    prefix_read: usize,
    payload: Vec<u8>,
    payload_read: usize,
    /// What this session last received in each presence slot.
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
    ///
    /// # Errors
    ///
    /// Returns `FrameError` when the stream fails or a frame is oversized or
    /// does not decode.
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
                            .push_back(EnvelopeV2::State(Envelope::Presence {
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
        if self
            .payload
            .first()
            .is_some_and(|kind| !is_state_kind(*kind))
            && len > MAX_PRIVATE_BYTES
        {
            return Err(FrameError::TooLarge(len));
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
    workspace_id: u64,
    author_device_public_key: &[u8; 32],
    authored_at_ns: u64,
    delta: &[u8],
) -> Result<Vec<u8>, postcard::Error> {
    #[derive(serde::Serialize)]
    struct Fields<'a> {
        workspace_id: u64,
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
    use crate::peer_actions::ActionMessage;

    fn keystroke(signature: [u8; 64]) -> Envelope {
        Envelope::Delta {
            workspace_id: 300,
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
        // A delta that carries a signature, and every other envelope, travel
        // as State.
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
        // The whole State frame of the same delta reads the same.
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
            workspace_id: 1,
            author_device_public_key: [1; 32],
            authored_at_ns: 1,
            delta: vec![0; MAX_PRIVATE_BYTES + 1],
            signature: [0; 64],
        };
        let wire = encode_state(&large).unwrap();
        let mut reader = SessionReader::new(std::io::Cursor::new(wire));
        assert_eq!(
            reader.read_next().await.unwrap(),
            Some(EnvelopeV2::State(large))
        );
    }

    fn presence_key(slot: u64) -> PresenceSlotKey {
        PresenceSlotKey {
            workspace_id: 300,
            author: [5; 32],
            slot,
        }
    }

    #[tokio::test]
    async fn a_presence_batch_reads_as_one_presence_per_item_in_order() {
        let pointer = |x: u8| -> std::sync::Arc<[u8]> {
            std::sync::Arc::from([&[5; 32][..], b"user key ", &[x, x], b" teal"].concat())
        };
        let event: std::sync::Arc<[u8]> = std::sync::Arc::from(b"heartbeat".to_vec());
        let mut encoder = PresenceEncoder::default();
        let first = vec![
            encoder.item(Some(presence_key(1)), &pointer(1)),
            encoder.item(None, &event),
        ];
        let second = vec![encoder.item(Some(presence_key(1)), &pointer(2))];
        assert!(second[0].middle.len() <= 2, "only the coordinates travel");
        let wire = [
            encode_presence_batch(300, &first).unwrap(),
            encode_state(&Envelope::Ping).unwrap(),
            encode_presence_batch(300, &second).unwrap(),
        ]
        .concat();
        let mut reader = SessionReader::new(std::io::Cursor::new(wire));
        let presence = |payload: &std::sync::Arc<[u8]>| {
            Some(EnvelopeV2::State(Envelope::Presence {
                workspace_id: 300,
                payload: payload.to_vec(),
            }))
        };
        assert_eq!(reader.read_next().await.unwrap(), presence(&pointer(1)));
        assert_eq!(reader.read_next().await.unwrap(), presence(&event));
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
                workspace_id: 300,
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
        let wire = encode_presence_batch(1, &[bad]).unwrap();
        let mut reader = SessionReader::new(std::io::Cursor::new(wire));
        assert!(matches!(
            reader.read_next().await,
            Err(FrameError::Decode(_))
        ));
    }

    #[test]
    fn private_frames_have_frozen_bytes() {
        let room = Uuid::from_u128(7);
        let request_id = Uuid::from_u128(8);
        let room_wire = [vec![16], vec![0; 15], vec![7]].concat();
        let request_wire = [vec![16], vec![0; 15], vec![8]].concat();
        let cases = [
            (EnvelopeV2::State(Envelope::Ping), vec![0, 6]),
            (
                EnvelopeV2::EnableActions { room },
                [vec![1], room_wire.clone()].concat(),
            ),
            (
                EnvelopeV2::ActionsReady {
                    room,
                    capability: ActionCapability::TransportOnly,
                },
                [vec![2], room_wire.clone(), vec![0]].concat(),
            ),
            (
                EnvelopeV2::Directed(DirectedAction {
                    room,
                    request_id,
                    source: [1; 32],
                    target: [2; 32],
                    message: ActionMessage::CatalogueRefresh { scope: None },
                }),
                [
                    vec![3],
                    room_wire.clone(),
                    request_wire.clone(),
                    vec![1; 32],
                    vec![2; 32],
                    vec![28],
                    br#"{"type":"catalogue_refresh"}"#.to_vec(),
                ]
                .concat(),
            ),
            (
                EnvelopeV2::Unavailable {
                    room,
                    request_id,
                    target: [2; 32],
                    invocation: None,
                    reason: UnavailableReason::Offline,
                },
                [
                    vec![4],
                    room_wire.clone(),
                    request_wire.clone(),
                    vec![2; 32],
                    vec![0, 0],
                ]
                .concat(),
            ),
            (
                EnvelopeV2::Delivered {
                    action: DirectedAction {
                        room,
                        request_id,
                        source: [1; 32],
                        target: [2; 32],
                        message: ActionMessage::CatalogueRefresh { scope: None },
                    },
                    source_user: [3; 32],
                },
                [
                    vec![5],
                    room_wire,
                    request_wire,
                    vec![1; 32],
                    vec![2; 32],
                    vec![28],
                    br#"{"type":"catalogue_refresh"}"#.to_vec(),
                    vec![3; 32],
                ]
                .concat(),
            ),
        ];
        for (frame, payload) in cases {
            let expected = [
                u32::try_from(payload.len()).unwrap().to_le_bytes().to_vec(),
                payload.clone(),
            ]
            .concat();
            assert_eq!(encode(&frame).unwrap(), expected);
            assert_eq!(decode(&payload).unwrap(), frame);
        }
    }

    #[test]
    fn broker_extension_frames_have_stable_encoding() {
        let room = Uuid::from_u128(7);
        let host = Uuid::from_u128(9);
        let room_bytes = [vec![16], vec![0; 15], vec![7]].concat();
        let host_bytes = [vec![16], vec![0; 15], vec![9]].concat();
        for (frame, frozen) in [
            (
                EnvelopeV2::EnableBrokerClient { room },
                [vec![6], room_bytes.clone()].concat(),
            ),
            (
                EnvelopeV2::RegisterBroker { room, host },
                [vec![7], room_bytes.clone(), host_bytes.clone()].concat(),
            ),
            (
                EnvelopeV2::ActionsReady {
                    room,
                    capability: ActionCapability::DurableBroker { host },
                },
                [vec![2], room_bytes, vec![1], host_bytes].concat(),
            ),
        ] {
            let wire = encode(&frame).unwrap();
            assert_eq!(wire[4..], frozen);
            assert_eq!(decode(&wire[4..]).unwrap(), frame);
        }
    }

    #[tokio::test]
    async fn private_frame_length_is_rejected_before_allocating_its_body() {
        let data = [
            u32::try_from(MAX_PRIVATE_BYTES + 1)
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
        assert_eq!(reader.payload.len(), 1);
        // The same size remains valid in the common state lane.
        let state = EnvelopeV2::State(Envelope::Presence {
            workspace_id: 7,
            payload: vec![0; MAX_PRIVATE_BYTES + 1],
        });
        let mut reader = SessionReader::new(std::io::Cursor::new(encode(&state).unwrap()));
        assert_eq!(reader.read_next().await.unwrap(), Some(state));
    }

    #[tokio::test]
    async fn interrupted_read_preserves_partial_prefix_and_body() {
        use tokio::io::AsyncWriteExt;
        let (mut writer, read) = tokio::io::duplex(1024);
        let mut reader = SessionReader::new(read);
        let wire = encode(&EnvelopeV2::EnableActions {
            room: Uuid::from_u128(7),
        })
        .unwrap();
        writer.write_all(&wire[..2]).await.unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(5), reader.read_next())
                .await
                .is_err()
        );
        writer.write_all(&wire[2..8]).await.unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(5), reader.read_next())
                .await
                .is_err()
        );
        writer.write_all(&wire[8..]).await.unwrap();
        assert_eq!(
            reader.read_next().await.unwrap(),
            Some(EnvelopeV2::EnableActions {
                room: Uuid::from_u128(7)
            })
        );
    }

    #[test]
    fn private_payload_bounds_identifiers_and_json_work() {
        use tilo_actions::{InvocationId, InvocationUpdate};
        let update = ActionMessage::Update {
            update: InvocationUpdate::Progress {
                invocation: InvocationId(Uuid::from_u128(7)),
                sequence: 1,
                phase: "x".repeat(513),
                completed: None,
                total: None,
                message: None,
            },
        };
        assert!(
            !update.validate(),
            "private phase identifiers are bounded before dispatch"
        );
        let mut argument = serde_json::json!(null);
        for _ in 0..33 {
            argument = serde_json::json!([argument]);
        }
        let invoke = ActionMessage::InvokeBound {
            host: Uuid::from_u128(9),
            invocation: InvocationId(Uuid::from_u128(7)),
            app_peer: "app".into(),
            app_id: "app".into(),
            record_id: String::new(),
            resource: "resource".into(),
            action: "action".into(),
            argument: Some(argument),
            confirmation: None,
            revision: None,
        };
        assert!(
            !invoke.validate(),
            "deep private payload is rejected before broker admission"
        );
    }

    #[test]
    fn private_codec_rejects_oversize_and_trailing_bytes() {
        assert!(decode(&[0, 6, 1]).is_err());
        let frame = EnvelopeV2::Directed(DirectedAction {
            room: Uuid::from_u128(7),
            request_id: Uuid::from_u128(8),
            source: [1; 32],
            target: [2; 32],
            message: ActionMessage::Catalogue {
                catalogue: serde_json::json!({"version":1,"surfaces":[],"private": "x".repeat(MAX_PRIVATE_BYTES)}),
            },
        });
        assert!(encode(&frame).is_err());
    }
}
