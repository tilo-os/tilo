//! Subscriptions, and the one thread that reads for all of them.
//!
//! The thread exists only while someone subscribes: the first subscription
//! starts it, and it returns once the last one is dropped. It sleeps until
//! the earliest subscription is due, takes one reading for every
//! subscription due then (with processes only if one of them asked for
//! them, and command lines only if one of them may see them), and hands each
//! its cut of that reading. Nobody subscribed costs nothing: no thread, no
//! timer, no read.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::apps::EndPolicy;
use crate::collect::Collector;
use crate::end::{PidfdSignaller, Signaller};
use crate::probe::{KernelProbe, Probe, Want};
use crate::protocol::{Grants, MAX_INTERVAL_MS, MIN_INTERVAL_MS, Refusal, Snapshot, Storage};
use crate::storage::{MachineStorage, StorageSource};

/// Where a subscription's snapshots go. Called on the hub's thread; it must
/// not block.
pub type Sink = Box<dyn FnMut(Snapshot) + Send>;

/// Where a storage reading goes. Called on the storage thread; it must not
/// block.
pub type StorageAnswer = Box<dyn FnOnce(Result<Storage, Refusal>) + Send>;

/// The thread that reads the machine's storage.
pub const STORAGE_THREAD: &str = "tilo-system-stats-storage";

/// Who waits for a storage reading, and whether one is being taken.
#[derive(Default)]
struct StorageQueue {
    waiting: Vec<StorageAnswer>,
    reading: bool,
}

/// What a subscription asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spec {
    pub interval: Duration,
    /// The app list and process list too.
    pub processes: bool,
}

impl Spec {
    /// `interval_ms`, held to the bounds the service serves.
    #[must_use]
    pub fn every(interval_ms: u32, processes: bool) -> Self {
        Self {
            interval: Duration::from_millis(u64::from(
                interval_ms.clamp(MIN_INTERVAL_MS, MAX_INTERVAL_MS),
            )),
            processes,
        }
    }
}

struct Entry {
    id: u64,
    spec: Spec,
    grants: Grants,
    due: Instant,
    sink: Sink,
}

#[derive(Default)]
struct State {
    entries: Vec<Entry>,
    next_id: u64,
    running: bool,
    /// Bumped by every change, so a sleeping thread re-plans.
    generation: u64,
    /// Subscriptions dropped while the thread was serving them: their
    /// entries are out of `entries` then, and must not come back.
    dropped: Vec<u64>,
    /// Grants changed while the thread was serving the subscription.
    regranted: Vec<(u64, Grants)>,
    /// The reading thread, while it runs.
    thread: Option<std::thread::JoinHandle<()>>,
}

struct Inner {
    state: Mutex<State>,
    changed: Condvar,
    collector: Mutex<Collector>,
    signaller: Box<dyn Signaller + Sync>,
    storage: Box<dyn StorageSource>,
    storage_queue: Mutex<StorageQueue>,
}

/// The session's one source of system readings.
#[derive(Clone)]
pub struct Hub {
    inner: Arc<Inner>,
}

/// A live subscription; dropping it unsubscribes.
pub struct Subscription {
    hub: Hub,
    id: u64,
}

impl Subscription {
    /// Change what this subscription may see (a grant was given).
    pub fn set_grants(&self, grants: Grants) {
        let mut state = self.hub.lock();
        if let Some(entry) = state.entries.iter_mut().find(|e| e.id == self.id) {
            entry.grants = grants;
        } else {
            // Out being served: take effect from the next reading.
            state.regranted.push((self.id, grants));
        }
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let mut state = self.hub.lock();
        let before = state.entries.len();
        state.entries.retain(|entry| entry.id != self.id);
        if state.entries.len() == before {
            state.dropped.push(self.id);
        }
        state.generation = state.generation.wrapping_add(1);
        drop(state);
        self.hub.inner.changed.notify_all();
    }
}

impl Hub {
    /// The running machine, ended through pidfds, for the person running
    /// this process.
    #[must_use]
    pub fn kernel() -> Self {
        Self::new(
            Box::new(KernelProbe::new()),
            EndPolicy::current(),
            Box::new(PidfdSignaller),
        )
    }

