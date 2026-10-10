//! Side-by-side v2 live transport. State embeds the frozen v1 envelope; private
//! frames never enter the v1 codec or durable relay store.
#![allow(missing_docs)]
use crate::peer_actions::{
    DirectedAction, MAX_PRIVATE_BYTES, SignedDirectedAction, UnavailableReason,
};
use serde::{Deserialize, Serialize};
use tilo_actions::InvocationId;
use tilo_relay_client::frame::FrameError;
use tilo_relay_protocol::transport::{Envelope, MAX_FRAME_BYTES};
use uuid::Uuid;

/// Tilo v2 transport with agent reads; the envelope codec is shared with v1.
pub const ALPN_AGENT_READS_V1: &[u8] = b"tilo-relay/3";

/// [`ALPN_AGENT_READS_V1`] whose private actions may be signed by their source
/// device: a client on it may send [`EnvelopeV2::SignedDirected`] and the host
/// may deliver [`EnvelopeV2::CertifiedSignedDelivered`]. Neither variant is
/// ever written to a session on the older protocol, whose decoder lacks them.
pub const ALPN_SIGNED_ACTIONS_V1: &[u8] = b"tilo-relay/4";

/// [`ALPN_SIGNED_ACTIONS_V1`] whose state lane drops the unused outer delta
/// signature. Co-edit publishers leave `Envelope::Delta::signature` all zero
/// (authentication is the session's and the payload's own signed op), so on
/// this protocol such a delta travels as [`EnvelopeV2::UnsignedDelta`], 66
/// bytes smaller per frame and per recipient. A reader on this protocol turns
/// it back into the same `State(Envelope::Delta)` value, and the variant is
/// never written to a session on an older protocol, whose decoder lacks it.
pub const ALPN_COMPACT_STATE_V1: &[u8] = b"tilo-relay/5";

/// [`ALPN_COMPACT_STATE_V1`] whose presence arrives batched (TIL-710): the
/// relay writes the presence it forwards as [`EnvelopeV2::PresenceBatch`]
/// frames whose items are coded against what the session last received in
/// the same slot. The session reader expands a batch back into one
/// `State(Envelope::Presence)` per item, so nothing past the session boundary
/// sees one, and it is never written to a session on an older protocol.
pub const ALPN_PRESENCE_BATCH_V1: &[u8] = b"tilo-relay/6";

/// Discriminant of [`EnvelopeV2::UnsignedDelta`]: a state frame, so it takes
/// the state lane's size bound rather than the private one.
const UNSIGNED_DELTA_KIND: u8 = 11;
/// Discriminant of [`EnvelopeV2::PresenceBatch`], a state frame.
const PRESENCE_BATCH_KIND: u8 = 12;

mod presence;
pub use presence::{
    MAX_PRESENCE_SLOTS, MAX_SLOTTED_PRESENCE_BYTES, PresenceEncoder, PresenceItem, PresenceSlotKey,
    encode_presence_batch,
};

/// How a session frames its state lane, fixed by the protocol it negotiated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateFraming {
    /// `tilo-relay/1`: bare [`Envelope`] frames.
    V1,
    /// [`ALPN_AGENT_READS_V1`] and [`ALPN_SIGNED_ACTIONS_V1`]: every state
    /// frame is [`EnvelopeV2::State`].
    V2,
    /// [`ALPN_COMPACT_STATE_V1`]: [`Self::V2`], except that a delta with an
    /// all-zero signature is [`EnvelopeV2::UnsignedDelta`].
    Compact,
    /// [`ALPN_PRESENCE_BATCH_V1`]: [`Self::Compact`], plus
    /// [`EnvelopeV2::PresenceBatch`].
    Presence,
}

impl StateFraming {
    /// Whether frames carry an [`EnvelopeV2`] discriminant.
    pub const fn is_v2(self) -> bool {
        !matches!(self, Self::V1)
    }

    /// Whether a zero-signature delta travels as [`EnvelopeV2::UnsignedDelta`].
    pub const fn is_compact(self) -> bool {
        matches!(self, Self::Compact | Self::Presence)
    }

