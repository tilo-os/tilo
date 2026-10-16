//! Newline-delimited framing: each message is the bytes before a `\n`.

use std::io::{BufRead, Read};

use crate::FrameError;

/// Read the next line of at most `max_len` bytes, without its `\n`, or
/// `None` if the stream ended cleanly before it began.
///
/// At most `max_len + 1` bytes are buffered, so a peer that never sends a
/// newline cannot make the reader grow without bound.
///
/// # Errors
///
/// [`FrameError::TooLarge`] if no `\n` arrives within `max_len` bytes,
/// [`FrameError::Truncated`] if the stream ends inside a line, or
/// [`FrameError::Io`] if the stream fails.
pub fn read_line<R: BufRead + ?Sized>(
    reader: &mut R,
    max_len: usize,
) -> Result<Option<Vec<u8>>, FrameError> {
    let mut line = Vec::new();
    let limit = u64::try_from(max_len).unwrap_or(u64::MAX).saturating_add(1);
    let read = reader.take(limit).read_until(b'\n', &mut line)?;
    finish(line, read, max_len)
}

/// [`read_line`] over tokio.
///
/// # Errors
///
/// As [`read_line`].
#[cfg(feature = "tokio")]
pub async fn read_line_async<R: tokio::io::AsyncBufRead + Unpin + ?Sized>(
    reader: &mut R,
    max_len: usize,
) -> Result<Option<Vec<u8>>, FrameError> {
    use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _};
    let mut line = Vec::new();
    let limit = u64::try_from(max_len).unwrap_or(u64::MAX).saturating_add(1);
    let read = reader.take(limit).read_until(b'\n', &mut line).await?;
    finish(line, read, max_len)
}

fn finish(mut line: Vec<u8>, read: usize, max_len: usize) -> Result<Option<Vec<u8>>, FrameError> {
    if read == 0 {
        return Ok(None);
    }
    if line.last() != Some(&b'\n') {
        return Err(if line.len() > max_len {
            FrameError::TooLarge(line.len())
        } else {
            FrameError::Truncated
        });
    }
    line.pop();
    Ok(Some(line))
}
