//! Shared relay/1 length framing for native and browser Iroh streams.

use iroh::endpoint::{RecvStream, SendStream};

/// Read a 4-byte-LE-prefix framed message from `recv`.
///
/// # Errors
///
/// Returns an error if the stream closes or errors mid-read, or if the
/// decoded length prefix is `0` or exceeds `max` — the latter guards
/// against a peer claiming an unbounded frame.
pub async fn read_frame(recv: &mut RecvStream, max: usize) -> Result<Vec<u8>, std::io::Error> {
    let mut prefix = [0u8; 4];
    recv.read_exact(&mut prefix)
        .await
        .map_err(std::io::Error::other)?;
    let n = u32::from_le_bytes(prefix) as usize;
    if n == 0 || n > max {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("frame size {n} out of range"),
        ));
    }
    let mut buf = vec![0u8; n];
    recv.read_exact(&mut buf)
        .await
        .map_err(std::io::Error::other)?;
    Ok(buf)
}

/// Write a 4-byte-LE-prefix framed message to `send`.
///
/// # Errors
///
/// Returns an error if the stream write fails, or if `data` is longer
/// than `u32::MAX` bytes and so cannot be represented in the length
/// prefix.
pub async fn write_frame(send: &mut SendStream, data: &[u8]) -> Result<(), std::io::Error> {
    let len = u32::try_from(data.len()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "frame too large to encode a u32 length prefix",
        )
    })?;
    send.write_all(&len.to_le_bytes()).await?;
    if !data.is_empty() {
        send.write_all(data).await?;
    }
    Ok(())
}
