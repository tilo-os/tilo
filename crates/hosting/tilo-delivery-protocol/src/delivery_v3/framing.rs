//! Bounded delivery-v3 frame transport, independent of authentication and lanes.
//!
//! A frame is untrusted until its payload and direction are admitted by the
//! session owner. Per-frame limits here do not replace aggregate byte permits.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Delivery protocol major version.
pub const VERSION: u16 = 3;
/// Fixed bytes preceding every payload.
pub const HEADER_BYTES: usize = 12;
/// Maximum encoded authentication payload, including all routing fields.
pub const MAX_AUTH_BYTES: usize = 8 * 1024;
/// Maximum encoded chunk/private/control payload, including routing fields.
pub const MAX_PAYLOAD_BYTES: usize = 256 * 1024;

/// Closed v3 wire registry. Unknown values are not extension frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u16)]
pub enum MessageType {
    /// `ClientHello` message (1).
    ClientHello = 1,
    /// `ServerHello` message (2).
    ServerHello = 2,
    /// `ClientProof` message (3).
    ClientProof = 3,
    /// Ready message (4).
    Ready = 4,
    /// `LaneOpen` message (5).
    LaneOpen = 5,
    /// `StateItemChunk` message (10).
    StateItemChunk = 10,
    /// `StateAdmission` message (11).
    StateAdmission = 11,
    /// `EventChunk` message (12).
    EventChunk = 12,
    /// `CommitBoundary` message (13).
    CommitBoundary = 13,
    /// `FlowCredit` message (14).
    FlowCredit = 14,
    /// `CursorAck` message (15).
    CursorAck = 15,
    /// `SnapshotRequired` message (16).
    SnapshotRequired = 16,
    /// `SnapshotManifest` message (17).
    SnapshotManifest = 17,
    /// Pinned snapshot transfer chunk (18).
    SnapshotChunk = 18,
    /// Invoke message (20).
    Invoke = 20,
    /// Status message (21).
    Status = 21,
    /// Cancel message (22).
    Cancel = 22,
    /// `PrivateUpdate` message (23).
    PrivateUpdate = 23,
    /// `PrivateHistoryQuery` message (24).
    PrivateHistoryQuery = 24,
    /// `PrivateHistoryPage` message (25).
    PrivateHistoryPage = 25,
    /// Private catalogue request (26).
    CatalogueRequest = 26,
    /// Private catalogue response (27).
    CatalogueResponse = 27,
    /// Disposable agent read (28).
    AgentRead = 28,
    /// Bound disposable agent result (29).
    AgentReadResult = 29,
    /// Ping message (30).
    Ping = 30,
    /// Pong message (31).
    Pong = 31,
    /// Drain message (32).
    Drain = 32,
    /// Error message (33).
    Error = 33,
    /// Subscribe message (34).
    Subscribe = 34,
    /// Unsubscribe message (35).
    Unsubscribe = 35,
    /// Presence message (36).
    Presence = 36,
}

impl TryFrom<u16> for MessageType {
    type Error = FrameError;
    fn try_from(value: u16) -> Result<Self, FrameError> {
        match value {
            1 => Ok(Self::ClientHello),
            2 => Ok(Self::ServerHello),
            3 => Ok(Self::ClientProof),
            4 => Ok(Self::Ready),
            5 => Ok(Self::LaneOpen),
            10 => Ok(Self::StateItemChunk),
            11 => Ok(Self::StateAdmission),
            12 => Ok(Self::EventChunk),
            13 => Ok(Self::CommitBoundary),
            14 => Ok(Self::FlowCredit),
            15 => Ok(Self::CursorAck),
            16 => Ok(Self::SnapshotRequired),
            17 => Ok(Self::SnapshotManifest),
            18 => Ok(Self::SnapshotChunk),
            20 => Ok(Self::Invoke),
            21 => Ok(Self::Status),
            22 => Ok(Self::Cancel),
            23 => Ok(Self::PrivateUpdate),
            24 => Ok(Self::PrivateHistoryQuery),
            25 => Ok(Self::PrivateHistoryPage),
            26 => Ok(Self::CatalogueRequest),
            27 => Ok(Self::CatalogueResponse),
            28 => Ok(Self::AgentRead),
            29 => Ok(Self::AgentReadResult),
            30 => Ok(Self::Ping),
            31 => Ok(Self::Pong),
            32 => Ok(Self::Drain),
            33 => Ok(Self::Error),
            34 => Ok(Self::Subscribe),
            35 => Ok(Self::Unsubscribe),
            36 => Ok(Self::Presence),
            other => Err(FrameError::UnknownType(other)),
        }
    }
}

