//! Chunked mbox framing shared by folder inspection and the streamed import.
//!
//! A mailbox is never read whole. The splitter reads fixed chunks and hands
//! back one complete message at a time, so memory follows the largest single
//! message rather than the folder. A message larger than the per-message bound
//! is reported instead of being buffered without limit, and the splitter then
//! continues at the next message, so a caller that only counts can say the
//! folder was not fully counted without silently skipping anything.

use std::fmt;
use std::io::{self, Read};

/// Bytes appended to the frame buffer per read.
pub(crate) const CHUNK: usize = 64 * 1024;

/// The line-start marker that separates two messages in an mbox.
const BOUNDARY: &[u8] = b"From ";

/// Bytes of a possible boundary kept back between chunks, so a marker that
/// straddles two reads is still recognized.
const KEEP: usize = 4;

/// A message that does not fit the per-message bound. Callers distinguish this
/// from an I/O failure and report it in their own terms.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MessageTooLarge {
    pub limit: usize,
}

impl fmt::Display for MessageTooLarge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "a mail message is larger than the {} byte per-message bound",
            self.limit
        )
    }
}

impl std::error::Error for MessageTooLarge {}

/// Whether an error from [`MboxMessages`] is a per-message bound rather than a
/// failure to read the file.
pub(crate) fn is_too_large(error: &io::Error) -> bool {
    match error.get_ref() {
        Some(inner) => inner.is::<MessageTooLarge>(),
        None => false,
    }
}

/// The bytes of each message in order, exactly as the file holds them.
pub(crate) struct MboxMessages<R> {
    reader: R,
    /// Read bytes that are not yet classified. Never larger than one chunk
    /// plus the retained line context.
    buffer: Vec<u8>,
    /// First unconsumed byte in `buffer`. The byte before it is kept as the
    /// line context a boundary check needs.
    offset: usize,
    /// Where to resume searching for the next boundary.
    scan: usize,
    /// The message under assembly.
    current: Vec<u8>,
    in_message: bool,
    discarding: bool,
    limit: usize,
    messages: u64,
    eof: bool,
    max_buffered: usize,
    peak: usize,
    chunk: Vec<u8>,
}

impl<R: Read> MboxMessages<R> {
    pub(crate) fn new(reader: R, limit: usize) -> Self {
        Self {
            reader,
            buffer: Vec::with_capacity(CHUNK),
            offset: 0,
            scan: 0,
            current: Vec::new(),
            in_message: false,
            discarding: false,
            limit,
            messages: 0,
            eof: false,
            max_buffered: 0,
            peak: 0,
            chunk: vec![0_u8; CHUNK],
        }
    }

    /// The next complete message. `None` once the file is exhausted.
    ///
    /// # Errors
    /// Refuses a message beyond the per-message bound, or a read failure. The
    /// splitter stays on the next message after a bound refusal.
    pub(crate) fn next_message(&mut self) -> io::Result<Option<Vec<u8>>> {
        loop {
            if let Some(found) = self.boundary() {
                if !self.in_message {
                    self.in_message = true;
                    self.offset = found;
                    self.scan = found.saturating_add(BOUNDARY.len());
                    continue;
                }
                let piece = self.piece(self.offset, found);
                self.offset = found;
                self.scan = found.saturating_add(BOUNDARY.len());
                if self.discarding {
                    // The oversized message ended here; the boundary starts
                    // the next one.
                    self.discarding = false;
                    continue;
                }
                self.append(&piece)?;
                self.messages = self.messages.saturating_add(1);
                return Ok(Some(std::mem::take(&mut self.current)));
            }
            let flush_end = self.buffer.len().saturating_sub(KEEP);
            if flush_end > self.offset {
                let piece = self.piece(self.offset, flush_end);
                self.offset = flush_end;
                if self.in_message && !self.discarding {
                    self.append(&piece)?;
                }
                self.scan = self.scan.max(self.offset);
            }
            if self.eof {
                // The retained boundary prefix is the end of the last message,
                // so it belongs to the frame.
                let tail = self.piece(self.offset, self.buffer.len());
                self.offset = self.buffer.len();
                if self.in_message && !self.discarding {
                    self.append(&tail)?;
                    self.messages = self.messages.saturating_add(1);
                    // Nothing follows: a further call must not repeat it.
                    self.in_message = false;
                    return Ok(Some(std::mem::take(&mut self.current)));
                }
                self.in_message = false;
                self.discarding = false;
                return Ok(None);
            }
            self.fill()?;
        }
    }

    /// Largest message body held at once, for a caller that reports the cost
    /// of an import it just made.
    pub(crate) fn max_message_bytes(&self) -> usize {
        self.peak
    }

    /// Largest unconsumed read buffer held at once.
    pub(crate) fn max_buffered(&self) -> usize {
        self.max_buffered
    }

