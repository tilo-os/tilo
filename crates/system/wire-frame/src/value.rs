//! Frames whose payload is one postcard-encoded value.

use std::io::{Read, Write};

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{FrameError, Framing, PREFIX_BYTES};

/// Decode one postcard value from a frame payload (without its prefix).
///
/// # Errors
///
/// [`FrameError::Decode`] if `payload` is not a valid encoding of `T`.
pub fn decode_value<T: DeserializeOwned>(payload: &[u8]) -> Result<T, FrameError> {
    postcard::from_bytes(payload).map_err(FrameError::Decode)
}

impl Framing {
    /// `value` postcard-encoded behind its prefix, ready for a single write.
    ///
    /// # Errors
    ///
    /// [`FrameError::Encode`] if `value` does not serialize, or as
    /// [`encode_prefix`](Self::encode_prefix) for the encoded size.
    pub fn encode_value<T: Serialize + ?Sized>(self, value: &T) -> Result<Vec<u8>, FrameError> {
        let mut out =
            postcard::to_extend(value, vec![0_u8; PREFIX_BYTES]).map_err(FrameError::Encode)?;
        let prefix = self.encode_prefix(out.len().saturating_sub(PREFIX_BYTES))?;
        if let Some(head) = out.get_mut(..PREFIX_BYTES) {
            head.copy_from_slice(&prefix);
        }
        Ok(out)
    }

    /// Write `value` as one postcard frame. Does not flush.
    ///
    /// # Errors
    ///
    /// As [`encode_value`](Self::encode_value), in which case nothing is
    /// written, or [`FrameError::Io`] if the write fails.
    pub fn write_value<W: Write + ?Sized, T: Serialize + ?Sized>(
        self,
        writer: &mut W,
        value: &T,
    ) -> Result<(), FrameError> {
        writer.write_all(&self.encode_value(value)?)?;
        Ok(())
    }

    /// Read and decode the next postcard frame, or `None` on a clean close
    /// before it.
    ///
    /// # Errors
    ///
    /// As [`read`](Self::read), or [`FrameError::Decode`].
    pub fn read_value<R: Read + ?Sized, T: DeserializeOwned>(
        self,
        reader: &mut R,
    ) -> Result<Option<T>, FrameError> {
        self.read(reader)?
            .map(|payload| decode_value(&payload))
            .transpose()
    }
}

#[cfg(feature = "tokio")]
impl Framing {
    /// Write `value` as one postcard frame. Does not flush.
    ///
    /// # Errors
    ///
    /// As [`write_value`](Self::write_value).
    pub async fn write_value_async<W, T>(self, writer: &mut W, value: &T) -> Result<(), FrameError>
    where
        W: tokio::io::AsyncWrite + Unpin + ?Sized,
        T: Serialize + Sync + ?Sized,
    {
        use tokio::io::AsyncWriteExt as _;
        writer.write_all(&self.encode_value(value)?).await?;
        Ok(())
    }

    /// Read and decode the next postcard frame, or `None` on a clean close
    /// before it.
    ///
    /// # Errors
    ///
    /// As [`read_value`](Self::read_value).
    pub async fn read_value_async<R, T>(self, reader: &mut R) -> Result<Option<T>, FrameError>
    where
        R: tokio::io::AsyncRead + Unpin + ?Sized,
        T: DeserializeOwned,
    {
        self.read_async(reader)
            .await?
            .map(|payload| decode_value(&payload))
            .transpose()
    }
}
