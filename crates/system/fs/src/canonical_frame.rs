//! Frames whose payload is one value in the collab canonical encoding.
//!
//! The share, recipient, replica-acknowledgement, named-share delivery and
//! record protocols all send canonical values behind a `u32` length. Each
//! names its own [`Framing`]; this module does the encoding on top, and the
//! protocols map [`CanonicalFrameError`] into the errors their callers see.
//!
//! The canonical decode requires the payload to be exactly one value, so a
//! frame with trailing bytes is rejected, unlike a plain postcard frame.

use std::io;

use collab_types::canonical::{CanonicalError, canonical_bytes, from_canonical_bytes};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};
use wire_frame::{FrameError, Framing};

/// Why a canonical frame could not be written or read.
#[derive(Debug)]
pub(crate) enum CanonicalFrameError {
    /// The stream failed, or ended inside the payload.
    Io(io::Error),
    /// A zero-length frame where the protocol forbids one.
    Empty,
    /// A frame of this many bytes, over the protocol's limit: announced by
    /// the peer, or encoded locally and therefore not sent.
    TooLarge(usize),
    /// The value did not encode.
    Encode(CanonicalError),
    /// The payload is not exactly one canonical value of the expected type.
    Decode(CanonicalError),
}

impl From<FrameError> for CanonicalFrameError {
    fn from(error: FrameError) -> Self {
        match error {
            FrameError::Io(error) => Self::Io(error),
            FrameError::EmptyFrame => Self::Empty,
            FrameError::TooLarge(len) | FrameError::PayloadTooLarge(len) => Self::TooLarge(len),
            other => Self::Io(other.into()),
        }
    }
}

/// The error a stream that ends where a frame was required reads as: the
/// one tokio's `read_exact` returns.
pub(crate) fn closed() -> io::Error {
    io::Error::new(io::ErrorKind::UnexpectedEof, "early eof")
}

/// Encode `value` and write it as one frame. Nothing is written if it does
/// not encode or is over the limit. Does not flush.
pub(crate) async fn write<W, T>(
    framing: Framing,
    writer: &mut W,
    value: &T,
) -> Result<(), CanonicalFrameError>
where
    W: AsyncWrite + Unpin + ?Sized,
    T: Serialize + Sync,
{
    let bytes = canonical_bytes(value).map_err(CanonicalFrameError::Encode)?;
    Ok(framing.write_async(writer, &bytes).await?)
}

/// Read and decode the next frame, or `None` if the stream ended before a
/// whole length prefix arrived.
pub(crate) async fn read<R, T>(
    framing: Framing,
    reader: &mut R,
) -> Result<Option<T>, CanonicalFrameError>
where
    R: AsyncRead + Unpin + ?Sized,
    T: DeserializeOwned,
{
    let len = match framing.read_len_async(reader).await {
        Ok(Some(len)) => len,
        Ok(None) | Err(FrameError::Truncated) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut bytes = vec![0_u8; len];
    reader
        .read_exact(&mut bytes)
        .await
        .map_err(CanonicalFrameError::Io)?;
    from_canonical_bytes(&bytes)
        .map(Some)
        .map_err(CanonicalFrameError::Decode)
}

/// As [`read`], for protocols where the next frame is mandatory: a stream
/// that ends before it is [`closed`].
pub(crate) async fn read_required<R, T>(
    framing: Framing,
    reader: &mut R,
) -> Result<T, CanonicalFrameError>
where
    R: AsyncRead + Unpin + ?Sized,
    T: DeserializeOwned,
{
    read(framing, reader)
        .await?
        .ok_or_else(|| CanonicalFrameError::Io(closed()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    const FRAMING: Framing = Framing::big_endian(8).reject_empty();

    #[tokio::test]
    async fn value_is_prefixed_with_its_encoded_length() {
        let mut wire = Vec::new();
        write(FRAMING, &mut wire, &(7_u16, true)).await.unwrap();
        assert_eq!(wire, [0, 0, 0, 2, 7, 1]);
        let decoded: (u16, bool) = read(FRAMING, &mut wire.as_slice()).await.unwrap().unwrap();
        assert_eq!(decoded, (7, true));
    }

    #[tokio::test]
    async fn stream_ending_before_a_whole_prefix_is_a_clean_close() {
        for wire in [&[][..], &[0, 0][..]] {
            let frame: Option<u8> = read(FRAMING, &mut &wire[..]).await.unwrap();
            assert_eq!(frame, None);
        }
        let error = read_required::<_, u8>(FRAMING, &mut &[][..])
            .await
            .unwrap_err();
        assert!(
            matches!(error, CanonicalFrameError::Io(error) if error.kind() == io::ErrorKind::UnexpectedEof)
        );
    }

    #[tokio::test]
    async fn stream_ending_inside_the_payload_is_an_io_error() {
        let wire = [0, 0, 0, 2, 7];
        let error = read::<_, u16>(FRAMING, &mut &wire[..]).await.unwrap_err();
        assert!(
            matches!(error, CanonicalFrameError::Io(error) if error.kind() == io::ErrorKind::UnexpectedEof)
        );
    }

    #[tokio::test]
    async fn limits_apply_before_the_payload() {
        let (mut writer, mut reader) = tokio::io::duplex(16);
        writer.write_all(&[0, 0, 0, 9]).await.unwrap();
        assert!(matches!(
            read::<_, u8>(FRAMING, &mut reader).await,
            Err(CanonicalFrameError::TooLarge(9))
        ));
        assert!(matches!(
            read::<_, u8>(FRAMING, &mut &[0, 0, 0, 0][..]).await,
            Err(CanonicalFrameError::Empty)
        ));
        let mut wire = Vec::new();
        assert!(matches!(
            write(FRAMING, &mut wire, &vec![1_u8; 8]).await,
            Err(CanonicalFrameError::TooLarge(9))
        ));
        assert!(wire.is_empty());
    }

    #[tokio::test]
    async fn trailing_bytes_after_the_value_are_rejected() {
        let wire = [0, 0, 0, 2, 7, 1];
        assert!(matches!(
            read::<_, u8>(FRAMING, &mut &wire[..]).await,
            Err(CanonicalFrameError::Decode(_))
        ));
    }
}