    /// Whether presence may arrive as [`EnvelopeV2::PresenceBatch`].
    pub const fn batches_presence(self) -> bool {
        matches!(self, Self::Presence)
    }

    /// Whether `kind` names a state-lane frame on this framing, which takes
    /// the state lane's size bound rather than the private one.
    const fn state_kind(self, kind: u8) -> bool {
        kind == 0
            || (self.is_compact() && kind == UNSIGNED_DELTA_KIND)
            || (self.batches_presence() && kind == PRESENCE_BATCH_KIND)
    }
}

impl From<bool> for StateFraming {
    /// The framing of a session that is (`true`) or is not (`false`) on a
    /// v2 protocol older than [`ALPN_COMPACT_STATE_V1`].
    fn from(v2: bool) -> Self {
        if v2 { Self::V2 } else { Self::V1 }
    }
}

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
    /// A private action its source device signed. Sent only on
    /// [`ALPN_SIGNED_ACTIONS_V1`]; the host checks the signature against the
    /// authenticated session's device and routes it like [`Self::Directed`].
    SignedDirected(SignedDirectedAction),
    /// [`Self::CertifiedDelivered`] carrying the source's signed bytes
    /// unchanged. Written only to [`ALPN_SIGNED_ACTIONS_V1`] sessions; the
    /// target verifies the signature against the certificate's device key.
    CertifiedSignedDelivered {
        action: SignedDirectedAction,
        source_certificate: Vec<u8>,
    },
    /// `State(Envelope::Delta { signature: [0; 64], .. })` without the
    /// signature. Written only to [`ALPN_COMPACT_STATE_V1`] sessions, whose
    /// [`SessionReader`] returns it as that `State` value, so nothing past the
    /// session boundary sees this variant.
    UnsignedDelta {
        workspace_id: u64,
        author_device_public_key: [u8; 32],
        authored_at_ns: u64,
        #[serde(with = "serde_bytes")]
        delta: Vec<u8>,
    },
    /// Presence payloads for one room, each coded against the session's
    /// presence slots ([`PresenceItem`]). Written only to
    /// [`ALPN_PRESENCE_BATCH_V1`] sessions, whose [`SessionReader`] returns
    /// one `State(Envelope::Presence)` per item, in order.
    PresenceBatch {
        workspace_id: u64,
        items: Vec<PresenceItem>,
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
        EnvelopeV2::State(_) | EnvelopeV2::UnsignedDelta { .. } | EnvelopeV2::PresenceBatch { .. }
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
    kind == 0 || kind == UNSIGNED_DELTA_KIND || kind == PRESENCE_BATCH_KIND
}