    /// The session's hub: one per process, shared by the service and the
    /// shell's own widgets.
    pub fn shared() -> &'static Self {
        static SHARED: OnceLock<Hub> = OnceLock::new();
        SHARED.get_or_init(Self::kernel)
    }

    /// Readings from `probe`, ending through `signaller` under `policy`,
    /// and this machine's storage.
    #[must_use]
    pub fn new(
        probe: Box<dyn Probe>,
        policy: EndPolicy,
        signaller: Box<dyn Signaller + Sync>,
    ) -> Self {
        Self::with_storage(probe, policy, signaller, Box::new(MachineStorage))
    }

    /// [`Self::new`], with storage described by `storage`.
    #[must_use]
    pub fn with_storage(
        probe: Box<dyn Probe>,
        policy: EndPolicy,
        signaller: Box<dyn Signaller + Sync>,
        storage: Box<dyn StorageSource>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State::default()),
                changed: Condvar::new(),
                collector: Mutex::new(Collector::new(probe, policy)),
                signaller,
                storage,
                storage_queue: Mutex::new(StorageQueue::default()),
            }),
        }
    }

    /// Describe the machine's storage to `answer` once it is read. The
    /// reading is taken on the hub's storage thread ([`STORAGE_THREAD`]),
    /// never on the asker's: a slow device (a busy udisks2, a drive waking
    /// up) answers late and holds up neither the asking connection nor the
    /// process that serves it. Askers who come while a reading is being
    /// taken share the next one, so each sees storage as it was after they
    /// asked. The thread exists only while someone waits.
    pub fn read_storage(&self, answer: StorageAnswer) {
        let mut queue = self.storage_queue();
        queue.waiting.push(answer);
        if queue.reading {
            return;
        }
        queue.reading = true;
        drop(queue);
        let hub = self.clone();
        let spawned = std::thread::Builder::new()
            .name(STORAGE_THREAD.into())
            .spawn(move || hub.serve_storage());
        if let Err(error) = spawned {
            let waiting = {
                let mut queue = self.storage_queue();
                queue.reading = false;
                std::mem::take(&mut queue.waiting)
            };
            for answer in waiting {
                answer(Err(Refusal::Failed {
                    detail: format!("storage could not be read: {error}"),
                }));
            }
        }
    }

    fn serve_storage(&self) {
        loop {
            let waiting = {
                let mut queue = self.storage_queue();
                if queue.waiting.is_empty() {
                    queue.reading = false;
                    return;
                }
                std::mem::take(&mut queue.waiting)
            };
            let reading = self.inner.storage.read();
            for answer in waiting {
                answer(reading.clone());
            }
        }
    }

    fn storage_queue(&self) -> MutexGuard<'_, StorageQueue> {
        self.inner
            .storage_queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn collector(&self) -> MutexGuard<'_, Collector> {
        self.inner
            .collector
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Push a cut of every reading to `sink` per `spec` and `grants`. The
    /// first snapshot comes at once (with no rates yet when no one else was
    /// subscribed); then one each interval.
    pub fn subscribe(&self, spec: Spec, grants: Grants, sink: Sink) -> Subscription {
        let mut state = self.lock();
        let id = state.next_id;
        state.next_id = state.next_id.wrapping_add(1);
        state.entries.push(Entry {
            id,
            spec,
            grants,
            due: Instant::now(),
            sink,
        });
        state.generation = state.generation.wrapping_add(1);
        let start = !state.running;
        state.running = true;
        drop(state);
        if start {
            let hub = self.clone();
            let spawned = std::thread::Builder::new()
                .name("tilo-system-stats".into())
                .spawn(move || hub.run());
            let mut state = self.lock();
            match spawned {
                Ok(thread) => state.thread = Some(thread),
                Err(error) => {
                    tracing::warn!(%error, "system information thread could not start");
                    state.running = false;
                }
            }
        } else {
            self.inner.changed.notify_all();
        }
        Subscription {
            hub: self.clone(),
            id,
        }
    }

    /// Whether the reading thread is running: there are subscribers.
    #[must_use]
    pub fn is_reading(&self) -> bool {
        self.lock().running
    }

    /// Wait for the reading thread to finish once nobody subscribes; at
    /// once when it is not running.
    pub fn wait_until_idle(&self) {
        let thread = {
            let mut state = self.lock();
            if state.entries.is_empty() {
                state.thread.take()
            } else {
                None
            }
        };
        if let Some(thread) = thread {
            let _ = thread.join();
        }
    }

    /// End the process `pid` of the lifetime `started_at`.
    ///
    /// # Errors
    /// Why it was not ended.
    pub fn end_process(&self, pid: u32, started_at: u64, force: bool) -> Result<(), Refusal> {
        let mut collector = self.collector();
        let policy = collector.policy().clone();
        crate::end::end_process(
            collector.probe_mut(),
            &policy,
            self.inner.signaller.as_ref(),
            (pid, started_at),
            force,
        )
    }

    /// End every process of `app` the person may end.
    ///
    /// # Errors
    /// Why none was ended.
    pub fn end_app(&self, app: &str, force: bool) -> Result<u32, Refusal> {
        let mut collector = self.collector();
        let policy = collector.policy().clone();
        crate::end::end_app(
            collector.probe_mut(),
            &policy,
            self.inner.signaller.as_ref(),
            app,
            force,
        )
    }

    fn run(&self) {
        loop {
            let mut state = self.lock();
            if state.entries.is_empty() {
                state.running = false;
                drop(state);
                // The next subscriber starts a new interval: rates across
                // the time nobody watched mean nothing.
                self.collector().reset();
                return;
            }
            let now = Instant::now();
            let next = state.entries.iter().map(|e| e.due).min().unwrap_or(now);
            if next > now {
                let generation = state.generation;
                let (guard, _) = self
                    .inner
                    .changed
                    .wait_timeout_while(state, next.saturating_duration_since(now), |s| {
                        s.generation == generation
                    })
                    .unwrap_or_else(PoisonError::into_inner);
                drop(guard);
                continue;
            }
            // Due now: take their sinks out, read without the lock held,
            // then hand the sinks back.
            let mut due: Vec<Entry> = Vec::new();
            let mut index = 0;
            while index < state.entries.len() {
                if state.entries.get(index).is_some_and(|e| e.due <= now) {
                    due.push(state.entries.swap_remove(index));
                } else {
                    index = index.saturating_add(1);
                }
            }
            drop(state);
            let want = Want {
                system: true,
                processes: due.iter().any(|e| e.spec.processes && e.grants.processes),
                command_lines: due.iter().any(|e| {
                    e.spec.processes && e.grants.holds(crate::protocol::Grant::CommandLines)
                }),
            };
            let reading = self.collector().read(want, unix_ms());
            for entry in &mut due {
                (entry.sink)(reading.cut(entry.grants, entry.spec.processes));
                entry.due = now.checked_add(entry.spec.interval).unwrap_or(now);
            }
            let mut state = self.lock();
            let dropped = std::mem::take(&mut state.dropped);
            for (id, grants) in std::mem::take(&mut state.regranted) {
                if let Some(entry) = due.iter_mut().find(|entry| entry.id == id) {
                    entry.grants = grants;
                }
            }
            state
                .entries
                .extend(due.into_iter().filter(|entry| !dropped.contains(&entry.id)));
        }
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}
