//! The [`Framing`] operations for tokio's `AsyncRead`/`AsyncWrite`.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::{FrameError, Framing, PREFIX_BYTES, PrefixRead, classify_prefix_error, truncated};

impl Framing {
    /// Write one frame carrying `payload`. Does not flush.
    ///
    /// # Errors
    ///
    /// As [`write`](Self::write).
    pub async fn write_async<W: AsyncWrite + Unpin + ?Sized>(
        self,
        writer: &mut W,
        payload: &[u8],
    ) -> Result<(), FrameError> {
        let prefix = self.encode_prefix(payload.len())?;
        writer.write_all(&prefix).await?;
        if !payload.is_empty() {
            writer.write_all(payload).await?;
        }
        Ok(())
    }

    /// Read the next prefix, or `None` on a clean close before the frame.
    ///
    /// # Errors
    ///
    /// As [`read_len`](Self::read_len).
    pub async fn read_len_async<R: AsyncRead + Unpin + ?Sized>(
        self,
        reader: &mut R,
    ) -> Result<Option<usize>, FrameError> {
        let mut prefix = [0_u8; PREFIX_BYTES];
        let mut filled = 0_usize;
        while let Some(rest) = prefix.get_mut(filled..).filter(|rest| !rest.is_empty()) {
            match reader.read(rest).await {
                Ok(0) if filled == 0 => return Ok(None),
                Ok(0) => return Err(FrameError::Truncated),
                Ok(read) => filled = filled.saturating_add(read),
                Err(error) => match classify_prefix_error(error, filled)? {
                    PrefixRead::Retry => {}
                    PrefixRead::Closed => return Ok(None),
                },
            }
        }
        self.decode_prefix(prefix).map(Some)
    }

    /// Read the next frame's payload, or `None` on a clean close before it.
    ///
    /// # Errors
    ///
    /// As [`read`](Self::read).
    pub async fn read_async<R: AsyncRead + Unpin + ?Sized>(
        self,
        reader: &mut R,
    ) -> Result<Option<Vec<u8>>, FrameError> {
        let mut payload = Vec::new();
        Ok(self
            .read_into_async(reader, &mut payload)
            .await?
            .then_some(payload))
    }

    /// Read the next frame's payload into `buf`, reusing its allocation.
    ///
    /// # Errors
    ///
    /// As [`read_into`](Self::read_into).
    pub async fn read_into_async<R: AsyncRead + Unpin + ?Sized>(
        self,
        reader: &mut R,
        buf: &mut Vec<u8>,
    ) -> Result<bool, FrameError> {
        buf.clear();
        let Some(len) = self.read_len_async(reader).await? else {
            return Ok(false);
        };
        buf.resize(len, 0);
        reader.read_exact(buf).await.map_err(truncated)?;
        Ok(true)
    }
}