/// Stateful framing survives cancellation by a concurrent writer/access audit.
/// Read the discriminant before allocating so private frames have a small cap.
pub struct SessionReader<R> {
    inner: R,
    framing: StateFraming,
    prefix: [u8; 4],
    prefix_read: usize,
    payload: Vec<u8>,
    payload_read: usize,
    /// What this session last received in each presence slot
    /// ([`StateFraming::Presence`] only).
    presence: presence::PresenceSlots,
    /// The rest of the last [`EnvelopeV2::PresenceBatch`], returned before
    /// anything is read again. Taken without awaiting, so a cancelled read
    /// loses none of it.
    expanded: std::collections::VecDeque<EnvelopeV2>,
}
impl<R: tokio::io::AsyncRead + Unpin> SessionReader<R> {
    pub fn new(inner: R, v2: bool) -> Self {
        Self::framed(inner, StateFraming::from(v2))
    }
    /// A reader for a session that negotiated `framing`.
    pub fn framed(inner: R, framing: StateFraming) -> Self {
        Self {
            inner,
            framing,
            prefix: [0; 4],
            prefix_read: 0,
            payload: Vec::new(),
            payload_read: 0,
            presence: presence::PresenceSlots::default(),
            expanded: std::collections::VecDeque::new(),
        }
    }
    /// The framing this reader was built for.
    pub const fn framing(&self) -> StateFraming {
        self.framing
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
        let v2 = self.framing.is_v2();
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
        if len > MAX_FRAME_BYTES.saturating_add(usize::from(v2)) {
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
        let framing = self.framing;
        if v2
            && self
                .payload
                .first()
                .is_some_and(|kind| !framing.state_kind(*kind))
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
        let value = if v2 {
            decode(&self.payload)
        } else {
            tilo_relay_client::frame::decode(&self.payload).map(EnvelopeV2::State)
        }
        .map_err(FrameError::Decode)?;
        let value = match value {
            EnvelopeV2::UnsignedDelta {
                workspace_id,
                author_device_public_key,
                authored_at_ns,
                delta,
            } if framing.is_compact() => EnvelopeV2::State(Envelope::Delta {
                workspace_id,
                author_device_public_key,
                authored_at_ns,
                delta,
                signature: [0; 64],
            }),
            value @ EnvelopeV2::PresenceBatch { .. } if framing.batches_presence() => value,
            // An older protocol has no such frame.
            EnvelopeV2::UnsignedDelta { .. } | EnvelopeV2::PresenceBatch { .. } => {
                return Err(FrameError::Decode(postcard::Error::DeserializeBadEncoding));
            }
            value => value,
        };
        self.prefix_read = 0;
        self.payload_read = 0;
        self.payload.clear();
        Ok(Some(value))
    }
}

/// Encode one state envelope in the session's negotiated live framing.
///
/// # Errors
///
/// Returns a postcard error if the envelope cannot be encoded within the frame bound.
pub fn encode_state(env: &Envelope, v2: bool) -> Result<Vec<u8>, postcard::Error> {
    encode_state_framed(env, StateFraming::from(v2))
}

/// Encode one state envelope in `framing`. On [`StateFraming::Compact`] a
/// delta whose signature is all zero becomes [`EnvelopeV2::UnsignedDelta`];
/// every other envelope encodes exactly as on [`StateFraming::V2`].
///
/// # Errors
///
/// Returns a postcard error if the envelope cannot be encoded within the frame bound.
pub fn encode_state_framed(
    env: &Envelope,
    framing: StateFraming,
) -> Result<Vec<u8>, postcard::Error> {
    if framing.is_v2() {
        let payload = match env {
            Envelope::Delta {
                workspace_id,
                author_device_public_key,
                authored_at_ns,
                delta,
                signature,
            } if framing.is_compact() && *signature == [0; 64] => unsigned_delta_payload(
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
    } else {
        tilo_relay_client::frame::encode(env)
    }
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
    fn compact_framing_drops_only_an_all_zero_delta_signature() {
        let unsigned = keystroke([0; 64]);
        let v2 = encode_state_framed(&unsigned, StateFraming::V2).unwrap();
        let compact = encode_state_framed(&unsigned, StateFraming::Compact).unwrap();
        // The State and Delta discriminants become one, and the signature goes
        // with its length prefix.
        assert_eq!(v2.len().checked_sub(compact.len()), Some(66));
        assert_eq!(compact[4], UNSIGNED_DELTA_KIND);
        let owned = decode(&compact[4..]).unwrap();
        assert!(matches!(owned, EnvelopeV2::UnsignedDelta { .. }));
        assert_eq!(
            encode(&owned).unwrap(),
            compact,
            "the borrowed encoding is the variant's own encoding",
        );
        // Older framings never see the variant.
        assert_eq!(v2, encode_state(&unsigned, true).unwrap());
        assert_eq!(
            encode_state_framed(&unsigned, StateFraming::V1).unwrap(),
            encode_state(&unsigned, false).unwrap(),
        );
        // A delta that carries a signature, and every other envelope, keep
        // the v2 encoding byte for byte.
        let signed = keystroke([7; 64]);
        assert_eq!(
            encode_state_framed(&signed, StateFraming::Compact).unwrap(),
            encode_state(&signed, true).unwrap(),
        );
        assert_eq!(
            encode_state_framed(&Envelope::Ping, StateFraming::Compact).unwrap(),
            encode_state(&Envelope::Ping, true).unwrap(),
        );
    }

    #[tokio::test]
    async fn compact_reader_restores_the_state_value_and_older_readers_refuse_it() {
        let unsigned = keystroke([0; 64]);
        let compact = encode_state_framed(&unsigned, StateFraming::Compact).unwrap();
        let mut reader =
            SessionReader::framed(std::io::Cursor::new(compact.clone()), StateFraming::Compact);
        assert_eq!(
            reader.read_next().await.unwrap(),
            Some(EnvelopeV2::State(unsigned.clone()))
        );
        // The ordinary v2 frame still reads on the compact protocol.
        let mut reader = SessionReader::framed(
            std::io::Cursor::new(encode_state(&unsigned, true).unwrap()),
            StateFraming::Compact,
        );
        assert_eq!(
            reader.read_next().await.unwrap(),
            Some(EnvelopeV2::State(unsigned))
        );
        let mut reader = SessionReader::new(std::io::Cursor::new(compact), true);
        assert!(matches!(
            reader.read_next().await,
            Err(FrameError::Decode(_))
        ));
    }

    #[tokio::test]
    async fn a_large_unsigned_delta_takes_the_state_bound_only_on_the_compact_protocol() {
        let large = Envelope::Delta {
            workspace_id: 1,
            author_device_public_key: [1; 32],
            authored_at_ns: 1,
            delta: vec![0; MAX_PRIVATE_BYTES + 1],
            signature: [0; 64],
        };
        let wire = encode_state_framed(&large, StateFraming::Compact).unwrap();
        let mut reader =
            SessionReader::framed(std::io::Cursor::new(wire.clone()), StateFraming::Compact);
        assert_eq!(
            reader.read_next().await.unwrap(),
            Some(EnvelopeV2::State(large))
        );
        let mut reader = SessionReader::new(std::io::Cursor::new(wire), true);
        assert!(matches!(
            reader.read_next().await,
            Err(FrameError::TooLarge(_))
        ));
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
            encode_state_framed(&Envelope::Ping, StateFraming::Presence).unwrap(),
            encode_presence_batch(300, &second).unwrap(),
        ]
        .concat();
        let mut reader = SessionReader::framed(std::io::Cursor::new(wire), StateFraming::Presence);
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
    async fn presence_batches_exist_only_on_the_presence_protocol() {
        let item = PresenceItem {
            slot: None,
            keep_prefix: 0,
            keep_suffix: 0,
            middle: b"pointer".to_vec(),
        };
        let batch = encode_presence_batch(1, &[item]).unwrap();
        for framing in [StateFraming::V2, StateFraming::Compact] {
            let mut reader = SessionReader::framed(std::io::Cursor::new(batch.clone()), framing);
            assert!(
                matches!(reader.read_next().await, Err(FrameError::Decode(_))),
                "{framing:?} must refuse a presence batch"
            );
        }
        let mut reader = SessionReader::framed(std::io::Cursor::new(batch), StateFraming::Presence);
        assert_eq!(
            reader.read_next().await.unwrap(),
            Some(EnvelopeV2::State(Envelope::Presence {
                workspace_id: 1,
                payload: b"pointer".to_vec(),
            }))
        );
        // The presence protocol is compact too.
        let unsigned = keystroke([0; 64]);
        assert_eq!(
            encode_state_framed(&unsigned, StateFraming::Presence).unwrap(),
            encode_state_framed(&unsigned, StateFraming::Compact).unwrap(),
        );
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
        let mut reader = SessionReader::framed(std::io::Cursor::new(wire), StateFraming::Presence);
        assert!(matches!(
            reader.read_next().await,
            Err(FrameError::Decode(_))
        ));
    }

    #[test]
    fn frozen_v2_bytes_keep_private_frames_out_of_v1() {
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
        let mut reader = SessionReader::new(std::io::Cursor::new(data), true);
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
        let mut reader = SessionReader::new(std::io::Cursor::new(encode(&state).unwrap()), true);
        assert_eq!(reader.read_next().await.unwrap(), Some(state));
    }

    #[tokio::test]
    async fn interrupted_read_preserves_partial_prefix_and_body() {
        use tokio::io::AsyncWriteExt;
        let (mut writer, read) = tokio::io::duplex(1024);
        let mut reader = SessionReader::new(read, true);
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
