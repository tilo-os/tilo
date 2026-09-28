//! Shared relay/1 length framing for native and browser Iroh streams.

use iroh::endpoint::{RecvStream, SendStream};
use wire_frame::Framing;

/// Read a 4-byte-LE-prefix framed message from `recv`.
///
/// # Errors
///
/// Returns an error if the stream closes or errors mid-read, or if the
/// decoded length prefix is `0` or exceeds `max` — the latter guards
/// against a peer claiming an unbounded frame.
pub async fn read_frame(recv: &mut RecvStream, max: usize) -> Result<Vec<u8>, std::io::Error> {
    Framing::little_endian(max)
        .reject_empty()
        .read_async(recv)
        .await?
        .ok_or_else(|| std::io::ErrorKind::UnexpectedEof.into())
}

/// Write a 4-byte-LE-prefix framed message to `send`.
///
/// # Errors
///
/// Returns an error if the stream write fails, or if `data` is longer
/// than `u32::MAX` bytes and so cannot be represented in the length
/// prefix.
pub async fn write_frame(send: &mut SendStream, data: &[u8]) -> Result<(), std::io::Error> {
    Ok(Framing::little_endian(usize::MAX)
        .write_async(send, data)
        .await?)
}
