//! Length-prefixed frame codec for relay streams.
//!
//! Every message on a relay stream — the handshake, discovery and management
//! exchanges, compute sessions and the post-handshake [`Envelope`] pipe — is
//! one frame: a little-endian `u32` byte count followed by that many bytes of
//! postcard. A zero-length frame is never sent, and readers enforce a caller
//! chosen upper bound before allocating.
//!
//! The daemon persists pending and retained envelopes in the same framed form
//! produced by [`encode`], so a queued entry is written to a peer verbatim.

use tilo_relay_protocol::transport::{Envelope, MAX_FRAME_BYTES};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Size of the length prefix in front of every frame.
pub const PREFIX_BYTES: usize = 4;

/// Errors from reading a frame.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// Underlying I/O.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// A frame declared length 0. The wire protocol never uses
    /// zero-length frames so this indicates a peer bug.
    #[error("empty frame on the wire")]
    EmptyFrame,
    /// Frame length exceeds the reader's bound.
    #[error("frame size {0} exceeds max")]
    TooLarge(usize),
    /// The frame bytes did not decode as an `Envelope`.
    #[error("decode: {0}")]
    Decode(postcard::Error),
}

impl From<FrameError> for std::io::Error {
    fn from(error: FrameError) -> Self {
        match error {
            FrameError::Io(error) => error,
            other => Self::new(std::io::ErrorKind::InvalidData, other),
        }
    }
}

/// Prefix `payload` with its length, ready for a single `write_all`.
///
/// # Errors
///
/// Returns [`std::io::ErrorKind::InvalidInput`] if `payload` is longer than
/// `u32::MAX` bytes and so cannot be represented in the prefix.
pub fn frame(payload: &[u8]) -> Result<Vec<u8>, std::io::Error> {
    let len = u32::try_from(payload.len()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "frame too large to encode a u32 length prefix",
        )
    })?;
    let mut out = Vec::with_capacity(PREFIX_BYTES.saturating_add(payload.len()));
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

/// Write one frame carrying `payload`.
///
/// # Errors
///
/// Returns an error if the stream write fails, or if `payload` is longer
/// than `u32::MAX` bytes.
pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    payload: &[u8],
) -> Result<(), std::io::Error> {
    writer.write_all(&frame(payload)?).await
}

/// Read the next frame, or `None` if the stream ends cleanly before one
/// starts.
///
/// # Errors
///
/// Returns [`FrameError`] if the stream fails or ends mid-frame, or the
/// declared length is zero or exceeds `max`.
pub async fn read_frame_or_eof<R: AsyncRead + Unpin>(
    reader: &mut R,
    max: usize,
) -> Result<Option<Vec<u8>>, FrameError> {
    let mut prefix = [0u8; PREFIX_BYTES];
    let (first, rest) = prefix.split_at_mut(1);
    match reader.read(first).await {
        Ok(0) => return Ok(None),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(FrameError::Io(error)),
    }
    reader.read_exact(rest).await?;
    let len = u32::from_le_bytes(prefix) as usize;
    if len == 0 {
        return Err(FrameError::EmptyFrame);
    }
    if len > max {
        return Err(FrameError::TooLarge(len));
    }
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload).await?;
    Ok(Some(payload))
}

/// Read the next frame's payload, at most `max` bytes.
///
/// # Errors
///
/// Returns an error if the stream fails or ends before a complete frame, or
/// the declared length is zero or exceeds `max`
/// ([`std::io::ErrorKind::InvalidData`]).
pub async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
    max: usize,
) -> Result<Vec<u8>, std::io::Error> {
    read_frame_or_eof(reader, max)
        .await?
        .ok_or_else(|| std::io::ErrorKind::UnexpectedEof.into())
}

/// Encode an envelope as one frame. Returns the length-prefixed bytes
/// ready for `send.write_all`.
///
/// # Errors
///
/// Returns an error if `envelope` fails to postcard-serialize, or if the
/// serialized payload is longer than `u32::MAX` bytes.
pub fn encode(envelope: &Envelope) -> Result<Vec<u8>, postcard::Error> {
    let payload = postcard::to_allocvec(envelope)?;
    frame(&payload).map_err(|_| postcard::Error::SerializeBufferFull)
}

/// Decode an envelope from a frame's payload (WITHOUT the length prefix).
///
/// # Errors
///
/// Returns an error if `payload` does not decode as a valid `Envelope`.
pub fn decode(payload: &[u8]) -> Result<Envelope, postcard::Error> {
    postcard::from_bytes(payload)
}