    fn piece(&self, start: usize, end: usize) -> Vec<u8> {
        self.buffer
            .get(start..end)
            .map_or_else(Vec::new, <[u8]>::to_vec)
    }

    /// The next message boundary at or after the scan cursor. Position zero is
    /// a boundary only at the true start of the file; afterwards the trim in
    /// [`Self::fill`] keeps the preceding byte so the line check always works.
    fn boundary(&self) -> Option<usize> {
        let from = self.scan.max(self.offset);
        let end = self.buffer.len().saturating_sub(KEEP);
        (from..end).find(|&position| {
            self.buffer
                .get(position..position.saturating_add(BOUNDARY.len()))
                .is_some_and(|marker| marker == BOUNDARY)
                && (position == 0
                    || self.buffer.get(position.saturating_sub(1)).copied() == Some(b'\n'))
        })
    }

    fn append(&mut self, piece: &[u8]) -> io::Result<()> {
        if self.current.len().saturating_add(piece.len()) > self.limit {
            self.discarding = true;
            self.current = Vec::new();
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                MessageTooLarge { limit: self.limit },
            ));
        }
        self.current.extend_from_slice(piece);
        self.peak = self.peak.max(self.current.len());
        Ok(())
    }

    fn fill(&mut self) -> io::Result<()> {
        let dropped = self.offset.saturating_sub(1);
        if dropped > 0 {
            self.buffer.drain(..dropped);
            self.offset = self.offset.saturating_sub(dropped);
            self.scan = self.scan.saturating_sub(dropped);
        }
        self.max_buffered = self.max_buffered.max(self.buffer.len());
        let read = self.reader.read(&mut self.chunk)?;
        if let Some(bytes) = self.chunk.get(..read) {
            self.buffer.extend_from_slice(bytes);
        }
        if read == 0 {
            self.eof = true;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn messages(bytes: &[u8], limit: usize) -> (Vec<Vec<u8>>, usize) {
        let mut splitter = MboxMessages::new(bytes, limit);
        let mut output = Vec::new();
        while let Some(message) = splitter.next_message().unwrap() {
            output.push(message);
        }
        (output, splitter.max_buffered())
    }

    #[test]
    fn a_boundary_split_across_chunks_still_starts_a_message() {
        let body = vec![b'a'; CHUNK.saturating_sub(3)];
        let mut file = Vec::new();
        file.extend_from_slice(b"From - Mon Jan 01 00:00:00 2024\r\n");
        file.extend_from_slice(&body);
        file.extend_from_slice(b"\r\nFrom - Mon Jan 01 00:00:00 2024\r\nSubject: two\r\n\r\nB\r\n");
        let (parsed, buffered) = messages(&file, 8 * CHUNK);
        assert_eq!(parsed.len(), 2);
        assert!(parsed[1].starts_with(b"From - Mon"));
        assert!(
            buffered <= CHUNK.saturating_add(BOUNDARY.len()),
            "{buffered} bytes buffered"
        );
        let joined: Vec<u8> = parsed.concat();
        assert_eq!(joined, file);
    }

    #[test]
    fn only_a_from_at_the_start_of_a_line_separates_messages() {
        let file = b"From a\r\nSubject: one\r\n\r\n>From the archive\r\nFrom now on\r\nFrom b\r\nSubject: two\r\n\r\nB\r\n";
        let (parsed, _) = messages(file, 4096);
        assert_eq!(parsed.len(), 3, "an mbox escapes a quoted line with `>`");
        assert!(parsed[0].ends_with(b">From the archive\r\n"));
        assert!(parsed[1].starts_with(b"From now on"));
        assert_eq!(parsed.concat(), file);
    }

    #[test]
    fn an_oversized_message_is_reported_and_the_next_one_is_still_found() {
        let mut file = Vec::new();
        file.extend_from_slice(b"From a\r\nSubject: big\r\n\r\n");
        file.extend_from_slice(&vec![b'x'; 128 * 1024]);
        file.extend_from_slice(b"\r\nFrom b\r\nSubject: small\r\n\r\nB\r\n");
        let mut splitter = MboxMessages::new(file.as_slice(), 64 * 1024);
        let error = splitter.next_message().unwrap_err();
        assert!(is_too_large(&error));
        let next = splitter.next_message().unwrap().unwrap();
        assert!(next.starts_with(b"From b\r\n"));
        assert!(splitter.max_message_bytes() <= 64 * 1024);
        assert!(splitter.next_message().unwrap().is_none());
    }

    #[test]
    fn bytes_before_the_first_boundary_are_not_a_message() {
        let (parsed, _) = messages(b"preamble\r\nFrom a\r\n\r\nA\r\n", 4096);
        assert_eq!(parsed.len(), 1);
        assert!(parsed[0].starts_with(b"From a"));
    }
}