impl MessageType {
    fn limit(self) -> usize {
        if matches!(
            self,
            Self::ClientHello | Self::ServerHello | Self::ClientProof | Self::Ready
        ) {
            MAX_AUTH_BYTES
        } else {
            MAX_PAYLOAD_BYTES
        }
    }
}

/// Unauthenticated payload with a known type and checked encoded-size bound.
#[derive(Debug, PartialEq, Eq)]
pub struct Frame {
    kind: MessageType,
    payload: Vec<u8>,
}

impl Frame {
    /// Check the payload cap; this does not authorize or decode the message.
    ///
    /// # Errors
    ///
    /// Returns `FrameError` if the inputs violate this operation's bounds or invariants.
    pub fn new(kind: MessageType, payload: Vec<u8>) -> Result<Self, FrameError> {
        check_length(kind, payload.len())?;
        Ok(Self { kind, payload })
    }
    /// Registry type from the header.
    pub fn kind(&self) -> MessageType {
        self.kind
    }
    /// Exact bounded wire payload.
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }
}

/// Framing failures terminate the stream; no partial frame is admitted.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// I/O failed while transporting a frame.
    #[error("delivery frame I/O: {0}")]
    Io(#[from] std::io::Error),
    /// Header magic differs from TLD3.
    #[error("invalid delivery frame magic")]
    Magic,
    /// Protocol major is not supported.
    #[error("unsupported delivery version {0}")]
    UnsupportedVersion(u16),
    /// Registry value is reserved or unknown.
    #[error("unknown delivery message type {0}")]
    UnknownType(u16),
    /// Declared payload or aggregate output exceeds its encoded-size cap.
    #[error("delivery payload {length} exceeds {limit}")]
    Oversized {
        /// Claimed bytes.
        length: usize,
        /// Allowed bytes.
        limit: usize,
    },
    /// EOF interrupted a header or payload.
    #[error("truncated delivery frame")]
    Truncated,
    /// Reader previously failed and may not reinterpret remaining bytes.
    #[error("delivery frame reader is poisoned")]
    Poisoned,
    /// A bounded buffer allocation could not be reserved.
    #[error("delivery frame allocation failed")]
    Allocation,
}

/// Decode one exact prefix while borrowing payload and unconsumed tail.
///
/// # Errors
///
/// Returns `FrameError` if the input is not a canonical encoding within this type's bounds.
pub fn decode_frame_prefix(bytes: &[u8]) -> Result<(MessageType, &[u8], &[u8]), FrameError> {
    let (header, rest) = bytes
        .split_first_chunk::<HEADER_BYTES>()
        .ok_or(FrameError::Truncated)?;
    let (kind, size) = decode_header(header)?;
    let (payload, tail) = rest.split_at_checked(size).ok_or(FrameError::Truncated)?;
    Ok((kind, payload, tail))
}
/// Append a bounded frame atomically within the caller's total buffer limit.
///
/// # Errors
///
/// Returns `FrameError` if the value violates its wire bounds or cannot be serialized.
pub fn append_frame(
    output: &mut Vec<u8>,
    kind: MessageType,
    payload: &[u8],
    max_total: usize,
) -> Result<(), FrameError> {
    check_length(kind, payload.len())?;
    let additional = HEADER_BYTES.saturating_add(payload.len());
    let total = output
        .len()
        .checked_add(additional)
        .ok_or(FrameError::Oversized {
            length: usize::MAX,
            limit: max_total,
        })?;
    if total > max_total {
        return Err(FrameError::Oversized {
            length: total,
            limit: max_total,
        });
    }
    output
        .try_reserve_exact(additional)
        .map_err(|_| FrameError::Allocation)?;
    output.extend_from_slice(&encode_header(kind, payload.len()));
    output.extend_from_slice(payload);
    Ok(())
}

fn decode_header(header: &[u8; HEADER_BYTES]) -> Result<(MessageType, usize), FrameError> {
    if &header[..4] != b"TLD3" {
        return Err(FrameError::Magic);
    }
    let version = u16::from_le_bytes([header[4], header[5]]);
    if version != VERSION {
        return Err(FrameError::UnsupportedVersion(version));
    }
    let kind = MessageType::try_from(u16::from_le_bytes([header[6], header[7]]))?;
    let size = u32::from_le_bytes([header[8], header[9], header[10], header[11]]) as usize;
    check_length(kind, size)?;
    Ok((kind, size))
}

