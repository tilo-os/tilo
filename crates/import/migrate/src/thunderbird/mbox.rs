//! Chunked mbox framing shared by folder inspection and the streamed import.
//!
//! A mailbox is never read whole: memory follows the largest single message. A message over the
//! per-message bound is reported and the splitter continues at the next one, so a counter can say the
//! folder was not fully counted.

use std::fmt;
use std::io::{self, Read};

pub const CHUNK: usize = 64 * 1024;

const BOUNDARY: &[u8] = b"From ";

/// Kept back between chunks, so a marker that straddles two reads is still recognized.
const KEEP: usize = 4;

/// Callers distinguish this from an I/O failure and report it in their own terms.
#[derive(Clone, Copy, Debug)]
pub struct MessageTooLarge {
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

pub fn is_too_large(error: &io::Error) -> bool {
    match error.get_ref() {
        Some(inner) => inner.is::<MessageTooLarge>(),
        None => false,
    }
}

pub struct MboxMessages<R> {
    reader: R,
    /// Never larger than one chunk plus the retained line context.
    buffer: Vec<u8>,
    /// The byte before it is kept as the line context a boundary check needs.
    offset: usize,
    scan: usize,
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
    pub fn new(reader: R, limit: usize) -> Self {
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

    /// `None` once the file is exhausted. Refuses a message beyond the bound or a read failure, and
    /// stays on the next message after a bound refusal.
    pub fn next_message(&mut self) -> io::Result<Option<Vec<u8>>> {
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
                    // The oversized message ended here; the boundary starts the next one.
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
                // The retained boundary prefix ends the last message, so it belongs to the frame.
                let tail = self.piece(self.offset, self.buffer.len());
                self.offset = self.buffer.len();
                if self.in_message && !self.discarding {
                    self.append(&tail)?;
                    self.messages = self.messages.saturating_add(1);
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

    pub fn max_message_bytes(&self) -> usize {
        self.peak
    }

    pub fn max_buffered(&self) -> usize {
        self.max_buffered
    }

    fn piece(&self, start: usize, end: usize) -> Vec<u8> {
        self.buffer
            .get(start..end)
            .map_or_else(Vec::new, <[u8]>::to_vec)
    }

    /// Position zero is a boundary only at the true start of the file; afterwards the trim in
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
