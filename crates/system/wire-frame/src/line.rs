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
    read_bounded(reader, max_len, false)
}

/// Like [`read_line`], except a line within `max_len` bytes that ends because
/// the peer closed the stream without a trailing `\n` is returned rather than
/// refused as [`FrameError::Truncated`].
///
/// For single-request sockets whose callers may close their write half after
/// sending one line without a trailing newline.
///
/// # Errors
///
/// [`FrameError::TooLarge`] if more than `max_len` bytes arrive before `\n`
/// or end of stream, or [`FrameError::Io`] if the stream fails.
pub fn read_line_or_eof<R: BufRead + ?Sized>(
    reader: &mut R,
    max_len: usize,
) -> Result<Option<Vec<u8>>, FrameError> {
    read_bounded(reader, max_len, true)
}

fn read_bounded<R: BufRead + ?Sized>(
    reader: &mut R,
    max_len: usize,
    allow_eof: bool,
) -> Result<Option<Vec<u8>>, FrameError> {
    let mut line = Vec::new();
    let limit = u64::try_from(max_len).unwrap_or(u64::MAX).saturating_add(1);
    let read = reader.take(limit).read_until(b'\n', &mut line)?;
    finish(line, read, max_len, allow_eof)
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
    read_bounded_async(reader, max_len, false).await
}

/// [`read_line_or_eof`] over tokio.
///
/// # Errors
///
/// As [`read_line_or_eof`].
#[cfg(feature = "tokio")]
pub async fn read_line_or_eof_async<R: tokio::io::AsyncBufRead + Unpin + ?Sized>(
    reader: &mut R,
    max_len: usize,
) -> Result<Option<Vec<u8>>, FrameError> {
    read_bounded_async(reader, max_len, true).await
}

#[cfg(feature = "tokio")]
async fn read_bounded_async<R: tokio::io::AsyncBufRead + Unpin + ?Sized>(
    reader: &mut R,
    max_len: usize,
    allow_eof: bool,
) -> Result<Option<Vec<u8>>, FrameError> {
    use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _};
    let mut line = Vec::new();
    let limit = u64::try_from(max_len).unwrap_or(u64::MAX).saturating_add(1);
    let read = reader.take(limit).read_until(b'\n', &mut line).await?;
    finish(line, read, max_len, allow_eof)
}

fn finish(
    mut line: Vec<u8>,
    read: usize,
    max_len: usize,
    allow_eof: bool,
) -> Result<Option<Vec<u8>>, FrameError> {
    if read == 0 {
        return Ok(None);
    }
    if line.last() != Some(&b'\n') {
        if line.len() > max_len {
            return Err(FrameError::TooLarge(line.len()));
        }
        if allow_eof {
            return Ok(Some(line));
        }
        return Err(FrameError::Truncated);
    }
    line.pop();
    Ok(Some(line))
}