fn encode_header(kind: MessageType, size: usize) -> [u8; HEADER_BYTES] {
    let mut header = [0; HEADER_BYTES];
    header[..4].copy_from_slice(b"TLD3");
    header[4..6].copy_from_slice(&VERSION.to_le_bytes());
    header[6..8].copy_from_slice(&(kind as u16).to_le_bytes());
    // Callers bound every frame far below 4 GiB before encoding its header.
    header[8..12].copy_from_slice(&u32::try_from(size).unwrap_or(u32::MAX).to_le_bytes());
    header
}

fn check_length(kind: MessageType, length: usize) -> Result<(), FrameError> {
    if length > kind.limit() {
        return Err(FrameError::Oversized {
            length,
            limit: kind.limit(),
        });
    }
    Ok(())
}

/// Write one bounded frame. Cancellation discards this stream's partial frame;
/// callers must not append a new header after cancelling an in-flight write.
///
/// # Errors
///
/// Returns `FrameError` if the value violates its wire bounds or cannot be serialized.
pub async fn write_frame<W: AsyncWrite + Unpin>(
    write: &mut W,
    frame: &Frame,
) -> Result<(), FrameError> {
    check_length(frame.kind, frame.payload.len())?;
    let header = encode_header(frame.kind, frame.payload.len());
    write.write_all(&header).await?;
    write.write_all(&frame.payload).await?;
    Ok(())
}

/// Owns partial prefix/body progress so cancelling `read_next` loses no bytes.
pub struct FrameReader<R> {
    read: R,
    header: [u8; HEADER_BYTES],
    header_read: usize,
    kind: Option<MessageType>,
    payload: Vec<u8>,
    payload_read: usize,
    poisoned: bool,
}

impl<R: AsyncRead + Unpin> FrameReader<R> {
    /// Start an empty reader; no payload memory is allocated until a valid header.
    pub fn new(read: R) -> Self {
        Self {
            read,
            header: [0; HEADER_BYTES],
            header_read: 0,
            kind: None,
            payload: vec![],
            payload_read: 0,
            poisoned: false,
        }
    }

