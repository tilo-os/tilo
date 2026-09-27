//! Side-by-side v2 live transport. State embeds the frozen v1 envelope; private
//! frames never enter the v1 codec or durable relay store.
#![allow(missing_docs)]
use crate::peer_actions::{DirectedAction, MAX_PRIVATE_BYTES, UnavailableReason};
use serde::{Deserialize, Serialize};
use tilo_actions::InvocationId;
use tilo_relay_client::frame::FrameError;
use tilo_relay_protocol::transport::{Envelope, MAX_FRAME_BYTES};
use uuid::Uuid;

/// Tilo v2 transport with agent reads; the envelope codec is shared with v1.
pub const ALPN_AGENT_READS_V1: &[u8] = b"tilo-relay/3";

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
    let limit = if matches!(envelope, EnvelopeV2::State(_)) {
        MAX_FRAME_BYTES + 1
    } else {
        MAX_PRIVATE_BYTES
    };
    // Delivered appends a certified 32-byte root; reserve it at ingress so a
    // legal Directed frame cannot disconnect its target during re-encoding.
    let reserved = usize::from(matches!(envelope, EnvelopeV2::Directed(_))).saturating_mul(32);
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
    let limit = if kind == 0 {
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

/// Stateful framing survives cancellation by a concurrent writer/access audit.
/// Read the discriminant before allocating so private frames have a small cap.
pub struct SessionReader<R> {
    inner: R,
    v2: bool,
    prefix: [u8; 4],
    prefix_read: usize,
    payload: Vec<u8>,
    payload_read: usize,
}
impl<R: tokio::io::AsyncRead + Unpin> SessionReader<R> {
    pub fn new(inner: R, v2: bool) -> Self {
        Self {
            inner,
            v2,
            prefix: [0; 4],
            prefix_read: 0,
            payload: Vec::new(),
            payload_read: 0,
        }
    }
    /// The next complete envelope, or `None` at a clean end of stream.
    ///
    /// # Errors
    ///
    /// Returns `FrameError` when the stream fails or a frame is oversized or
    /// does not decode.
    pub async fn read_next(&mut self) -> Result<Option<EnvelopeV2>, FrameError> {
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
        if len > MAX_FRAME_BYTES.saturating_add(usize::from(self.v2)) {
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
        if self.v2 && self.payload.first().is_some_and(|kind| *kind != 0) && len > MAX_PRIVATE_BYTES
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
        let value = if self.v2 {
            decode(&self.payload)
        } else {
            tilo_relay_client::frame::decode(&self.payload).map(EnvelopeV2::State)
        }
        .map_err(FrameError::Decode)?;
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
    if v2 {
        let payload = state_payload(env)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peer_actions::ActionMessage;

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
