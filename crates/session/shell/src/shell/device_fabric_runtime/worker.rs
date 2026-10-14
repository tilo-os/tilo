//! The device fabric's pump, on a thread of its own.
//!
//! [`super::DeviceFabricRuntime::pump`] talks to the network, the disk and
//! the kernel: it waits on locks other threads hold through their fsyncs,
//! clones every device snapshot several times, and scans network interfaces
//! with ioctls that wait on the kernel's rtnl lock. On the compositor thread
//! that cost tens to hundreds of milliseconds a turn, and once 56 seconds,
//! during which no key, click or frame got through. The compositor now only
//! hands the pump the turn's local facts ([`FabricWorker::tick`], which never
//! waits) and picks up what it produced through the fabric's own queues.

use std::sync::{Arc, Condvar, Mutex, PoisonError};

/// One run of whatever the worker pumps, given the latest facts.
pub(crate) trait FabricPump: Send + 'static {
    /// What the compositor knows at the turn it asks for a run.
    type Facts: Send + 'static;

    fn pump(&mut self, facts: Self::Facts);
}

struct Slot<F> {
    /// The newest facts not yet pumped: a run that starts takes them, and
    /// facts from several turns that arrive during one run become one run
    /// with the latest of them.
    pending: Option<F>,
    closed: bool,
}

struct Shared<F> {
    slot: Mutex<Slot<F>>,
    ready: Condvar,
}

/// The thread running a [`FabricPump`]. Dropping it lets the thread finish
/// its current run and end; nothing waits for that.
pub(crate) struct FabricWorker<F> {
    shared: Arc<Shared<F>>,
}

impl<F: Send + 'static> FabricWorker<F> {
    /// Start `pump` on a thread named `name`. `after_run` follows each run,
    /// on that thread: it is how the compositor learns there is output to
    /// pick up.
    pub(crate) fn spawn<P>(
        name: &str,
        mut pump: P,
        after_run: impl Fn() + Send + 'static,
    ) -> std::io::Result<Self>
    where
        P: FabricPump<Facts = F>,
    {
        let shared = Arc::new(Shared {
            slot: Mutex::new(Slot {
                pending: None,
                closed: false,
            }),
            ready: Condvar::new(),
        });
        let worker = Arc::clone(&shared);
        std::thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                // A settings write made here must not wait for the disk
                // either, exactly as on the compositor thread it came from.
                settings_store::defer_writes_on_this_thread();
                while let Some(facts) = worker.next() {
                    pump.pump(facts);
                    after_run();
                }
            })?;
        Ok(Self { shared })
    }

    /// Ask for a run with this turn's `facts`. Returns at once, whether or
    /// not a run is going on.
    pub(crate) fn tick(&self, facts: F) {
        let mut slot = self
            .shared
            .slot
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        slot.pending = Some(facts);
        drop(slot);
        self.shared.ready.notify_one();
    }
}

impl<F> Shared<F> {
    /// The facts for the next run, waiting for some; `None` once the worker
    /// was dropped.
    fn next(&self) -> Option<F> {
        let mut slot = self.slot.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if slot.closed {
                return None;
            }
            if let Some(facts) = slot.pending.take() {
                return Some(facts);
            }
            slot = self
                .ready
                .wait(slot)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

impl<F> Drop for FabricWorker<F> {
    fn drop(&mut self) {
        self.shared
            .slot
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .closed = true;
        self.shared.ready.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    /// A pump whose run takes as long as the test says: it waits for a
    /// release, and reports each run's facts and whether the release came.
    struct Slow {
        started: mpsc::Sender<u32>,
        release: mpsc::Receiver<()>,
        runs: mpsc::Sender<(u32, bool)>,
    }

    impl FabricPump for Slow {
        type Facts = u32;

        fn pump(&mut self, facts: u32) {
            let _ = self.started.send(facts);
            let released = self.release.recv_timeout(Duration::from_secs(5)).is_ok();
            let _ = self.runs.send((facts, released));
        }
    }

    #[test]
    fn a_turn_never_waits_for_a_slow_run_and_late_facts_coalesce() {
        let (started_tx, started) = mpsc::channel();
        let (release_tx, release) = mpsc::channel();
        let (runs_tx, runs) = mpsc::channel();
        let (after_tx, after) = mpsc::channel();
        let worker = FabricWorker::spawn(
            "fabric-test",
            Slow {
                started: started_tx,
                release,
                runs: runs_tx,
            },
            move || {
                let _ = after_tx.send(());
            },
        )
        .expect("spawn the worker");

        // The first run is now stuck in its slow call; three more turns ask
        // for runs meanwhile. Were `tick` to pump in place, the first one
        // would sit out the pump's whole wait and report it unreleased.
        worker.tick(1);
        assert_eq!(started.recv_timeout(Duration::from_secs(10)), Ok(1));
        worker.tick(2);
        worker.tick(3);
        worker.tick(4);
        release_tx.send(()).unwrap();
        assert_eq!(
            runs.recv_timeout(Duration::from_secs(10)),
            Ok((1, true)),
            "the turns returned while the first run still waited"
        );
        after.recv_timeout(Duration::from_secs(10)).unwrap();

        // The turns that came during that run become one run, with the
        // latest facts.
        release_tx.send(()).unwrap();
        assert_eq!(runs.recv_timeout(Duration::from_secs(10)), Ok((4, true)));
        after.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(
            runs.recv_timeout(Duration::from_millis(100)).is_err(),
            "facts already pumped are not pumped again"
        );
    }
}
