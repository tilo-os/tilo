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
    ///
    /// # Errors
    /// Returns an error if cancelled, poisoned or unable to clone the connection.
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
    ///
    /// # Errors
    /// Returns the thread creation error.
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
    ///
    /// # Errors
    /// Returns `Disconnected` if the worker ended without an answer.
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

    /// Duplicate the connection for a reader that observes the peer's lifetime.
    /// The duplicate does not acquire responsibility for ending the session.
    ///
    /// # Errors
    /// Returns the descriptor duplication error.
    #[cfg(unix)]
    pub fn clone_for_reader(&self) -> std::io::Result<std::os::unix::net::UnixStream> {
        self.socket.try_clone()
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
    use std::time::Duration;

    #[test]
    fn cancellation_before_connection_registration_prevents_a_request() {
        let (mut connection, mut peer) = UnixStream::pair().expect("connection");
        peer.set_read_timeout(Some(Duration::from_secs(5)))
            .expect("deadline");
        let (release, delayed_discovery) = mpsc::channel();
        let (observed, observation) = mpsc::channel();
        let pending = PendingResponse::start(
            "delayed-discovery",
            move |cancellation| {
                delayed_discovery
                    .recv_timeout(Duration::from_secs(5))
                    .expect("discovery gate");
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
        assert!(
            observation
                .recv_timeout(Duration::from_secs(5))
                .expect("worker finished")
        );
        assert_eq!(
            peer.read(&mut [0]).expect("peer closed"),
            0,
            "a cancelled worker must never send the request"
        );
    }
}
