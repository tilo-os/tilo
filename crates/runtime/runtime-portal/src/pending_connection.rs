//! Cancellable background requests whose connection is a session lifetime.

use std::sync::{Arc, Mutex, mpsc};

#[derive(Default)]
struct Lifetime {
    cancelled: bool,
    #[cfg(unix)]
    socket: Option<std::os::unix::net::UnixStream>,
}

/// Cancellation can precede discovery, request IO or descriptor handoff.
#[derive(Clone, Default)]
pub struct Cancellation(Arc<Mutex<Lifetime>>);

impl Cancellation {
    /// Register the request connection before sending anything on it.
    #[cfg(unix)]
    pub fn track(&self, socket: &std::os::unix::net::UnixStream) -> std::io::Result<()> {
        let mut guard = self
            .0
            .lock()
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        if guard.cancelled {
            return Err(std::io::Error::other("session open cancelled"));
        }
        guard.socket = Some(socket.try_clone()?);
        drop(guard);
        Ok(())
    }

    fn detach(&self) {
        if let Ok(mut guard) = self.0.lock() {
            #[cfg(unix)]
            {
                guard.socket = None;
            }
            guard.cancelled = true;
        }
    }

    fn cancel(&self) {
        if let Ok(mut guard) = self.0.lock() {
            guard.cancelled = true;
            #[cfg(unix)]
            if let Some(socket) = guard.socket.take() {
                let _ = socket.shutdown(std::net::Shutdown::Both);
            }
        }
    }
}

/// A request remains owned here until its answer is accepted by the caller.
pub struct PendingResponse<T> {
    cancellation: Cancellation,
    answer: mpsc::Receiver<T>,
    worker: std::thread::JoinHandle<()>,
}

impl<T: Send + 'static> PendingResponse<T> {
    /// Start a request away from the host's event thread.
    pub fn start(
        name: &str,
        work: impl FnOnce(Cancellation) -> T + Send + 'static,
        notify: impl FnOnce() + Send + 'static,
    ) -> std::io::Result<Self> {
        let cancellation = Cancellation::default();
        let worker_cancellation = cancellation.clone();
        let (send, answer) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                // A dropped receiver disposes of late descriptors and capabilities.
                let _ = send.send(work(worker_cancellation));
                notify();
            })?;
        Ok(Self {
            cancellation,
            answer,
            worker,
        })
    }

    /// Whether the worker has finished, including publishing its answer and wake.
    /// This does not consume or accept the queued answer.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.worker.is_finished()
    }

    /// Transfer the answer's lifetime to its caller without blocking.
    pub fn take_answer(&self) -> Result<T, mpsc::TryRecvError> {
        let answer = self.answer.try_recv()?;
        self.cancellation.detach();
        Ok(answer)
    }
}

impl<T> Drop for PendingResponse<T> {
    fn drop(&mut self) {
        self.cancellation.cancel();
    }
}

/// Owning this capability ends the accepted session when its UI closes.
/// Shutdown also releases a supervisor blocked on a reader's socket clone.
pub struct SessionLifetime {
    #[cfg(unix)]
    socket: std::os::unix::net::UnixStream,
}

impl SessionLifetime {
    #[cfg(unix)]
    #[must_use]
    pub fn new(socket: std::os::unix::net::UnixStream) -> Self {
        Self { socket }
    }

    /// The lifetime a supervisor handed over as a descriptor, once it is
    /// checked to be a Unix stream socket.
    ///
    /// Its peer may have closed already (the guest ended at once): that is
    /// the lifetime's answer, read like any other end of the session, so the
    /// socket's own address and type are checked, never its peer's, which
    /// macOS stops answering for once the peer is gone.
    #[cfg(unix)]
    pub fn adopt(descriptor: std::os::fd::OwnedFd) -> std::io::Result<Self> {
        use std::os::fd::AsRawFd;
        let socket = std::os::unix::net::UnixStream::from(descriptor);
        socket.local_addr()?;
        let mut kind: libc::c_int = 0;
        let mut size = libc::socklen_t::try_from(std::mem::size_of_val(&kind))
            .map_err(std::io::Error::other)?;
        // SAFETY: a live descriptor and an output buffer of the size passed.
        if unsafe {
            libc::getsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_TYPE,
                (&raw mut kind).cast(),
                &raw mut size,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        if kind == libc::SOCK_STREAM {
            Ok(Self { socket })
        } else {
            Err(std::io::Error::other(
                "a session lifetime is a stream socket",
            ))
        }
    }

    /// Duplicate the connection for a reader that observes the peer's lifetime.
    /// The duplicate does not acquire responsibility for ending the session.
    ///
    /// # Errors
    /// Returns the descriptor duplication error, and
    /// [`std::io::ErrorKind::Unsupported`] where leases are not sockets.
    #[cfg(unix)]
    pub fn clone_for_reader(&self) -> std::io::Result<ReaderLifetime> {
        self.socket
            .try_clone()
            .map(|socket| ReaderLifetime { socket })
    }

    /// Where leases are not sockets there is nothing to duplicate.
    ///
    /// # Errors
    /// Always [`std::io::ErrorKind::Unsupported`].
    #[cfg(not(unix))]
    pub fn clone_for_reader(&self) -> std::io::Result<ReaderLifetime> {
        Err(std::io::ErrorKind::Unsupported.into())
    }
}

/// A reader's duplicate of a [`SessionLifetime`] connection; it only observes
/// whether the peer still holds the session open.
pub struct ReaderLifetime {
    #[cfg(unix)]
    socket: std::os::unix::net::UnixStream,
}

#[cfg(unix)]
impl ReaderLifetime {
    /// Whether the peer still holds the connection open. The probe peeks
    /// without blocking and without changing the descriptor's flags, which it
    /// shares with the owning [`SessionLifetime`].
    #[must_use]
    pub fn is_open(&self) -> bool {
        use std::os::fd::AsRawFd as _;
        let mut byte = 0u8;
        // SAFETY: `byte` is valid writable storage for the one byte requested
        // and the descriptor stays open for the duration of the call.
        let received = unsafe {
            libc::recv(
                self.socket.as_raw_fd(),
                (&raw mut byte).cast::<libc::c_void>(),
                1,
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            )
        };
        if received >= 0 {
            return received > 0;
        }
        matches!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EAGAIN | libc::EINTR)
        )
    }
}

