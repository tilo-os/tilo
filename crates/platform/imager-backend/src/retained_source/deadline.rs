//! Absolute, non-renewable proof transport deadlines and shutdown-only custody.
//! This module never admits a person or source; successful IO is not authority.
use super::workspace::ControlStream;
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const PROOF_WATCH_INTERVAL: Duration = Duration::from_millis(20);

/// Shutdown-only observation; possession cannot read, duplicate or admit a proof.
pub struct ProofInterrupt {
    #[cfg(unix)]
    stream: native::Interrupt,
}
impl ProofInterrupt {
    /// Retain the exact transport solely to interrupt blocked proof IO.
    /// # Errors
    /// Returns an error if the transport cannot be duplicated or the host lacks
    /// native proof-interruption support.
    pub fn observe(stream: &ControlStream) -> io::Result<Self> {
        #[cfg(unix)]
        {
            native::Interrupt::observe(stream).map(|stream| Self { stream })
        }
        #[cfg(not(unix))]
        {
            let _ = stream;
            Err(super::Unsupported.into())
        }
    }
    pub fn interrupt(&self) {
        #[cfg(unix)]
        self.stream.interrupt();
    }
}

#[cfg(unix)]
mod native {
    use std::{io, net::Shutdown, os::unix::net::UnixStream};

    pub(super) struct Interrupt(UnixStream);
    impl Interrupt {
        pub(super) fn observe(stream: &UnixStream) -> io::Result<Self> {
            stream.try_clone().map(Self)
        }
        pub(super) fn interrupt(&self) {
            let _ = self.0.shutdown(Shutdown::Both);
        }
    }
}

struct ProofFence<'scope> {
    interrupt: Arc<ProofInterrupt>,
    done: Arc<AtomicBool>,
    watcher: Option<std::thread::ScopedJoinHandle<'scope, ()>>,
}
impl ProofFence<'_> {
    fn stop(&mut self) -> io::Result<()> {
        self.done.store(true, Ordering::Release);
        if self
            .watcher
            .take()
            .is_some_and(|watcher| watcher.join().is_err())
        {
            self.interrupt.interrupt();
            return Err(io::Error::other("Installed proof deadline watcher failed"));
        }
        Ok(())
    }
}
impl Drop for ProofFence<'_> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.interrupt.interrupt();
        }
        let _ = self.stop();
    }
}
/// Execute one proof exchange under an absolute deadline. IO progress and
/// handshake timeout resets cannot renew it; every failed exchange is shut down.
pub fn with_proof_deadline_at<T>(
    stream: &ControlStream,
    cancel: &AtomicBool,
    deadline: Instant,
    run: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    let interrupt = Arc::new(ProofInterrupt::observe(stream)?);
    std::thread::scope(|scope| {
        let done = Arc::new(AtomicBool::new(false));
        let expired = Arc::new(AtomicBool::new(false));
        let observed_cancel = Arc::new(AtomicBool::new(false));
        let watchdog_interrupt = interrupt.clone();
        let watchdog_done = done.clone();
        let watchdog_expired = expired.clone();
        let watchdog_cancel = observed_cancel.clone();
        // Arm before the first QPE byte, not after certificate admission.
        if cancel.load(Ordering::Acquire) || Instant::now() >= deadline {
            interrupt.interrupt();
            return Err(io::Error::new(
                if cancel.load(Ordering::Acquire) {
                    io::ErrorKind::Interrupted
                } else {
                    io::ErrorKind::TimedOut
                },
                "Installed proof deadline refused",
            ));
        }
        let watcher = std::thread::Builder::new()
            .name("installed-proof-deadline".into())
            .spawn_scoped(scope, move || {
                while !watchdog_done.load(Ordering::Acquire) {
                    if cancel.load(Ordering::Acquire) {
                        watchdog_cancel.store(true, Ordering::Release);
                        watchdog_interrupt.interrupt();
                        return;
                    }
                    let now = Instant::now();
                    if now >= deadline {
                        watchdog_expired.store(true, Ordering::Release);
                        watchdog_interrupt.interrupt();
                        return;
                    }
                    std::thread::sleep(
                        PROOF_WATCH_INTERVAL.min(deadline.saturating_duration_since(now)),
                    );
                }
            })
            .inspect_err(|_| {
                interrupt.interrupt();
            })?;
        let mut fence = ProofFence {
            interrupt: interrupt.clone(),
            done,
            watcher: Some(watcher),
        };
        let result = run();
        // Progress, successful decoding and the QPE timeout reset never renew
        // the absolute exchange limit. Check even if the watcher has not run.
        fence.stop()?;
        let cancelled = cancel.load(Ordering::Acquire) || observed_cancel.load(Ordering::Acquire);
        let timed_out = Instant::now() >= deadline || expired.load(Ordering::Acquire);
        if cancelled || timed_out {
            interrupt.interrupt();
            return Err(io::Error::new(
                if cancelled {
                    io::ErrorKind::Interrupted
                } else {
                    io::ErrorKind::TimedOut
                },
                "Installed proof total deadline refused",
            ));
        }
        if result.is_err() {
            interrupt.interrupt();
        }
        result
    })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{io::Read, os::unix::net::UnixStream};

    #[test]
    fn deadline_interrupts_the_actual_blocked_transport() {
        let (mut stream, mut peer) = UnixStream::pair().unwrap();
        let held = stream.try_clone().unwrap();
        // The peer never writes, so only the deadline can end the read.
        let (ended, end) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let error = with_proof_deadline_at(
                &held,
                &AtomicBool::new(false),
                Instant::now() + Duration::from_millis(50),
                || {
                    let mut byte = [0];
                    stream.read_exact(&mut byte)
                },
            )
            .unwrap_err();
            let _ = ended.send(error.kind());
        });
        assert_eq!(
            tilo_test_env::recv("the deadline to interrupt the read", &end),
            io::ErrorKind::TimedOut
        );
        peer.set_read_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        assert_eq!(peer.read(&mut [0]).unwrap(), 0);
    }

    #[test]
    fn preexisting_cancellation_shuts_down_before_running_exchange() {
        let (stream, mut peer) = UnixStream::pair().unwrap();
        let error = with_proof_deadline_at(
            &stream,
            &AtomicBool::new(true),
            Instant::now() + tilo_test_env::hang_bound(),
            || -> io::Result<()> { panic!("cancelled exchange must not run") },
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        peer.set_read_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        assert_eq!(peer.read(&mut [0]).unwrap(), 0);
    }

    #[test]
    fn exchange_error_revokes_the_exact_transport() {
        let (stream, mut peer) = UnixStream::pair().unwrap();
        let error = with_proof_deadline_at(
            &stream,
            &AtomicBool::new(false),
            Instant::now() + tilo_test_env::hang_bound(),
            || -> io::Result<()> { Err(io::ErrorKind::PermissionDenied.into()) },
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        peer.set_read_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        assert_eq!(peer.read(&mut [0]).unwrap(), 0);
    }
}
