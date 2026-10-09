//! Sleep until an input-method connection has something to read.
//!
//! An input method shares its Wayland client between the thread that follows
//! focus and the thread that commits typed text, behind a lock, so neither
//! can block inside the client. [`ConnectionWait`] waits on a duplicate of the
//! connection's socket instead, outside the lock, and on a [`Nudge`] from the
//! committing thread: that thread's roundtrips read the socket themselves and
//! can leave events queued in the client while the socket reads empty.

use std::cell::Cell;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;

/// Why [`ConnectionWait::wait`] returned.
#[derive(Debug, PartialEq, Eq)]
pub enum Woken {
    /// The socket is readable, or the other thread read it and nudged.
    Ready,
    /// The compositor hung up or the [`Nudge`] was dropped: stop waiting.
    Closed,
}

/// Where a focus-following thread sleeps between batches of events.
pub struct ConnectionWait {
    socket: OwnedFd,
    nudged: UnixStream,
    /// The socket hung up; the read after it has been allowed once.
    hung_up: Cell<bool>,
}

/// The committing thread's end of a [`ConnectionWait`]. Dropping it ends the
/// wait.
pub struct Nudge(UnixStream);

impl Nudge {
    /// Wake the waiting thread to collect events this thread read.
    pub fn nudge(&self) {
        // A full buffer already holds a pending nudge, which is all one needs.
        let _ = (&self.0).write(&[1]);
    }
}

impl ConnectionWait {
    /// Wait on `connection`'s socket.
    ///
    /// # Errors
    /// The socket cannot be duplicated or the nudge socket created.
    pub fn for_connection(
        connection: &wayland_client::Connection,
    ) -> std::io::Result<(Self, Nudge)> {
        Self::for_socket(connection.backend().poll_fd().try_clone_to_owned()?)
    }

    /// Wait on `socket`, a connection's descriptor this wait owns.
    ///
    /// # Errors
    /// The nudge socket cannot be created.
    pub fn for_socket(socket: OwnedFd) -> std::io::Result<(Self, Nudge)> {
        let (nudged, nudge) = UnixStream::pair()?;
        nudged.set_nonblocking(true)?;
        nudge.set_nonblocking(true)?;
        Ok((
            Self {
                socket,
                nudged,
                hung_up: Cell::new(false),
            },
            Nudge(nudge),
        ))
    }

    /// Sleep until the socket is readable or the other thread nudges.
    pub fn wait(&self) -> Woken {
        if self.hung_up.get() {
            return Woken::Closed;
        }
        let mut fds = [
            libc::pollfd {
                fd: self.socket.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: self.nudged.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        loop {
            // SAFETY: `fds` is a live array of two initialised `pollfd`s whose
            // descriptors `self` owns for the duration of the call.
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
            if ready >= 0 {
                break;
            }
            if std::io::Error::last_os_error().kind() != ErrorKind::Interrupted {
                return Woken::Closed;
            }
        }
        let [socket, nudged] = fds;
        if nudged.revents != 0 && !self.drain_nudges() {
            return Woken::Closed;
        }
        if socket.revents & libc::POLLNVAL != 0 {
            return Woken::Closed;
        }
        // A hung-up socket stays readable at end of file, so waiting on it
        // again would spin: let the caller read what the compositor sent
        // before hanging up (one last `Ready`), then stop.
        if socket.revents & (libc::POLLHUP | libc::POLLERR) != 0 {
            self.hung_up.set(true);
        }
        Woken::Ready
    }

    /// Read every pending nudge; `false` once the [`Nudge`] is gone.
    fn drain_nudges(&self) -> bool {
        let mut buffer = [0u8; 64];
        loop {
            match (&self.nudged).read(&mut buffer) {
                Ok(0) => return false,
                Ok(_) => {}
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(_) => return true,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn waiting() -> (UnixStream, ConnectionWait, Nudge) {
        let (compositor, client) = UnixStream::pair().expect("socketpair");
        let (wait, nudge) = ConnectionWait::for_socket(OwnedFd::from(client)).expect("wait");
        (compositor, wait, nudge)
    }

    /// Run `wait` on a thread; fail if it is still asleep after `patience`.
    fn woken_within(wait: ConnectionWait, patience: Duration) -> (Woken, ConnectionWait) {
        let (send, receive) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            let woken = wait.wait();
            let _ = send.send(());
            (woken, wait)
        });
        let finished = receive.recv_timeout(patience).is_ok();
        assert!(finished, "the wait did not return within {patience:?}");
        thread.join().expect("waiter")
    }

    #[test]
    fn a_wait_returns_for_data_a_nudge_a_hangup_and_a_dropped_nudge() {
        let patience = Duration::from_secs(5);
        // Phase `readable_socket`.
        let (compositor, wait, nudge) = waiting();
        (&compositor).write_all(&[7]).expect("write");
        let (woken, wait) = woken_within(wait, patience);
        assert_eq!(woken, Woken::Ready);
        // Phase `nudge`: drains it, so the next wait would sleep again.
        let mut byte = [0u8; 1];
        (&wait
            .socket
            .try_clone()
            .map(UnixStream::from)
            .expect("socket"))
            .read_exact(&mut byte)
            .expect("drain the data");
        nudge.nudge();
        nudge.nudge();
        let (woken, wait) = woken_within(wait, patience);
        assert_eq!(woken, Woken::Ready);
        assert!(wait.drain_nudges(), "both nudges were drained at once");
        // Phase `hangup_reads_once_more_then_closes`.
        drop(compositor);
        let (woken, wait) = woken_within(wait, patience);
        assert_eq!(woken, Woken::Ready, "one last read after a hangup");
        assert_eq!(wait.wait(), Woken::Closed);
        // Phase `dropped_nudge_closes`.
        let (_compositor, wait, nudge) = waiting();
        drop(nudge);
        assert_eq!(woken_within(wait, patience).0, Woken::Closed);
    }
}
