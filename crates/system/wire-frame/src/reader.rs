//! A frame reader that reuses one payload buffer across frames.

use std::io::Read;

use crate::{FrameError, Framing};

/// Reads successive frames from `R` into one buffer that is reused, so a
/// receive loop allocates only when a frame is larger than any before it.
///
/// The buffer keeps the capacity of the largest frame read so far, which is
/// bounded by the framing's limit.
///
/// The async read is cancel-safe: the bytes of a partly read frame are kept
/// in the reader, so a `select!` that drops [`read_frame_async`] for another
/// branch resumes the same frame on the next call instead of losing its
/// start and misreading the rest of the stream as a new prefix.
///
/// [`read_frame_async`]: Self::read_frame_async
#[derive(Debug)]
pub struct FrameReader<R> {
    inner: R,
    framing: Framing,
    buf: Vec<u8>,
    #[cfg(feature = "tokio")]
    partial: Partial,
}

/// How far the async read is into the current frame.
#[cfg(feature = "tokio")]
#[derive(Debug, Default)]
struct Partial {
    prefix: [u8; crate::PREFIX_BYTES],
    prefix_filled: usize,
    /// The payload length, once the prefix is complete; `buf` then holds
    /// `payload_filled` of its bytes.
    len: Option<usize>,
    payload_filled: usize,
}

impl<R> FrameReader<R> {
    /// Read frames from `inner` as `framing` describes.
    pub const fn new(inner: R, framing: Framing) -> Self {
        Self {
            inner,
            framing,
            buf: Vec::new(),
            #[cfg(feature = "tokio")]
            partial: Partial {
                prefix: [0; crate::PREFIX_BYTES],
                prefix_filled: 0,
                len: None,
                payload_filled: 0,
            },
        }
    }

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
        use tokio::io::AsyncReadExt as _;
        let partial = &mut self.partial;
        let len = loop {
            if let Some(len) = partial.len {
                break len;
            }
            let Some(rest) = partial
                .prefix
                .get_mut(partial.prefix_filled..)
                .filter(|rest| !rest.is_empty())
            else {
                let len = self.framing.decode_prefix(partial.prefix)?;
                self.buf.clear();
                self.buf.resize(len, 0);
                partial.len = Some(len);
                partial.payload_filled = 0;
                continue;
            };
            match self.inner.read(rest).await {
                Ok(0) if partial.prefix_filled == 0 => return Ok(None),
                Ok(0) => return Err(FrameError::Truncated),
                Ok(read) => partial.prefix_filled = partial.prefix_filled.saturating_add(read),
                Err(error) => match crate::classify_prefix_error(error, partial.prefix_filled)? {
                    crate::PrefixRead::Retry => {}
                    crate::PrefixRead::Closed => return Ok(None),
                },
            }
        };
        while let Some(rest) = self
            .buf
            .get_mut(partial.payload_filled..len)
            .filter(|rest| !rest.is_empty())
        {
            match self.inner.read(rest).await {
                Ok(0) => return Err(FrameError::Truncated),
                Ok(read) => partial.payload_filled = partial.payload_filled.saturating_add(read),
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(crate::truncated(error)),
            }
        }
        *partial = Partial::default();
        Ok(Some(self.buf.as_slice()))
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