    /// Read one frame or clean EOF, preserving progress across cancellation.
    ///
    /// # Errors
    ///
    /// Returns `FrameError` if the input is not a canonical encoding within this type's bounds.
    pub async fn read_next(&mut self) -> Result<Option<Frame>, FrameError> {
        if self.poisoned {
            return Err(FrameError::Poisoned);
        }
        let result = self.read_inner().await;
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    async fn read_inner(&mut self) -> Result<Option<Frame>, FrameError> {
        while self.header_read < HEADER_BYTES {
            let count = self
                .read
                .read(self.header.get_mut(self.header_read..).unwrap_or_default())
                .await?;
            if count == 0 {
                return if self.header_read == 0 {
                    Ok(None)
                } else {
                    Err(FrameError::Truncated)
                };
            }
            self.header_read = self.header_read.saturating_add(count);
        }
        let kind = if let Some(kind) = self.kind {
            kind
        } else {
            let (kind, size) = decode_header(&self.header)?;
            self.payload
                .try_reserve_exact(size)
                .map_err(|_| FrameError::Allocation)?;
            self.payload.resize(size, 0);
            self.kind = Some(kind);
            kind
        };
        while self.payload_read < self.payload.len() {
            let count = self
                .read
                .read(
                    self.payload
                        .get_mut(self.payload_read..)
                        .unwrap_or_default(),
                )
                .await?;
            if count == 0 {
                return Err(FrameError::Truncated);
            }
            self.payload_read = self.payload_read.saturating_add(count);
        }
        self.header_read = 0;
        self.payload_read = 0;
        self.kind = None;
        Ok(Some(Frame {
            kind,
            payload: std::mem::take(&mut self.payload),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    fn header(kind: u16, size: u32) -> Vec<u8> {
        let mut bytes = b"TLD3".to_vec();
        bytes.extend_from_slice(&3u16.to_le_bytes());
        bytes.extend_from_slice(&kind.to_le_bytes());
        bytes.extend_from_slice(&size.to_le_bytes());
        bytes
    }

    #[tokio::test]
    async fn literal_header_and_consecutive_frames_roundtrip() {
        let mut bytes = vec![];
        write_frame(
            &mut bytes,
            &Frame::new(MessageType::Ping, vec![8, 9]).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            bytes,
            [b'T', b'L', b'D', b'3', 3, 0, 30, 0, 2, 0, 0, 0, 8, 9]
        );
        write_frame(&mut bytes, &Frame::new(MessageType::Pong, vec![]).unwrap())
            .await
            .unwrap();
        let mut reader = FrameReader::new(bytes.as_slice());
        let first = reader.read_next().await.unwrap().unwrap();
        assert_eq!(first.kind(), MessageType::Ping);
        assert_eq!(first.payload(), [8, 9]);
        assert_eq!(
            reader.read_next().await.unwrap().unwrap().kind(),
            MessageType::Pong
        );
        assert!(reader.read_next().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn complete_registry_is_accepted_and_reserved_values_are_rejected() {
        for kind in [
            1, 2, 3, 4, 5, 10, 11, 12, 13, 14, 15, 16, 17, 18, 20, 21, 22, 23, 24, 25, 26, 27, 28,
            29, 30, 31, 32, 33, 34, 35, 36,
        ] {
            let wire = header(kind, 0);
            let mut reader = FrameReader::new(wire.as_slice());
            assert_eq!(
                reader.read_next().await.unwrap().unwrap().kind() as u16,
                kind
            );
        }
        for kind in [0, 6, 9, 19, 37, u16::MAX] {
            let wire = header(kind, 0);
            assert!(matches!(
                FrameReader::new(wire.as_slice()).read_next().await,
                Err(FrameError::UnknownType(_))
            ));
        }
    }

    #[tokio::test]
    async fn invalid_headers_reject_before_any_body_read() {
        for (offset, value) in [(0, b'X'), (4, 4)] {
            let mut wire = header(30, 100);
            wire[offset] = value;
            assert!(FrameReader::new(wire.as_slice()).read_next().await.is_err());
        }
        for (kind, size) in [
            (1, 8193),
            (4, 8193),
            (10, 262_145),
            (20, 262_145),
            (36, u32::MAX),
        ] {
            let wire = header(kind, size);
            assert!(matches!(
                FrameReader::new(wire.as_slice()).read_next().await,
                Err(FrameError::Oversized { .. })
            ));
        }
        let wire = header(30, 0);
        let mut unsupported = wire.clone();
        unsupported[4] = 4;
        assert!(matches!(
            FrameReader::new(unsupported.as_slice()).read_next().await,
            Err(FrameError::UnsupportedVersion(4))
        ));
    }

    #[tokio::test]
    async fn every_truncated_prefix_or_body_is_an_error_and_poisoned() {
        let mut wire = header(30, 3);
        wire.extend_from_slice(&[7, 8, 9]);
        for end in 1..wire.len() {
            let mut reader = FrameReader::new(&wire[..end]);
            assert!(matches!(
                reader.read_next().await,
                Err(FrameError::Truncated)
            ));
            assert!(matches!(
                reader.read_next().await,
                Err(FrameError::Poisoned)
            ));
        }
    }

    #[tokio::test]
    async fn exact_payload_limits_are_supported() {
        for (kind, limit) in [
            (MessageType::ClientHello, 8192),
            (MessageType::StateItemChunk, 262_144),
            (MessageType::Invoke, 262_144),
        ] {
            assert!(Frame::new(kind, vec![0; limit + 1]).is_err());
            let frame = Frame::new(kind, vec![42; limit]).unwrap();
            let mut wire = vec![];
            write_frame(&mut wire, &frame).await.unwrap();
            let mut reader = FrameReader::new(wire.as_slice());
            assert_eq!(
                reader.read_next().await.unwrap().unwrap().payload().len(),
                limit
            );
        }
    }

    #[tokio::test]
    async fn cancelled_reads_resume_partial_header_and_body() {
        for split in [2, 13] {
            let mut wire = header(30, 3);
            wire.extend_from_slice(&[7, 8, 9]);
            let (mut send, recv) = tokio::io::duplex(32);
            let mut reader = FrameReader::new(recv);
            send.write_all(&wire[..split]).await.unwrap();
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(10), reader.read_next())
                    .await
                    .is_err()
            );
            send.write_all(&wire[split..]).await.unwrap();
            let result = tokio::time::timeout(tilo_test_env::hang_bound(), reader.read_next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(result.payload(), [7, 8, 9]);
        }
    }

    #[test]
    fn synchronous_frames_preserve_literal_wire_and_borrow_payload() {
        let golden = [b'T', b'L', b'D', b'3', 3, 0, 10, 0, 3, 0, 0, 0, 7, 8, 9];
        let mut wire = Vec::new();
        append_frame(&mut wire, MessageType::StateItemChunk, &[7, 8, 9], 27).unwrap();
        assert_eq!(wire, golden);
        append_frame(&mut wire, MessageType::CursorAck, &[], 27).unwrap();
        let (kind, payload, tail) = decode_frame_prefix(&wire).unwrap();
        assert_eq!(kind, MessageType::StateItemChunk);
        assert_eq!(payload, &[7, 8, 9]);
        assert_eq!(payload.as_ptr(), wire[HEADER_BYTES..].as_ptr());
        assert_eq!(tail.as_ptr(), wire[golden.len()..].as_ptr());
        let (kind, payload, tail) = decode_frame_prefix(tail).unwrap();
        assert_eq!(kind, MessageType::CursorAck);
        assert!(payload.is_empty() && tail.is_empty());
    }

    #[test]
    fn synchronous_frames_reject_headers_lengths_and_every_truncation() {
        let wire = [b'T', b'L', b'D', b'3', 3, 0, 10, 0, 3, 0, 0, 0, 7, 8, 9];
        for end in 0..wire.len() {
            assert!(matches!(
                decode_frame_prefix(&wire[..end]),
                Err(FrameError::Truncated)
            ));
        }
        let mut invalid = wire;
        invalid[0] = b'X';
        assert!(matches!(
            decode_frame_prefix(&invalid),
            Err(FrameError::Magic)
        ));
        let mut invalid = wire;
        invalid[4] = 4;
        assert!(matches!(
            decode_frame_prefix(&invalid),
            Err(FrameError::UnsupportedVersion(4))
        ));
        let mut invalid = wire;
        invalid[6] = 9;
        assert!(matches!(
            decode_frame_prefix(&invalid),
            Err(FrameError::UnknownType(9))
        ));
        for (kind, size) in [(1, 8193), (4, 8193), (10, 262_145), (36, u32::MAX)] {
            assert!(matches!(
                decode_frame_prefix(&header(kind, size)),
                Err(FrameError::Oversized { .. })
            ));
        }
        let mut trailing = wire.to_vec();
        trailing.push(0);
        let (_, _, tail) = decode_frame_prefix(&trailing).unwrap();
        assert!(matches!(
            decode_frame_prefix(tail),
            Err(FrameError::Truncated)
        ));
    }

    #[test]
    fn synchronous_append_checks_all_bounds_before_mutating_or_reserving() {
        for (kind, limit) in [
            (MessageType::ClientHello, MAX_AUTH_BYTES),
            (MessageType::StateItemChunk, MAX_PAYLOAD_BYTES),
        ] {
            let mut wire = vec![42];
            let pointer = wire.as_ptr();
            let capacity = wire.capacity();
            assert!(matches!(
                append_frame(&mut wire, kind, &vec![0; limit + 1], usize::MAX),
                Err(FrameError::Oversized { .. })
            ));
            assert_eq!(wire, &[42]);
            assert_eq!(wire.as_ptr(), pointer);
            assert_eq!(wire.capacity(), capacity);
            assert!(matches!(
                append_frame(&mut wire, kind, &[1], HEADER_BYTES + 1),
                Err(FrameError::Oversized { .. })
            ));
            assert_eq!(wire, &[42]);
            assert_eq!(wire.as_ptr(), pointer);
            assert_eq!(wire.capacity(), capacity);
            wire.clear();
            append_frame(&mut wire, kind, &vec![3; limit], HEADER_BYTES + limit).unwrap();
            assert_eq!(decode_frame_prefix(&wire).unwrap().1.len(), limit);
        }
        let mut page = vec![0; 1024 * 1024 - HEADER_BYTES - 1];
        append_frame(&mut page, MessageType::Ping, &[7], 1024 * 1024).unwrap();
        assert_eq!(page.len(), 1024 * 1024);
        let before = page.clone();
        let capacity = page.capacity();
        assert!(append_frame(&mut page, MessageType::Ping, &[], 1024 * 1024).is_err());
        assert_eq!(page, before);
        assert_eq!(page.capacity(), capacity);
    }
}
