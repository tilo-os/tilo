//! Length-prefixed framing for byte streams.
//!
//! Every socket protocol in the tree that sends discrete messages over a byte
//! stream uses the same shape: a 4-byte unsigned length followed by that many
//! payload bytes. This crate is the one implementation of that shape, and of
//! its newline-delimited counterpart, [`read_line`], for text protocols that
//! end each message with `\n`.
//!
//! A [`Framing`] names the two properties that differ between protocols and
//! therefore must be chosen explicitly at every call site:
//!
//! - the [`ByteOrder`] of the length prefix, which is part of each wire format
//!   and never defaulted; and
//! - the largest payload a peer may declare, checked before anything is
//!   allocated.
//!
//! Protocols that never send an empty payload can additionally
//! [reject](Framing::reject_empty) one.
//!
//! Reading distinguishes a peer that closed the stream between frames
//! (`Ok(None)`) from one that closed it mid-frame ([`FrameError::Truncated`]).
//!
//! The blocking API works on any [`std::io::Read`]/[`std::io::Write`]. The
//! `tokio` feature adds the same operations for tokio's `AsyncRead` and
//! `AsyncWrite`, and the `postcard` feature adds typed helpers that encode and
//! decode payloads with postcard. [`FrameReader`] owns a payload buffer that is
//! reused across frames, for receive loops on hot paths.
//!
//! ```
//! use wire_frame::Framing;
//!
//! const FRAMING: Framing = Framing::little_endian(1024);
//!
//! let mut wire = Vec::new();
//! FRAMING.write(&mut wire, b"hello").unwrap();
//! assert_eq!(wire, [5, 0, 0, 0, b'h', b'e', b'l', b'l', b'o']);
//!
//! let mut reader = wire.as_slice();
//! assert_eq!(FRAMING.read(&mut reader).unwrap().as_deref(), Some(&b"hello"[..]));
//! assert_eq!(FRAMING.read(&mut reader).unwrap(), None);
//! ```

use std::fmt;
use std::io::{self, Read, Write};

#[cfg(feature = "tokio")]
mod async_io;
mod line;
mod reader;
#[cfg(feature = "postcard")]
mod value;

pub use line::read_line;
#[cfg(feature = "tokio")]
pub use line::read_line_async;
pub use reader::FrameReader;
#[cfg(feature = "postcard")]
pub use value::decode_value;

/// Size of the length prefix in front of every frame.
pub const PREFIX_BYTES: usize = 4;

/// Byte order of the length prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ByteOrder {
    /// Least significant byte first.
    LittleEndian,
    /// Most significant byte first (network order).
    BigEndian,
}

/// How one protocol frames its messages: prefix byte order, the largest
/// payload a frame may carry, and whether an empty payload is legal.
///
/// The limit applies in both directions: a writer refuses to send a payload
/// its peer would refuse to read, and nothing is written when it does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Framing {
    order: ByteOrder,
    max_len: usize,
    allow_empty: bool,
}

impl Framing {
    /// Little-endian `u32` prefix, payloads of at most `max_len` bytes.
    pub const fn little_endian(max_len: usize) -> Self {
        Self::new(ByteOrder::LittleEndian, max_len)
    }

    /// Big-endian `u32` prefix, payloads of at most `max_len` bytes.
    pub const fn big_endian(max_len: usize) -> Self {
        Self::new(ByteOrder::BigEndian, max_len)
    }

    /// `order` prefix, payloads of at most `max_len` bytes.
    pub const fn new(order: ByteOrder, max_len: usize) -> Self {
        Self {
            order,
            max_len,
            allow_empty: true,
        }
    }

    /// Treat a zero-length frame as a protocol violation in both directions.
    #[must_use]
    pub const fn reject_empty(self) -> Self {
        Self {
            allow_empty: false,
            ..self
        }
    }

    /// The same framing with a different payload limit, for protocols whose
    /// first message is bounded more tightly than the rest.
    #[must_use]
    pub const fn with_max_len(self, max_len: usize) -> Self {
        Self { max_len, ..self }
    }

    /// Largest payload a frame may carry.
    pub const fn max_len(self) -> usize {
        self.max_len
    }

    /// Byte order of the length prefix.
    pub const fn byte_order(self) -> ByteOrder {
        self.order
    }

