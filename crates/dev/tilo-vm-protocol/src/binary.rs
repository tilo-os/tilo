//! Bounded layer framing on the existing authenticated byte stream.
//! A big-endian length precedes each chunk. Zero ends with byte count and SHA-256;
//! `u32::MAX` cancels. No archive is accepted before its footer verifies.
use sha2::{Digest as _, Sha256};
use std::io::{self, Read, Write};

pub const CHUNK_BYTES: usize = crate::UPLOAD_CHUNK_BYTES;
pub const MAX_BYTES: u64 = 8 << 30;
const CANCEL: u32 = u32::MAX;

pub struct Encoder<W> {
    output: W,
    pending: Vec<u8>,
    hash: Sha256,
    bytes: u64,
}

impl<W: Write> Encoder<W> {
    #[must_use]
    pub fn new(output: W) -> Self {
        Self {
            output,
            pending: Vec::with_capacity(CHUNK_BYTES),
            hash: Sha256::new(),
            bytes: 0,
        }
    }
    fn emit(&mut self) -> io::Result<()> {
        if !self.pending.is_empty() {
            let size = u32::try_from(self.pending.len()).map_err(io::Error::other)?;
            self.output.write_all(&size.to_be_bytes())?;
            self.output.write_all(&self.pending)?;
            self.pending.clear();
        }
        Ok(())
    }
    /// Finish only after the source has reached a verified EOF.
    /// # Errors
    /// I/O failure writing the bounded chunks or final checksum.
    pub fn finish(mut self) -> io::Result<u64> {
        self.emit()?;
        self.output.write_all(&0_u32.to_be_bytes())?;
        self.output.write_all(&self.bytes.to_be_bytes())?;
        self.output.write_all(&self.hash.finalize())?;
        self.output.flush()?;
        Ok(self.bytes)
    }
    /// Cancel at a frame boundary, discarding pending bytes.
    /// # Errors
    /// I/O failure writing the cancellation marker.
    pub fn cancel(mut self) -> io::Result<()> {
        // Pending data was never framed and must not be sent on cancellation.
        self.output.write_all(&CANCEL.to_be_bytes())?;
        self.output.flush()
    }
}
impl<W: Write> Write for Encoder<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let room = CHUNK_BYTES.saturating_sub(self.pending.len());
        let taken = bytes.get(..room.min(bytes.len())).unwrap_or_default();
        let next = self
            .bytes
            .checked_add(taken.len() as u64)
            .filter(|next| *next <= MAX_BYTES)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "binary layer exceeds 8 GiB")
            })?;
        self.pending.extend_from_slice(taken);
        self.hash.update(taken);
        self.bytes = next;
        if self.pending.len() == CHUNK_BYTES {
            self.emit()?;
        }
        Ok(taken.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.output.flush()
    }
}

