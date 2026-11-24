//! The patch transport between processes on Windows: one pipe (`tilo_socket::Stream`), each batch
//! a frame of its length and its bytes, whatever its size. A pipe carries any length and the
//! receiver reads the batch into its own memory as it arrives, so no batch rides in shared memory:
//! the two processes of a window and its instance are siblings, which hand each other no handles.

use std::io::{self, Read as _, Write as _};

use tilo_socket::Stream;

use super::{MAX_BATCH_BYTES, Transport};
use crate::WireError;

/// The bytes before each batch: its length.
const LENGTH: usize = size_of::<u32>();

/// One end of a patch connection between processes.
pub struct PipeTransport {
    stream: Stream,
    /// The other end went: every later call answers [`WireError::Closed`].
    closed: bool,
}

impl PipeTransport {
    /// Batches over `stream`.
    #[must_use]
    pub fn new(stream: Stream) -> Self {
        Self {
            stream,
            closed: false,
        }
    }

    /// The pipe, to wait on.
    #[must_use]
    pub fn stream(&self) -> &Stream {
        &self.stream
    }

    fn failed(&mut self, error: &io::Error) -> WireError {
        match error.kind() {
            io::ErrorKind::BrokenPipe
            | io::ErrorKind::NotConnected
            | io::ErrorKind::UnexpectedEof
            | io::ErrorKind::ConnectionReset => {
                self.closed = true;
                WireError::Closed
            }
            _ => WireError::Malformed("the connection failed"),
        }
    }
}

impl Transport for PipeTransport {
    fn send(&mut self, batch: Vec<u8>) -> Result<(), WireError> {
        if self.closed {
            return Err(WireError::Closed);
        }
        if batch.len() > MAX_BATCH_BYTES {
            return Err(WireError::OverLimit("batch size"));
        }
        let len = u32::try_from(batch.len()).map_err(|_| WireError::Overflow)?;
        let mut frame = Vec::with_capacity(LENGTH + batch.len());
        frame.extend_from_slice(&len.to_le_bytes());
        frame.extend_from_slice(&batch);
        (&self.stream)
            .write_all(&frame)
            .map_err(|error| self.failed(&error))
    }

    fn recv(&mut self) -> Result<Option<Vec<u8>>, WireError> {
        if self.closed {
            return Err(WireError::Closed);
        }
        if !self.stream.ready().map_err(|error| self.failed(&error))? {
            return Ok(None);
        }
        let mut length = [0_u8; LENGTH];
        (&self.stream)
            .read_exact(&mut length)
            .map_err(|error| self.failed(&error))?;
        let len = usize::try_from(u32::from_le_bytes(length)).map_err(|_| WireError::Overflow)?;
        if len > MAX_BATCH_BYTES {
            return Err(WireError::OverLimit("batch size"));
        }
        let mut batch = vec![0_u8; len];
        (&self.stream)
            .read_exact(&mut batch)
            .map_err(|error| self.failed(&error))?;
        Ok(Some(batch))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_and_large_batches_arrive_whole_and_in_order() {
        let (a, b) = tilo_socket::link::pair().expect("a pair");
        let mut window = PipeTransport::new(b);
        assert_eq!(window.recv(), Ok(None), "nothing sent yet");
        let small = vec![7_u8; 100];
        let large: Vec<u8> = (0..200_000_u32).map(|i| (i % 251) as u8).collect();
        let sent = (small.clone(), large.clone());
        // A batch larger than the pipe's buffer is written only as it is read.
        let app = std::thread::spawn(move || {
            let mut app = PipeTransport::new(a);
            app.send(sent.0).expect("sends");
            app.send(sent.1).expect("sends");
            app
        });
        let mut received = Vec::new();
        while received.len() < 2 {
            tilo_socket::link::wait(&[window.stream()], &[], None).expect("waits");
            received.extend(window.recv().expect("reads"));
        }
        assert_eq!(received, [small, large]);
        assert_eq!(window.recv(), Ok(None));
        drop(app.join().expect("the app's end"));
        assert_eq!(window.recv(), Err(WireError::Closed), "the app went away");
        assert_eq!(window.recv(), Err(WireError::Closed), "and stays gone");
        assert_eq!(
            window.send(vec![1]),
            Err(WireError::Closed),
            "nothing reaches it"
        );
    }
}
