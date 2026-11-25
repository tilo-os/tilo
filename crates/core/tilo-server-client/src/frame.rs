//! Length-prefixed frame codec for server streams.
//!
//! Every message on a server stream — the handshake, and the frames a link
//! carries — is one frame: a little-endian `u32` byte count followed by that
//! many bytes. A zero-length frame is never sent, and readers enforce a caller
//! chosen upper bound before allocating. The one empty frame on a stream is
//! no message: it stands before a handshake's refusal that says why
//! ([`write_refusal`]).
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt as _;

    #[tokio::test]
    async fn a_frame_round_trips_behind_its_length_prefix() {
        let mut wire = Vec::new();
        write_frame(&mut wire, b"hello").await.unwrap();
        assert_eq!(&wire[..4], &5u32.to_le_bytes());
        let read = read_frame(&mut std::io::Cursor::new(wire), 64)
            .await
            .unwrap();
        assert_eq!(read, b"hello");
    }

    #[tokio::test]
    async fn a_refusal_says_why_behind_an_empty_frame() {
        let mut wire = Vec::new();
        write_refusal(&mut wire, b"removed").await.unwrap();
        assert_eq!(
            read_answer(&mut std::io::Cursor::new(wire), 64)
                .await
                .unwrap(),
            Some(Answer::Refusal(b"removed".to_vec()))
        );
    }

    #[tokio::test]
    async fn an_oversized_frame_is_refused_before_its_payload_is_read() {
        let mut wire = Vec::new();
        wire.write_all(&1_000u32.to_le_bytes()).await.unwrap();
        assert!(
            read_frame(&mut std::io::Cursor::new(wire), 64)
                .await
                .is_err()
        );
    }
}