    /// The prefix announcing a payload of `len` bytes.
    ///
    /// # Errors
    ///
    /// [`FrameError::PayloadTooLarge`] if `len` exceeds the limit (or
    /// `u32::MAX`), [`FrameError::EmptyFrame`] if `len` is zero and empty
    /// frames are rejected.
    pub fn encode_prefix(self, len: usize) -> Result<[u8; PREFIX_BYTES], FrameError> {
        if len == 0 && !self.allow_empty {
            return Err(FrameError::EmptyFrame);
        }
        if len > self.max_len {
            return Err(FrameError::PayloadTooLarge(len));
        }
        let len = u32::try_from(len).map_err(|_| FrameError::PayloadTooLarge(len))?;
        Ok(match self.order {
            ByteOrder::LittleEndian => len.to_le_bytes(),
            ByteOrder::BigEndian => len.to_be_bytes(),
        })
    }

    /// The payload length a received prefix announces.
    ///
    /// # Errors
    ///
    /// [`FrameError::TooLarge`] if it exceeds the limit,
    /// [`FrameError::EmptyFrame`] if it is zero and empty frames are rejected.
    pub fn decode_prefix(self, prefix: [u8; PREFIX_BYTES]) -> Result<usize, FrameError> {
        let len = match self.order {
            ByteOrder::LittleEndian => u32::from_le_bytes(prefix),
            ByteOrder::BigEndian => u32::from_be_bytes(prefix),
        };
        let len = usize::try_from(len).map_err(|_| FrameError::TooLarge(usize::MAX))?;
        if len == 0 && !self.allow_empty {
            return Err(FrameError::EmptyFrame);
        }
        if len > self.max_len {
            return Err(FrameError::TooLarge(len));
        }
        Ok(len)
    }

    /// `payload` with its prefix, ready for a single write.
    ///
    /// # Errors
    ///
    /// As [`encode_prefix`](Self::encode_prefix).
    pub fn encode(self, payload: &[u8]) -> Result<Vec<u8>, FrameError> {
        let prefix = self.encode_prefix(payload.len())?;
        let mut out = Vec::with_capacity(PREFIX_BYTES.saturating_add(payload.len()));
        out.extend_from_slice(&prefix);
        out.extend_from_slice(payload);
        Ok(out)
    }

    /// Write one frame carrying `payload`. Does not flush.
    ///
    /// # Errors
    ///
    /// As [`encode_prefix`](Self::encode_prefix), in which case nothing is
    /// written, or [`FrameError::Io`] if the write fails.
    pub fn write<W: Write + ?Sized>(
        self,
        writer: &mut W,
        payload: &[u8],
    ) -> Result<(), FrameError> {
        let prefix = self.encode_prefix(payload.len())?;
        writer.write_all(&prefix)?;
        if !payload.is_empty() {
            writer.write_all(payload)?;
        }
        Ok(())
    }

