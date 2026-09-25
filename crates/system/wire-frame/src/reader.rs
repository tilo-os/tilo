//! A frame reader that reuses one payload buffer across frames.

use std::io::Read;

use crate::{FrameError, Framing};

/// Reads successive frames from `R` into one buffer that is reused, so a
/// receive loop allocates only when a frame is larger than any before it.
///
/// The buffer keeps the capacity of the largest frame read so far, which is
/// bounded by the framing's limit.
#[derive(Debug)]
pub struct FrameReader<R> {
    inner: R,
    framing: Framing,
    buf: Vec<u8>,
}

impl<R> FrameReader<R> {
    /// Read frames from `inner` as `framing` describes.
    pub const fn new(inner: R, framing: Framing) -> Self {
        Self {
            inner,
            framing,
            buf: Vec::new(),
        }
    }

    /// The framing frames are read with.
    pub const fn framing(&self) -> Framing {
        self.framing
    }

    /// The wrapped reader.
    pub const fn get_ref(&self) -> &R {
        &self.inner
    }

    /// The wrapped reader, mutably. Reading from it directly desynchronises
    /// the framing unless it is done exactly at a frame boundary.
    pub const fn inner_mut(&mut self) -> &mut R {
        &mut self.inner
    }

    /// Consume the reader, returning the wrapped one.
    pub fn into_inner(self) -> R {
        self.inner
    }
}

impl<R: Read> FrameReader<R> {
    /// The next frame's payload, or `None` on a clean close before it. The
    /// slice is valid until the next read.
    ///
    /// # Errors
    ///
    /// As [`Framing::read`].
    pub fn read_frame(&mut self) -> Result<Option<&[u8]>, FrameError> {
        let framed = self.framing.read_into(&mut self.inner, &mut self.buf)?;
        Ok(framed.then_some(self.buf.as_slice()))
    }

    /// Read and decode the next postcard frame, or `None` on a clean close.
    ///
    /// # Errors
    ///
    /// As [`Framing::read_value`].
    #[cfg(feature = "postcard")]
    pub fn read_value<T: serde::de::DeserializeOwned>(&mut self) -> Result<Option<T>, FrameError> {
        self.read_frame()?.map(crate::decode_value).transpose()
    }
}

#[cfg(feature = "tokio")]
impl<R: tokio::io::AsyncRead + Unpin> FrameReader<R> {
    /// The next frame's payload, or `None` on a clean close before it.
    ///
    /// # Errors
    ///
    /// As [`Framing::read_async`].
    pub async fn read_frame_async(&mut self) -> Result<Option<&[u8]>, FrameError> {
        let framed = self
            .framing
            .read_into_async(&mut self.inner, &mut self.buf)
            .await?;
        Ok(framed.then_some(self.buf.as_slice()))
    }

    /// Read and decode the next postcard frame, or `None` on a clean close.
    ///
    /// # Errors
    ///
    /// As [`Framing::read_value_async`].
    #[cfg(feature = "postcard")]
    pub async fn read_value_async<T: serde::de::DeserializeOwned>(
        &mut self,
    ) -> Result<Option<T>, FrameError> {
        self.read_frame_async()
            .await?
            .map(crate::decode_value)
            .transpose()
    }
}
