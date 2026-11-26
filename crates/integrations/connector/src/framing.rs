//! One JSON message per line. A line longer than [`MAX_FRAME_BYTES`] is an
//! error: a 32 MiB attachment is about 43 MiB of base64, and nothing larger
//! is ever sent.

use std::io::{BufRead, Write};

use serde::Serialize;
use serde::de::DeserializeOwned;

pub const MAX_FRAME_BYTES: usize = 48 * 1024 * 1024;

fn decode<T: DeserializeOwned>(line: Option<Vec<u8>>) -> std::io::Result<Option<T>> {
    line.map(|line| {
        serde_json::from_slice(&line)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
    })
    .transpose()
}

/// Write `message` as one line and flush it.
pub fn write_frame<W: Write, T: Serialize>(out: &mut W, message: &T) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(message)?;
    line.push(b'\n');
    out.write_all(&line)?;
    out.flush()
}

/// The next message, or `None` at the end of the stream.
pub fn read_frame<R: BufRead, T: DeserializeOwned>(input: &mut R) -> std::io::Result<Option<T>> {
    decode(wire_frame::read_line(input, MAX_FRAME_BYTES)?)
}

/// [`write_frame`] over tokio.
///
/// # Errors
/// As [`write_frame`].
pub async fn write_frame_async<W, T>(out: &mut W, message: &T) -> std::io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin + Send,
    T: Serialize + Sync,
{
    use tokio::io::AsyncWriteExt as _;
    let mut line = serde_json::to_vec(message)?;
    line.push(b'\n');
    out.write_all(&line).await?;
    out.flush().await
}

/// [`read_frame`] over tokio.
///
/// # Errors
/// As [`read_frame`].
pub async fn read_frame_async<R, T>(input: &mut R) -> std::io::Result<Option<T>>
where
    R: tokio::io::AsyncBufRead + Unpin,
    T: DeserializeOwned,
{
    decode(wire_frame::read_line_async(input, MAX_FRAME_BYTES).await?)
}