/// Where a lease stands for a reader, when its peer may say how the session
/// ended before closing it (a relay session's `end_report`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LeaseState {
    Open,
    Closed,
    /// The peer said how the session ended (one line, without its newline)
    /// and closes the lease.
    Reported(Vec<u8>),
}

/// The longest end report a reader takes.
#[cfg(unix)]
const MAX_END_REPORT: usize = 4096;

#[cfg(unix)]
impl ReaderLifetime {
    /// [`Self::is_open`] for a lease whose peer may report how the session
    /// ended before it closes: a report is read (once; afterwards the lease
    /// reads as closed), never taken for a peer still holding it open.
    #[must_use]
    pub fn state(&self) -> LeaseState {
        use std::os::fd::AsRawFd as _;
        let mut line = Vec::new();
        let mut chunk = [0u8; 512];
        loop {
            // SAFETY: `chunk` is valid writable storage for its length and the
            // descriptor stays open for the duration of the call.
            let received = unsafe {
                libc::recv(
                    self.socket.as_raw_fd(),
                    chunk.as_mut_ptr().cast::<libc::c_void>(),
                    chunk.len(),
                    libc::MSG_DONTWAIT,
                )
            };
            match usize::try_from(received) {
                Ok(0) => break,
                Ok(n) => {
                    line.extend_from_slice(chunk.get(..n).unwrap_or_default());
                    if line.contains(&b'\n') || line.len() >= MAX_END_REPORT {
                        break;
                    }
                }
                Err(_) => {
                    let error = std::io::Error::last_os_error().raw_os_error();
                    if line.is_empty() && matches!(error, Some(libc::EAGAIN | libc::EINTR)) {
                        return LeaseState::Open;
                    }
                    break;
                }
            }
        }
        if line.is_empty() {
            return LeaseState::Closed;
        }
        let end = line
            .iter()
            .position(|byte| *byte == b'\n')
            .unwrap_or(line.len());
        line.truncate(end.min(MAX_END_REPORT));
        LeaseState::Reported(line)
    }
}

/// Where leases are not sockets no reader holds one; this is only for the
/// type to exist on every target.
#[cfg(not(unix))]
impl ReaderLifetime {
    /// A lease that is not there reads as closed.
    #[must_use]
    pub fn state(&self) -> LeaseState {
        LeaseState::Closed
    }
}

impl Drop for SessionLifetime {
    fn drop(&mut self) {
        #[cfg(unix)]
        let _ = self.socket.shutdown(std::net::Shutdown::Both);
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    #[test]
    fn a_reader_takes_the_end_a_peer_reports_and_then_sees_it_closed() {
        let (ours, peer) = UnixStream::pair().expect("socket pair");
        let lifetime = SessionLifetime::new(ours);
        let reader = lifetime.clone_for_reader().expect("reader duplicate");
        assert_eq!(reader.state(), LeaseState::Open);
        (&peer)
            .write_all(b"{\"ended\":\"lost\"}\n")
            .expect("peer reports");
        peer.shutdown(std::net::Shutdown::Both)
            .expect("peer closes");
        assert_eq!(
            reader.state(),
            LeaseState::Reported(b"{\"ended\":\"lost\"}".to_vec())
        );
        assert_eq!(reader.state(), LeaseState::Closed);

        let (ours, peer) = UnixStream::pair().expect("socket pair");
        let lifetime = SessionLifetime::new(ours);
        let reader = lifetime.clone_for_reader().expect("reader duplicate");
        drop(peer);
        assert_eq!(reader.state(), LeaseState::Closed, "closed without a word");
    }

    #[test]
    fn reader_lifetime_sees_the_peer_close_its_end() {
        let (ours, peer) = UnixStream::pair().expect("socket pair");
        let lifetime = SessionLifetime::new(ours);
        let reader = lifetime.clone_for_reader().expect("reader duplicate");
        assert!(reader.is_open());
        peer.shutdown(std::net::Shutdown::Both)
            .expect("peer closes");
        assert!(!reader.is_open());
    }

    #[test]
    fn cancellation_before_connection_registration_prevents_a_request() {
        let (mut connection, mut peer) = UnixStream::pair().expect("connection");
        peer.set_read_timeout(Some(tilo_test_env::hang_bound()))
            .expect("deadline");
        let (release, delayed_discovery) = mpsc::channel();
        let (observed, observation) = mpsc::channel();
        let pending = PendingResponse::start(
            "delayed-discovery",
            move |cancellation| {
                tilo_test_env::recv("the discovery gate", &delayed_discovery);
                let result = cancellation
                    .track(&connection)
                    .and_then(|()| connection.write_all(b"request"));
                observed
                    .send(result.is_err())
                    .expect("cancellation observation");
                result
            },
            || {},
        )
        .expect("worker");
        drop(pending);
        release.send(()).expect("release discovery");
        assert!(tilo_test_env::recv("the worker to finish", &observation));
        assert_eq!(
            peer.read(&mut [0]).expect("peer closed"),
            0,
            "a cancelled worker must never send the request"
        );
    }
}
