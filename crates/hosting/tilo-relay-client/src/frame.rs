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
use tokio::io::{AsyncRead, AsyncWrite};
use wire_frame::Framing;
pub use wire_frame::{FrameError, PREFIX_BYTES};

/// Relay frames read with a bound of `max` bytes. Empty frames are a peer bug.
const fn bounded(max: usize) -> Framing {
    Framing::little_endian(max).reject_empty()
}

/// Outgoing frames: bounded only by what the `u32` prefix can express.
const OUTGOING: Framing = Framing::little_endian(u32::MAX as usize);

/// Prefix `payload` with its length, ready for a single `write_all`.
///
/// # Errors
///
/// Returns [`std::io::ErrorKind::InvalidInput`] if `payload` is longer than
/// `u32::MAX` bytes and so cannot be represented in the prefix.
pub fn frame(payload: &[u8]) -> Result<Vec<u8>, std::io::Error> {
    Ok(OUTGOING.encode(payload)?)
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
    Ok(OUTGOING.write_async(writer, payload).await?)
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
    bounded(max).read_async(reader).await
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
    OUTGOING
        .encode_value(envelope)
        .map_err(|error| match error {
            FrameError::Encode(error) => error,
            _ => postcard::Error::SerializeBufferFull,
        })
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
/// `AsyncRead`, bounded by [`MAX_FRAME_BYTES`], reusing one payload buffer.
pub struct FrameReader<R> {
    inner: wire_frame::FrameReader<R>,
}

impl<R> FrameReader<R> {
    /// Wrap `inner` in a framing reader.
    pub const fn new(inner: R) -> Self {
        Self {
            inner: wire_frame::FrameReader::new(inner, bounded(MAX_FRAME_BYTES)),
        }
    }

    /// Consume + return the wrapped reader.
    pub fn into_inner(self) -> R {
        self.inner.into_inner()
    }

    /// Access the underlying reader mutably.
    pub const fn inner_mut(&mut self) -> &mut R {
        self.inner.inner_mut()
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
        self.inner.read_value_async().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frames_are_length_prefixed_and_read_back_in_bounds() {
        // Envelopes round-trip behind the length prefix.
        for e in [
            Envelope::Subscribe { workspace_id: 42 },
            Envelope::Delta {
                workspace_id: 7,
                author_device_public_key: [1u8; 32],
                authored_at_ns: 1_234_567_890,
                delta: vec![1, 2, 3, 4, 5],
                signature: [9u8; 64],
            },
        ] {
            let bytes = encode(&e).unwrap();
            assert!(bytes.len() > PREFIX_BYTES, "{e:?}");
            assert_eq!(decode(&bytes[PREFIX_BYTES..]).unwrap(), e);
        }

        // The prefix is the little-endian length.
        assert_eq!(frame(&[7, 8, 9]).unwrap(), vec![3, 0, 0, 0, 7, 8, 9]);

        // A reader yields consecutive frames, then a clean end.
        let mut wire = Vec::new();
        wire.extend_from_slice(&encode(&Envelope::Ping).unwrap());
        wire.extend_from_slice(&encode(&Envelope::Subscribe { workspace_id: 1 }).unwrap());
        wire.extend_from_slice(&encode(&Envelope::Pong).unwrap());
        let mut reader = FrameReader::new(std::io::Cursor::new(wire));
        assert_eq!(reader.read_next().await.unwrap().unwrap(), Envelope::Ping);
        assert_eq!(
            reader.read_next().await.unwrap().unwrap(),
            Envelope::Subscribe { workspace_id: 1 }
        );
        assert_eq!(reader.read_next().await.unwrap().unwrap(), Envelope::Pong);
        assert!(reader.read_next().await.unwrap().is_none());

        // A length over the maximum is refused.
        let mut wire = Vec::new();
        wire.extend_from_slice(
            &u32::try_from(MAX_FRAME_BYTES)
                .unwrap_or(u32::MAX)
                .saturating_add(1)
                .to_le_bytes(),
        );
        wire.extend_from_slice(&[0u8; 8]);
        let mut reader = FrameReader::new(std::io::Cursor::new(wire));
        match reader.read_next().await.unwrap_err() {
            FrameError::TooLarge(_) => {}
            other => panic!("expected TooLarge, got {other}"),
        }

        // Raw frames round-trip and are bounded: EOF, oversize and empty.
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