    /// Read the next prefix and return the payload length it announces, or
    /// `None` if the stream ended cleanly before the frame began. The caller
    /// then reads exactly that many payload bytes itself.
    ///
    /// # Errors
    ///
    /// [`FrameError::Truncated`] if the stream ends inside the prefix,
    /// [`FrameError::Io`] if reading fails, or as
    /// [`decode_prefix`](Self::decode_prefix).
    pub fn read_len<R: Read + ?Sized>(self, reader: &mut R) -> Result<Option<usize>, FrameError> {
        let mut prefix = [0_u8; PREFIX_BYTES];
        let mut filled = 0_usize;
        while let Some(rest) = prefix.get_mut(filled..).filter(|rest| !rest.is_empty()) {
            match reader.read(rest) {
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

    /// Read the next frame's payload, or `None` if the stream ended cleanly
    /// before the frame began.
    ///
    /// # Errors
    ///
    /// As [`read_len`](Self::read_len), or [`FrameError::Truncated`] if the
    /// stream ends inside the payload.
    pub fn read<R: Read + ?Sized>(self, reader: &mut R) -> Result<Option<Vec<u8>>, FrameError> {
        let mut payload = Vec::new();
        Ok(self.read_into(reader, &mut payload)?.then_some(payload))
    }

    /// Read the next frame's payload into `buf`, replacing its contents and
    /// reusing its allocation. Returns `false` if the stream ended cleanly
    /// before the frame began, leaving `buf` empty.
    ///
    /// # Errors
    ///
    /// As [`read`](Self::read). `buf`'s contents are unspecified on error.
    pub fn read_into<R: Read + ?Sized>(
        self,
        reader: &mut R,
        buf: &mut Vec<u8>,
    ) -> Result<bool, FrameError> {
        buf.clear();
        let Some(len) = self.read_len(reader)? else {
            return Ok(false);
        };
        buf.resize(len, 0);
        reader.read_exact(buf).map_err(truncated)?;
        Ok(true)
    }
}

enum PrefixRead {
    Retry,
    Closed,
}

/// Interrupted reads are retried; an EOF error before any prefix byte is a
/// clean close, like `Ok(0)`, and after one is truncation.
fn classify_prefix_error(error: io::Error, filled: usize) -> Result<PrefixRead, FrameError> {
    match error.kind() {
        io::ErrorKind::Interrupted => Ok(PrefixRead::Retry),
        io::ErrorKind::UnexpectedEof if filled == 0 => Ok(PrefixRead::Closed),
        io::ErrorKind::UnexpectedEof => Err(FrameError::Truncated),
        _ => Err(FrameError::Io(error)),
    }
}

fn truncated(error: io::Error) -> FrameError {
    if error.kind() == io::ErrorKind::UnexpectedEof {
        FrameError::Truncated
    } else {
        FrameError::Io(error)
    }
}

/// Why a frame could not be read or written.
#[derive(Debug)]
#[non_exhaustive]
pub enum FrameError {
    /// The underlying stream failed.
    Io(io::Error),
    Truncated,
    /// The stream ended where the caller required another frame. Readers
    /// return `Ok(None)` for a clean close; callers for which the next frame
    /// is mandatory turn that into this error.
    Closed,
    /// A zero-length frame where the protocol forbids one.
    EmptyFrame,
    /// A received prefix declared this many bytes, more than the limit.
    TooLarge(usize),
    /// A payload of this many bytes is larger than the limit, so it was not
    /// sent.
    PayloadTooLarge(usize),
    /// The value could not be postcard-encoded.
    #[cfg(feature = "postcard")]
    Encode(postcard::Error),
    /// The payload did not postcard-decode as the expected type.
    #[cfg(feature = "postcard")]
    Decode(postcard::Error),
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "io: {error}"),
            Self::Truncated => f.write_str("stream ended mid-frame"),
            Self::Closed => f.write_str("stream closed before the expected frame"),
            Self::EmptyFrame => f.write_str("empty frame on the wire"),
            Self::TooLarge(len) => write!(f, "frame size {len} exceeds max"),
            Self::PayloadTooLarge(len) => {
                write!(f, "payload of {len} bytes exceeds the frame limit")
            }
            #[cfg(feature = "postcard")]
            Self::Encode(error) => write!(f, "encode: {error}"),
            #[cfg(feature = "postcard")]
            Self::Decode(error) => write!(f, "decode: {error}"),
        }
    }
}

impl std::error::Error for FrameError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            #[cfg(feature = "postcard")]
            Self::Encode(error) | Self::Decode(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for FrameError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<FrameError> for io::Error {
    /// Stream failures pass through unchanged. Truncation and closure become
    /// [`io::ErrorKind::UnexpectedEof`], a refused outgoing payload
    /// [`io::ErrorKind::InvalidInput`], and anything wrong with received bytes
    /// [`io::ErrorKind::InvalidData`].
    fn from(error: FrameError) -> Self {
        if let FrameError::Io(error) = error {
            return error;
        }
        let kind = error.kind();
        Self::new(kind, error)
    }
}

impl FrameError {
    /// The [`io::ErrorKind`] this error converts to.
    pub fn kind(&self) -> io::ErrorKind {
        match self {
            Self::Io(error) => error.kind(),
            Self::Truncated | Self::Closed => io::ErrorKind::UnexpectedEof,
            Self::PayloadTooLarge(_) => io::ErrorKind::InvalidInput,
            _ => io::ErrorKind::InvalidData,
        }
    }
}

#[cfg(test)]
mod tests;