/// Reads one [`Envelope`] per [`read_next`](Self::read_next) from any
/// `AsyncRead`, bounded by [`MAX_FRAME_BYTES`].
pub struct FrameReader<R> {
    inner: R,
}

impl<R> FrameReader<R> {
    /// Wrap `inner` in a framing reader.
    pub fn new(inner: R) -> Self {
        Self { inner }
    }

    /// Consume + return the wrapped reader.
    pub fn into_inner(self) -> R {
        self.inner
    }

    /// Access the underlying reader mutably.
    pub fn inner_mut(&mut self) -> &mut R {
        &mut self.inner
    }
}

impl<R: AsyncRead + Unpin> FrameReader<R> {
    /// Read the next envelope. Returns `Ok(None)` on clean EOF.
    ///
    /// # Errors
    ///
    /// Returns [`FrameError`] if the underlying reader errors mid-frame, the
    /// declared length is zero or exceeds [`MAX_FRAME_BYTES`], or the frame's
    /// bytes don't decode as a valid `Envelope`.
    pub async fn read_next(&mut self) -> Result<Option<Envelope>, FrameError> {
        read_frame_or_eof(&mut self.inner, MAX_FRAME_BYTES)
            .await?
            .map(|payload| decode(&payload).map_err(FrameError::Decode))
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_roundtrip_subscribe() {
        let e = Envelope::Subscribe { workspace_id: 42 };
        let bytes = encode(&e).unwrap();
        assert!(bytes.len() > PREFIX_BYTES);
        let back = decode(&bytes[PREFIX_BYTES..]).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn envelope_roundtrip_delta() {
        let e = Envelope::Delta {
            workspace_id: 7,
            author_device_public_key: [1u8; 32],
            authored_at_ns: 1_234_567_890,
            delta: vec![1, 2, 3, 4, 5],
            signature: [9u8; 64],
        };
        let bytes = encode(&e).unwrap();
        let back = decode(&bytes[PREFIX_BYTES..]).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn prefix_is_little_endian_length() {
        assert_eq!(frame(&[7, 8, 9]).unwrap(), vec![3, 0, 0, 0, 7, 8, 9]);
    }

    #[tokio::test]
    async fn frame_reader_reads_multiple() {
        let mut wire = Vec::new();
        wire.extend_from_slice(&encode(&Envelope::Ping).unwrap());
        wire.extend_from_slice(&encode(&Envelope::Subscribe { workspace_id: 1 }).unwrap());
        wire.extend_from_slice(&encode(&Envelope::Pong).unwrap());
        let cursor = std::io::Cursor::new(wire);
        let mut reader = FrameReader::new(cursor);
        assert_eq!(reader.read_next().await.unwrap().unwrap(), Envelope::Ping);
        assert_eq!(
            reader.read_next().await.unwrap().unwrap(),
            Envelope::Subscribe { workspace_id: 1 }
        );
        assert_eq!(reader.read_next().await.unwrap().unwrap(), Envelope::Pong);
        assert!(reader.read_next().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn frame_reader_rejects_oversize() {
        let mut wire = Vec::new();
        wire.extend_from_slice(
            &u32::try_from(MAX_FRAME_BYTES)
                .unwrap_or(u32::MAX)
                .saturating_add(1)
                .to_le_bytes(),
        );
        wire.extend_from_slice(&[0u8; 8]);
        let cursor = std::io::Cursor::new(wire);
        let mut reader = FrameReader::new(cursor);
        match reader.read_next().await.unwrap_err() {
            FrameError::TooLarge(_) => {}
            other => panic!("expected TooLarge, got {other}"),
        }
    }

    #[tokio::test]
    async fn read_frame_round_trips_and_bounds() {
        let mut wire = Vec::new();
        write_frame(&mut wire, b"hello").await.unwrap();
        let mut cursor = std::io::Cursor::new(wire.clone());
        assert_eq!(read_frame(&mut cursor, 16).await.unwrap(), b"hello");
        let eof = read_frame(&mut cursor, 16).await.unwrap_err();
        assert_eq!(eof.kind(), std::io::ErrorKind::UnexpectedEof);

        let mut cursor = std::io::Cursor::new(wire);
        let oversize = read_frame(&mut cursor, 4).await.unwrap_err();
        assert_eq!(oversize.kind(), std::io::ErrorKind::InvalidData);

        let mut cursor = std::io::Cursor::new(vec![0, 0, 0, 0]);
        let empty = read_frame(&mut cursor, 16).await.unwrap_err();
        assert_eq!(empty.kind(), std::io::ErrorKind::InvalidData);
    }
}
