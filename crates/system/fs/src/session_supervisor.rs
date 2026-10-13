//! Supervising a mounted FUSE session without polling it.
//!
//! A mount ends one of two ways: the session ends by itself (an external
//! unmount, a crash in its thread), or the daemon asks for an unmount (a
//! signal, a rebind) and then waits for the session's thread so the old
//! mount is gone before the process returns to systemd. The supervisor
//! used to learn of the first by waking every 50 ms to look: 20 wakeups a
//! second for the life of every mount. Now the session's thread
//! is joined on a watcher thread that reports its end on the same channel
//! the unmount request comes in on, and the supervisor blocks on that
//! channel with no timeout.

use std::io;
use std::sync::mpsc::{Receiver, Sender};
use std::thread::JoinHandle;

/// What the supervisor waits for.
pub enum SessionEvent {
    /// Unmount, then return once the session has ended.
    Unmount,
    /// The session's thread ended, by itself or after an unmount.
    Ended(io::Result<()>),
}

/// What [`supervise`] needs of a running session: a seam so tests can stand
/// in for the kernel.
pub trait SupervisedSession {
    /// The session's own thread, taken out to be joined elsewhere.
    fn take_thread(&mut self) -> JoinHandle<io::Result<()>>;
    /// Unmounts the filesystem; the session's thread then ends.
    ///
    /// # Errors
    /// The unmount failed; the session may still be serving.
    fn unmount(self) -> io::Result<()>;
}

impl SupervisedSession for fuser::BackgroundSession {
    fn take_thread(&mut self) -> JoinHandle<io::Result<()>> {
        // `BackgroundSession` keeps its mount private and joins `guard` in
        // `umount_and_join`: a finished stand-in takes the thread's place.
        std::mem::replace(&mut self.guard, std::thread::spawn(|| Ok(())))
    }

    fn unmount(self) -> io::Result<()> {
        self.umount_and_join()
    }
}

/// Serves until the session ends by itself or `events` asks for an
/// unmount, and returns how the session ended. `ended` is a sender of
/// `events` for the watcher thread.
///
/// # Errors
/// The session's thread ended with an error or panicked, the watcher thread
/// could not be started, or the requested unmount failed (then this returns
/// at once, as the session may still be serving).
pub fn supervise<S: SupervisedSession>(
    session: S,
    events: &Receiver<SessionEvent>,
    ended: Sender<SessionEvent>,
) -> io::Result<()> {
    supervise_watching_on(session, events, ended, "fs-fuse-watch")
}

