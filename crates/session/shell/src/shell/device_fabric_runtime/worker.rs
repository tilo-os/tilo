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
//!
//! # Lock order
//!
//! This thread and the compositor's share state behind locks. Every path
//! takes them in this order, outermost first, and only ever releases out of
//! it; a new path that needs two of them takes them in this order too:
//!
//! 1. `ShellNetwork::event_rx` (the loop's event drain in `tick` only).
//! 2. Relay-room and native-enrollment state, then the joined room's
//!    `owner.mutation`.
//! 3. `ShellNetwork::room_push`.
//! 4. `personal_continuity_fingerprint`, then `personal_continuity_counter`,
//!    then `personal_continuity`.
//! 5. `ShellNetwork::replica`.
//! 6. The workspace tree. Taken inside it: the native store writer, owner
//!    authority, `personal_workspaces`, `next_sync_pull`, the continuation
//!    registry (`personal_continuity_registry::global_registry`), and last
//!    the tree guard's own holder and policy locks.
//! 7. Input-control operations, then input declines.
//! 8. Subsystem roots, never held together: media-handoff statuses, the
//!    gamepad manager, speaker-group routes (then remote audio, then the
//!    speaker core and clock state), remote capture (then its adapter and
//!    broker), the realtime adapter, MPRIS.
//! 9. `authenticated_personal_routes`.
//! 10. `spatial_input_state`, then peer departures.
//! 11. Sending: `personal_device_peers`, then registered home devices; then
//!     `joined_topics` on its own.
//! 12. `device_fabric`, then the state writer's queue.
//! 13. Leaves, under which nothing else is taken: Nearby drop state, the
//!     Nearby store thread's follow-up and wake slots, the selection store,
//!     the settings store, `KnownContents`, the continuity durability ledger
//!     (its obligations run after it is released), the wake state, queued
//!     personal fabric actions, the state writer's queue, and this worker's
//!     slot. The Nearby transfer store itself is taken only on its own
//!     thread and by staging threads; this thread and the loop queue work
//!     to it and never take it.
//!
//! This thread never takes 1 to 6: the pump reaches neither the tree, the
//! replica, the continuity ledger nor the event drain. What the two threads
//! share sits in 7 to 13, so a cycle needs one of them to break the order.
//!
//! The order rules out deadlock, not waiting. A lock the loop takes may not
//! be held here across the disk, a process or the kernel. So: Nearby store
//! work, with its fsyncs, runs on the store's own thread, and what follows
//! it comes back here as a follow-up; a remote audio capture starts outside
//! the route state's lock; the interface scan for wake routes runs outside
//! the wake state's lock; and this device's own fabric snapshot is read,
//! edited and written under one hold of `device_fabric`, so an edit on the
//! loop never builds on a snapshot this thread replaced meanwhile.

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
    /// Start a pump on a thread named `name`, made there by `make`: making
    /// one may read settings and bind sockets, which the compositor thread
    /// must not wait for. Facts asked for before it is made wait for it.
    /// `after_run` follows each run, on that thread: it is how the
    /// compositor learns there is output to pick up.
    pub(crate) fn spawn<P>(
        name: &str,
        make: impl FnOnce() -> P + Send + 'static,
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
                let mut pump = make();
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

    use super::*;
    use tilo_test_env::recv;

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
            let released = self
                .release
                .recv_timeout(tilo_test_env::hang_bound())
                .is_ok();
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
            move || Slow {
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
        assert_eq!(recv("the first run to start", &started), 1);
        worker.tick(2);
        worker.tick(3);
        worker.tick(4);
        release_tx.send(()).unwrap();
        assert_eq!(
            recv("the first run", &runs),
            (1, true),
            "the turns returned while the first run still waited"
        );
        recv("the first run's follow-up", &after);

        release_tx.send(()).unwrap();
        assert_eq!(recv("the coalesced run", &runs), (4, true));
        recv("the coalesced run's follow-up", &after);

        // Facts already pumped are not pumped again: the next run is the
        // next turn's, and nothing ran in between.
        worker.tick(5);
        release_tx.send(()).unwrap();
        assert_eq!(recv("the next turn's run", &runs), (5, true));
        assert_eq!(
            started.try_iter().collect::<Vec<_>>(),
            [4, 5],
            "facts already pumped are not pumped again"
        );
    }
}
