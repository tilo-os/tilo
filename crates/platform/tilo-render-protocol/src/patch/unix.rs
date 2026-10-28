//! The patch transport between processes: a [`Channel`] (`SOCK_SEQPACKET`, one batch a packet)
//! for what fits a packet, and a sealed memfd riding along for a larger batch (a window's first
//! tree of a long list), so no batch is split or streamed. The receiver copies a batch out of
//! the memfd once, into its own memory, before it reads it: the sender cannot change what the
//! receiver validated.

use std::io;

use serde::{Deserialize, Serialize};

use super::{MAX_BATCH_BYTES, Transport};
use crate::WireError;
use crate::channel::{Channel, ChannelError};
use crate::shm::{Mapping, sealed_blob};

/// Largest batch carried in the packet itself; a larger one comes in a sealed memfd.
pub const INLINE_BYTES: usize = 12 * 1024;

/// What one packet carries.
#[derive(Serialize, Deserialize)]
enum Carried {
    /// The batch itself.
    Inline(Vec<u8>),
    /// A batch of `len` bytes, in the sealed memfd that comes with the packet.
    Sealed { len: u32 },
}

/// One end of a patch connection between processes.
pub struct ChannelTransport {
    channel: Channel,
}

impl ChannelTransport {
    /// Batches over `channel`, which receives without waiting from now on.
    ///
    /// # Errors
    /// The channel's descriptor cannot be made non-blocking.
    pub fn new(channel: Channel) -> io::Result<Self> {
        channel.set_nonblocking(true)?;
        Ok(Self { channel })
    }

    /// The channel, to wait on.
    #[must_use]
    pub fn channel(&self) -> &Channel {
        &self.channel
    }
}

fn wire(error: ChannelError) -> WireError {
    match error {
        ChannelError::Wire(error) => error,
        ChannelError::Closed => WireError::Malformed("the other side closed the connection"),
        ChannelError::Io(_) => WireError::Malformed("the connection failed"),
    }
}

impl Transport for ChannelTransport {
    fn send(&mut self, batch: Vec<u8>) -> Result<(), WireError> {
        if batch.len() > MAX_BATCH_BYTES {
            return Err(WireError::OverLimit("batch size"));
        }
        if batch.len() <= INLINE_BYTES {
            return self
                .channel
                .send(&Carried::Inline(batch), &[])
                .map_err(wire);
        }
        let len = u32::try_from(batch.len()).map_err(|_| WireError::Overflow)?;
        let fd = sealed_blob("tilo-patch-batch", &batch)
            .map_err(|_| WireError::Malformed("a batch's memfd"))?;
        self.channel
            .send(&Carried::Sealed { len }, &[std::os::fd::AsFd::as_fd(&fd)])
            .map_err(wire)
    }

    fn recv(&mut self) -> Result<Option<Vec<u8>>, WireError> {
        let received = match self.channel.recv::<Carried>() {
            Ok(received) => received,
            Err(ChannelError::Io(error)) if error.kind() == io::ErrorKind::WouldBlock => {
                return Ok(None);
            }
            Err(error) => return Err(wire(error)),
        };
        match received.message {
            Carried::Inline(batch) => {
                if !received.fds.is_empty() {
                    return Err(WireError::Malformed("a descriptor with an inline batch"));
                }
                Ok(Some(batch))
            }
            Carried::Sealed { len } => {
                let [fd] = <[_; 1]>::try_from(received.fds)
                    .map_err(|_| WireError::Malformed("a sealed batch without its memfd"))?;
                let len = len as usize;
                if len > MAX_BATCH_BYTES {
                    return Err(WireError::OverLimit("batch size"));
                }
                let mapping = Mapping::map_sealed(&fd, true, len, len.saturating_add(1 << 16))
                    .map_err(|_| WireError::Malformed("a batch's memfd"))?;
                let bytes = mapping.get(0, len).ok_or(WireError::Truncated)?;
                Ok(Some(bytes.to_vec()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_and_large_batches_arrive_whole_and_in_order() {
        let (a, b) = Channel::pair().expect("a pair");
        let mut app = ChannelTransport::new(a).expect("an end");
        let mut window = ChannelTransport::new(b).expect("an end");
        assert_eq!(window.recv(), Ok(None), "nothing sent yet");
        let small = vec![7_u8; 100];
        let large: Vec<u8> = (0..200_000_u32).map(|i| (i % 251) as u8).collect();
        app.send(small.clone()).expect("sends");
        app.send(large.clone()).expect("sends");
        assert_eq!(window.recv(), Ok(Some(small)));
        assert_eq!(window.recv(), Ok(Some(large)));
        assert_eq!(window.recv(), Ok(None));
        drop(app);
        assert!(window.recv().is_err(), "the app went away");
    }
}