/// [`supervise`], naming the watcher thread `watcher`.
fn supervise_watching_on<S: SupervisedSession>(
    mut session: S,
    events: &Receiver<SessionEvent>,
    ended: Sender<SessionEvent>,
    watcher: &str,
) -> io::Result<()> {
    let thread = session.take_thread();
    std::thread::Builder::new()
        .name(watcher.into())
        .spawn(move || {
            let result = thread
                .join()
                .unwrap_or_else(|_| Err(io::Error::other("filesystem background thread panicked")));
            let _ = ended.send(SessionEvent::Ended(result));
        })?;
    // The watcher holds a sender until it reports, so `recv` fails only
    // after an `Ended` it already returned.
    match events.recv() {
        Ok(SessionEvent::Ended(result)) => return result,
        Ok(SessionEvent::Unmount) | Err(_) => session.unmount()?,
    }
    loop {
        match events.recv() {
            Ok(SessionEvent::Ended(result)) => return result,
            Ok(SessionEvent::Unmount) => {}
            Err(_) => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc;

    /// A session whose thread serves until it is told to end, with
    /// `result`, by the test (ending by itself) or by an unmount.
    struct StandIn {
        thread: Option<JoinHandle<io::Result<()>>>,
        end: mpsc::Sender<()>,
    }

    impl StandIn {
        fn serving(result: fn() -> io::Result<()>) -> (Self, mpsc::Sender<()>, Arc<AtomicBool>) {
            let (end, ends) = mpsc::channel::<()>();
            let finished = Arc::new(AtomicBool::new(false));
            let done = Arc::clone(&finished);
            let thread = std::thread::spawn(move || {
                let _ = ends.recv();
                done.store(true, Ordering::SeqCst);
                result()
            });
            (
                Self {
                    thread: Some(thread),
                    end: end.clone(),
                },
                end,
                finished,
            )
        }
    }

    impl SupervisedSession for StandIn {
        fn take_thread(&mut self) -> JoinHandle<io::Result<()>> {
            self.thread.take().unwrap()
        }

        fn unmount(self) -> io::Result<()> {
            self.end.send(()).unwrap();
            Ok(())
        }
    }

    /// Runs `supervise` on its own thread, returning its result channel
    /// and the sender a mount's async side keeps.
    fn supervised(
        session: StandIn,
        name: &str,
    ) -> (mpsc::Receiver<io::Result<()>>, mpsc::Sender<SessionEvent>) {
        let (events_tx, events) = mpsc::channel();
        let ended = events_tx.clone();
        let (result_tx, result) = mpsc::channel();
        let watcher = format!("{name}-w");
        std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                let outcome = supervise_watching_on(session, &events, ended, &watcher);
                result_tx.send(outcome).unwrap();
            })
            .unwrap();
        (result, events_tx)
    }

    /// The fault the supervisor exists for: the session ends without being
    /// asked (an external unmount, a crash). It returns at once, with the
    /// session's own result.
    #[test]
    fn a_session_that_ends_by_itself_is_noticed_at_once() {
        let (session, end, _) = StandIn::serving(|| Ok(()));
        let (result, _keep) = supervised(session, "fs-test-ends");
        assert!(result.try_recv().is_err(), "returned while serving");
        end.send(()).unwrap();
        result
            .recv_timeout(tilo_test_env::hang_bound())
            .expect("a session that ended was not noticed")
            .unwrap();

        let (session, end, _) = StandIn::serving(|| Err(io::Error::other("device gone")));
        let (result, _keep) = supervised(session, "fs-test-fails");
        end.send(()).unwrap();
        let error = result
            .recv_timeout(tilo_test_env::hang_bound())
            .unwrap()
            .unwrap_err();
        assert_eq!(error.to_string(), "device gone");
    }

    /// An unmount request unmounts and returns only once the session's
    /// thread has ended, so the old mount is gone before a rebind.
    #[test]
    fn an_unmount_request_waits_for_the_session_to_end() {
        let (session, _end, finished) = StandIn::serving(|| Ok(()));
        let (result, events) = supervised(session, "fs-test-unmount");
        events.send(SessionEvent::Unmount).unwrap();
        result
            .recv_timeout(tilo_test_env::hang_bound())
            .expect("an unmount request was not served")
            .unwrap();
        assert!(
            finished.load(Ordering::SeqCst),
            "returned before the session's thread ended"
        );
    }

    /// Neither the supervisor nor the watcher is scheduled while the
    /// session serves (the loop this replaces woke 20 times a second).
    #[cfg(target_os = "linux")] // the threads' wakeups are read from /proc
    #[test]
    fn an_idle_supervisor_never_wakes() {
        let (session, end, _) = StandIn::serving(|| Ok(()));
        let ((result, _keep), supervisor) =
            tilo_test_env::idle::spawned_thread("fs-test-idle", || {
                supervised(session, "fs-test-idle")
            });
        // The supervisor starts its watcher once it runs.
        let started = std::time::Instant::now();
        while !std::fs::read_dir("/proc/self/task").unwrap().any(|task| {
            let comm = task.unwrap().path().join("comm");
            std::fs::read_to_string(comm).is_ok_and(|name| name.trim() == "fs-test-idle-w")
        }) {
            assert!(
                started.elapsed() < tilo_test_env::hang_bound(),
                "no watcher thread"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let watcher = tilo_test_env::idle::named_thread("fs-test-idle-w");
        tilo_test_env::idle::assert_stays_asleep(&supervisor, "FUSE session supervisor");
        tilo_test_env::idle::assert_stays_asleep(&watcher, "FUSE session watcher");
        end.send(()).unwrap();
        result
            .recv_timeout(tilo_test_env::hang_bound())
            .unwrap()
            .unwrap();
    }
}
