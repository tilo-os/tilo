//! The two links the person's speakers and audio routes travel: the shell link between a desktop
//! session and its device's daemon, and the daemon's own link to another of the person's devices.
//!
//! A session holds no endpoint of its own, so a route it starts or answers is carried by the
//! daemon: the session asks the daemon to hand one envelope to another device, the daemon writes
//! it on its own connection, and the device's daemon hands what arrives to its session. This is
//! the frame both links speak: a length-prefixed postcard [`Frame`].
//!
//! | Bytes | Field |
//! | --- | --- |
//! | 4 | payload length, little-endian, at most [`MAX_FRAME`] |
//! | length | the postcard [`Frame`] |

use std::io;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

/// The protocol two of the person's devices speak to carry their speakers' audio and the actions
/// that start, approve and keep it.
pub const ALPN: &[u8] = b"tilo-speakers/1";

/// The largest frame one link carries: one personal-fabric envelope and its room.
pub const MAX_FRAME: usize = 128 * 1024;

/// One thing a link says.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Frame {
    /// To the daemon: hand `envelope`, one device of the person's, to `peer`. Between devices:
    /// the envelope the device at the other end of the link is sent.
    Send { peer: [u8; 32], envelope: Vec<u8> },
    /// To the session: `peer` sent this envelope to this device, which the daemon took at
    /// `received_us` on the monotonic clock both are on.
    Heard {
        peer: [u8; 32],
        envelope: Vec<u8>,
        #[serde(default)]
        received_us: u64,
    },
}

/// Write `frame` on `writer`.
///
/// # Errors
/// The frame is larger than a link carries, or the writer failed.
pub async fn write<W: AsyncWrite + Unpin>(writer: &mut W, frame: &Frame) -> io::Result<()> {
    let payload = postcard::to_allocvec(frame)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    if payload.len() > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "a speaker link frame exceeds what a link carries",
        ));
    }
    let length = u32::try_from(payload.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "a frame is longer than 4 GiB"))?;
    writer.write_all(&length.to_le_bytes()).await?;
    writer.write_all(&payload).await?;
    writer.flush().await
}

/// Read one frame from `reader`.
///
/// # Errors
/// The link ended, the length is larger than a link carries, or the frame does not read.
pub async fn read<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<Frame> {
    let mut length = [0_u8; 4];
    reader.read_exact(&mut length).await?;
    let length = usize::try_from(u32::from_le_bytes(length))
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "a frame is longer than 4 GiB"))?;
    if length > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "a speaker link frame exceeds what a link carries",
        ));
    }
    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload).await?;
    postcard::from_bytes(&payload)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))
}
