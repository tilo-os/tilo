//! Length-prefixed frame codec for server streams.
//!
//! Every message on a server stream — the handshake, discovery and management
//! exchanges, and the post-handshake [`Envelope`] pipe — is
//! one frame: a little-endian `u32` byte count followed by that many bytes of
//! postcard. A zero-length frame is never sent, and readers enforce a caller
//! chosen upper bound before allocating. The one empty frame on a stream is
//! no message: it stands before a handshake's refusal that says why
//! ([`write_refusal`]).
//!
//! The daemon persists pending and retained envelopes in the same framed form
//! produced by [`encode`], so a queued entry is written to a peer verbatim.

use tilo_server_protocol::transport::{Envelope, MAX_FRAME_BYTES};
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

/// The most bytes a refusal that says why may declare: its server's
/// certificate, a reason and a signature fit many times over.
const MAX_REFUSAL_BYTES: usize = 64 * 1024;

/// A handshake's answer, as its server framed it.
#[derive(Debug, PartialEq, Eq)]
pub enum Answer {
    /// The answer itself, one frame.
    Frame(Vec<u8>),
    /// A refusal that says why: the frame behind an empty one
    /// ([`write_refusal`]).
    Refusal(Vec<u8>),
}

/// Write a handshake's refusal that says why, where the answer would stand:
/// an empty frame, then `payload` as one frame.
///
/// No answer is empty. A device built before refusals said why refuses the
/// empty frame ([`read_frame_or_eof`]) and reads what it read when the stream
/// ended there, a hello nobody answered; a device built since reads `payload`
/// ([`read_answer`]).
///
/// # Errors
///
/// As [`write_frame`].
pub async fn write_refusal<W: AsyncWrite + Unpin>(
    writer: &mut W,
    payload: &[u8],
) -> Result<(), std::io::Error> {
    OUTGOING.write_async(writer, &[]).await?;
    write_frame(writer, payload).await
}

/// Read a handshake's answer of at most `max` bytes, or `None` if the stream
/// ends cleanly before one starts.
///
/// # Errors
///
/// As [`read_frame_or_eof`], and [`FrameError::Truncated`] when the stream
/// ends behind the empty frame of a refusal.
pub async fn read_answer<R: AsyncRead + Unpin>(
    reader: &mut R,
    max: usize,
) -> Result<Option<Answer>, FrameError> {
    let Some(first) = Framing::little_endian(max).read_async(reader).await? else {
        return Ok(None);
    };
    if !first.is_empty() {
        return Ok(Some(Answer::Frame(first)));
    }
    bounded(max.min(MAX_REFUSAL_BYTES))
        .read_async(reader)
        .await?
        .map(|refusal| Some(Answer::Refusal(refusal)))
        .ok_or(FrameError::Truncated)
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
    pub async fn read_next(&mut self) -> Result<Option<Envelope>, FrameError> {
        self.inner.read_value_async().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::WorkspaceId;

    #[tokio::test]
    async fn frames_are_length_prefixed_and_read_back_in_bounds() {
        // Envelopes round-trip behind the length prefix.
        for e in [
            Envelope::Subscribe {
                workspace_id: WorkspaceId::from_u128(42),
            },
            Envelope::Delta {
                workspace_id: WorkspaceId::from_u128(7),
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

        assert_eq!(frame(&[7, 8, 9]).unwrap(), vec![3, 0, 0, 0, 7, 8, 9]);

        let mut wire = Vec::new();
        wire.extend_from_slice(&encode(&Envelope::Ping).unwrap());
        wire.extend_from_slice(
            &encode(&Envelope::Subscribe {
                workspace_id: WorkspaceId::from_u128(1),
            })
            .unwrap(),
        );
        wire.extend_from_slice(&encode(&Envelope::Pong).unwrap());
        let mut reader = FrameReader::new(std::io::Cursor::new(wire));
        assert_eq!(reader.read_next().await.unwrap().unwrap(), Envelope::Ping);
        assert_eq!(
            reader.read_next().await.unwrap().unwrap(),
            Envelope::Subscribe {
                workspace_id: WorkspaceId::from_u128(1)
            }
        );
        assert_eq!(reader.read_next().await.unwrap().unwrap(), Envelope::Pong);
        assert!(reader.read_next().await.unwrap().is_none());

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

    #[tokio::test]
    async fn a_refusal_that_says_why_reads_as_no_answer_to_a_device_built_before() {
        let mut wire = Vec::new();
        write_refusal(&mut wire, b"why").await.unwrap();
        assert_eq!(wire, vec![0, 0, 0, 0, 3, 0, 0, 0, b'w', b'h', b'y']);

        // A device built before reads frames one way, and refuses the first.
        let mut before = std::io::Cursor::new(wire.clone());
        assert!(matches!(
            read_frame_or_eof(&mut before, 64).await,
            Err(FrameError::EmptyFrame)
        ));

        // A device built since reads the refusal, and an answer as before.
        let mut since = std::io::Cursor::new(wire);
        assert_eq!(
            read_answer(&mut since, 64).await.unwrap(),
            Some(Answer::Refusal(b"why".to_vec()))
        );
        assert_eq!(read_answer(&mut since, 64).await.unwrap(), None);
        let mut answer = Vec::new();
        write_frame(&mut answer, b"ack").await.unwrap();
        let mut answer = std::io::Cursor::new(answer);
        assert_eq!(
            read_answer(&mut answer, 64).await.unwrap(),
            Some(Answer::Frame(b"ack".to_vec()))
        );
        assert_eq!(read_answer(&mut answer, 64).await.unwrap(), None);

        // Nothing behind the empty frame, a second empty frame and a refusal
        // past the bound are no refusal.
        for (wire, max) in [
            (vec![0, 0, 0, 0], 64),
            (vec![0, 0, 0, 0, 0, 0, 0, 0], 64),
            (vec![0, 0, 0, 0, 3, 0, 0, 0, b'w', b'h', b'y'], 2),
            (vec![0, 0, 0, 0, 3, 0, 0, 0, b'w'], 64),
        ] {
            let mut cursor = std::io::Cursor::new(wire.clone());
            assert!(
                read_answer(&mut cursor, max).await.is_err(),
                "{wire:?} within {max}"
            );
        }
    }
}