#[derive(Default)]
pub struct Decoder {
    pending: Vec<u8>,
    at: usize,
    hash: Sha256,
    bytes: u64,
    boundary: bool,
    failed: bool,
}
impl Decoder {
    /// Whether a complete footer/cancellation was consumed (safe RPC alignment).
    #[must_use]
    pub fn at_boundary(&self) -> bool {
        self.boundary
    }
    /// Decode a bounded chunk, refusing missing or incorrect integrity evidence.
    /// # Errors
    /// Truncation, cancellation, bad checksum or exceeded bounds.
    pub fn read(&mut self, input: &mut impl Read, output: &mut [u8]) -> io::Result<usize> {
        let result = self.read_inner(input, output);
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    fn read_inner(&mut self, input: &mut impl Read, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        if self.failed {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "binary layer already refused",
            ));
        }
        while self.at == self.pending.len() {
            if self.boundary {
                return Ok(0);
            }
            let mut prefix = [0; 4];
            input.read_exact(&mut prefix)?;
            let size = u32::from_be_bytes(prefix);
            if size == CANCEL {
                self.boundary = true;
                self.failed = true;
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionAborted,
                    "binary layer cancelled",
                ));
            }
            if size == 0 {
                let mut count = [0; 8];
                let mut hash = [0; 32];
                input.read_exact(&mut count)?;
                input.read_exact(&mut hash)?;
                self.boundary = true;
                if u64::from_be_bytes(count) != self.bytes
                    || hash.as_slice() != self.hash.clone().finalize().as_slice()
                {
                    self.failed = true;
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "binary layer count or SHA-256 mismatch",
                    ));
                }
                return Ok(0);
            }
            let size = usize::try_from(size).map_err(io::Error::other)?;
            let next = self
                .bytes
                .checked_add(size as u64)
                .filter(|next| *next <= MAX_BYTES);
            if size > CHUNK_BYTES || next.is_none() {
                self.failed = true;
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "binary layer chunk or total exceeds its bound",
                ));
            }
            self.pending.resize(size, 0);
            input.read_exact(&mut self.pending)?;
            self.hash.update(&self.pending);
            self.bytes = next.unwrap_or_default();
            self.at = 0;
        }
        let rest = self.pending.get(self.at..).unwrap_or_default();
        let size = rest.len().min(output.len());
        output
            .get_mut(..size)
            .unwrap_or_default()
            .copy_from_slice(rest.get(..size).unwrap_or_default());
        self.at = self.at.saturating_add(size);
        Ok(size)
    }
}
pub struct Reader<R> {
    input: R,
    pub decoder: Decoder,
}
impl<R> Reader<R> {
    #[must_use]
    pub fn new(input: R) -> Self {
        Self {
            input,
            decoder: Decoder::default(),
        }
    }
}
impl<R: Read> Read for Reader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        self.decoder.read(&mut self.input, output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn binary_round_trip_is_bounded_and_refuses_damage() {
        for size in [0, 1, CHUNK_BYTES * 2 + 97] {
            let source = vec![0x9b; size];
            let mut framed = Vec::new();
            let mut encoder = Encoder::new(&mut framed);
            encoder.write_all(&source).unwrap();
            assert_eq!(encoder.finish().unwrap(), size as u64);
            assert!(framed.len() <= size + 56);
            let mut reader = Reader::new(framed.as_slice());
            let mut received = Vec::new();
            reader.read_to_end(&mut received).unwrap();
            assert_eq!(received, source);
            assert!(reader.decoder.at_boundary());
            assert!(reader.decoder.pending.capacity() <= CHUNK_BYTES);
            let last = framed.last_mut().unwrap();
            *last ^= 1;
            let mut reader = Reader::new(framed.as_slice());
            assert!(reader.read_to_end(&mut Vec::new()).is_err());
            assert!(reader.decoder.at_boundary());
            let mut reader = Reader::new(&framed[..framed.len().saturating_sub(1)]);
            assert!(reader.read_to_end(&mut Vec::new()).is_err());
            assert!(!reader.decoder.at_boundary());
        }
    }
    #[test]
    fn binary_cancel_oversize_and_total_limits_fail_closed() {
        let mut framed = Vec::new();
        let mut encoder = Encoder::new(&mut framed);
        encoder.write_all(b"never emitted partial chunk").unwrap();
        encoder.cancel().unwrap();
        assert_eq!(framed.len(), 4);
        let mut reader = Reader::new(framed.as_slice());
        assert!(reader.read_to_end(&mut Vec::new()).is_err());
        assert!(reader.decoder.at_boundary());
        let oversized = u32::try_from(CHUNK_BYTES + 1).unwrap().to_be_bytes();
        let mut reader = Reader::new(oversized.as_slice());
        assert!(reader.read_to_end(&mut Vec::new()).is_err());
        assert_eq!(reader.decoder.pending.capacity(), 0);
        let mut framed = Vec::new();
        let mut encoder = Encoder::new(&mut framed);
        encoder.bytes = MAX_BYTES;
        assert!(encoder.write_all(b"x").is_err());
        assert!(framed.is_empty());
    }
}
