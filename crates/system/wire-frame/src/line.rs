//! Newline-delimited framing: each message is the bytes before a `\n`.

use std::io::{BufRead, Read};

use crate::FrameError;

/// The most bytes kept of one relayed output line: reading to a newline would
/// let an untrusted child process exhaust memory with one unterminated write.
pub const LINE_BYTES: usize = 8 * 1024;

/// Splits incoming byte chunks on `\n`, keeping at most `max_len` bytes of each
/// line and dropping the rest of an overlong line up to its `\n`.
///
/// The internal buffer holds at most `max_len` bytes. Each line is handed out
/// as its kept bytes and whether any bytes past `max_len` were dropped.
#[derive(Debug)]
pub struct BoundedLines {
    max_len: usize,
    partial: Vec<u8>,
    truncated: bool,
}

impl BoundedLines {
    /// A line cutter keeping at most `max_len` bytes per line.
    #[must_use]
    pub const fn new(max_len: usize) -> Self {
        Self {
            max_len,
            partial: Vec::new(),
            truncated: false,
        }
    }

    /// Feed `bytes` as they arrive, calling `on_line(kept, truncated)` for each
    /// `\n`-terminated line.
    pub fn push(&mut self, mut bytes: &[u8], mut on_line: impl FnMut(&[u8], bool)) {
        while !bytes.is_empty() {
            if let Some(end) = bytes.iter().position(|&byte| byte == b'\n') {
                let Some((head, tail)) = bytes.split_at_checked(end) else {
                    break;
                };
                self.append(head);
                on_line(&self.partial, self.truncated);
                self.partial.clear();
                self.truncated = false;
                bytes = tail.get(1..).unwrap_or_default();
            } else {
                self.append(bytes);
                break;
            }
        }
    }

    /// End the stream, calling `on_line(kept, truncated)` once if any bytes
    /// arrived after the last `\n`.
    pub fn finish(&mut self, on_line: impl FnOnce(&[u8], bool)) {
        if !self.partial.is_empty() || self.truncated {
            on_line(&self.partial, self.truncated);
            self.partial.clear();
            self.truncated = false;
        }
    }

    fn append(&mut self, bytes: &[u8]) {
        let room = self.max_len.saturating_sub(self.partial.len());
        if bytes.len() <= room {
            self.partial.extend_from_slice(bytes);
        } else {
            if let Some(kept) = bytes.get(..room) {
                self.partial.extend_from_slice(kept);
            }
            self.truncated = true;
        }
    }
}

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
