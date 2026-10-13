//! Background work and timers, provided by the host.
//!
//! `tilo_host::background` and `ui::background` re-export this module.
//!
//! Toolkit services that must not block a frame (image decoding, file-history
//! transport calls, periodic refreshes) hand their work to a [`Background`]
//! rather than spawning threads or sleeping themselves. The host decides how
//! that work runs:
//!
//! * [`ThreadPool`] — the default when no executor is entered: a bounded pool
//!   of named threads plus one timer thread, so native behaviour needs no
//!   setup;
//! * [`Pumped`] — a queue and a virtual clock that run only when their owner
//!   pumps them, for deterministic headless tests and for hosts that drive
//!   work from their own loop;
//! * `PageExecutor` — the default in a browser page, which has one thread:
//!   jobs run from the page's microtask queue and timers are `setTimeout`s;
//! * any other [`Executor`] a host installs with [`Background::enter`].
//!
//! Like [`crate::wake::WakeScope`], the executor is captured when a service is
//! constructed: enter it around construction, and the service keeps using it
//! from any thread afterwards.
//!
//! An executor may refuse work with [`Unsupported`] — a host with no threads
//! of its own, for example. Callers must degrade (report the feature as
//! unavailable, settle a request as failed) instead of retrying forever.

use std::cell::RefCell;
use std::collections::{BinaryHeap, VecDeque};
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};

use tilo_clock::{Duration, Instant};

use crate::wake::WakeHandle;

/// One unit of background work.
pub type Job = Box<dyn FnOnce() + Send + 'static>;

/// The host refused to run background work.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("background work is not available on this host")]
pub struct Unsupported;

/// How a host runs background work and timers.
pub trait Executor: Send + Sync + 'static {
    /// Run `job` off the calling thread. `name` identifies the work in logs.
    ///
    /// Implementations must not run `job` before returning: callers may hold
    /// locks the job also takes.
    ///
    /// # Errors
    ///
    /// Returns [`Unsupported`] when this host cannot run background work.
    fn spawn(&self, name: &'static str, job: Job) -> Result<(), Unsupported>;

    /// Call [`WakeHandle::wake`] on `wake` once [`now`](Self::now) reaches
    /// `deadline`. A deadline already in the past wakes promptly.
    fn wake_at(&self, deadline: Instant, wake: WakeHandle);

    /// This executor's clock. Deadlines passed to [`wake_at`](Self::wake_at)
    /// are measured against it.
    fn now(&self) -> Instant {
        tilo_clock::now()
    }

    /// Block until background work may have made progress, or until
    /// `deadline`. For synchronous drivers only (headless capture, tests) —
    /// never call this on a live UI thread.
    fn park(&self, deadline: Instant);
}

thread_local! {
    static CURRENT: RefCell<Option<Background>> = const { RefCell::new(None) };
}

/// A cheap-to-clone handle to the executor a service was constructed under.
#[derive(Clone)]
pub struct Background(Arc<dyn Executor>);

impl std::fmt::Debug for Background {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Background").finish_non_exhaustive()
    }
}

impl Default for Background {
    fn default() -> Self {
        Self::current()
    }
}

impl Background {
    pub fn new(executor: impl Executor) -> Self {
        Self(Arc::new(executor))
    }

    /// The executor entered on this thread, or the process-wide
    /// [`ThreadPool`] when none is.
    pub fn current() -> Self {
        CURRENT
            .with(|current| current.borrow().clone())
            .unwrap_or_else(native_default)
    }

    /// Make this executor [`current`](Self::current) on this thread until the
    /// guard drops. Drop guards in reverse entry order.
    #[must_use = "the executor stays current only while the guard lives"]
    pub fn enter(&self) -> BackgroundGuard {
        let previous = CURRENT.with(|current| current.replace(Some(self.clone())));
        BackgroundGuard {
            previous,
            thread_affinity: PhantomData,
        }
    }

    /// # Errors
    ///
    /// Returns [`Unsupported`] when the executor refuses background work.
    pub fn spawn(
        &self,
        name: &'static str,
        job: impl FnOnce() + Send + 'static,
    ) -> Result<(), Unsupported> {
        self.0.spawn(name, Box::new(job))
    }

    pub fn wake_at(&self, deadline: Instant, wake: WakeHandle) {
        self.0.wake_at(deadline, wake);
    }

    pub fn wake_after(&self, delay: Duration, wake: WakeHandle) {
        let now = self.now();
        self.0.wake_at(now.checked_add(delay).unwrap_or(now), wake);
    }

    pub fn now(&self) -> Instant {
        self.0.now()
    }

    /// See [`Executor::park`].
    pub fn park(&self, deadline: Instant) {
        self.0.park(deadline);
    }
}

/// Restores the previously entered executor on drop. Thread-affine.
pub struct BackgroundGuard {
    previous: Option<Background>,
    thread_affinity: PhantomData<Rc<()>>,
}

impl Drop for BackgroundGuard {
    fn drop(&mut self) {
        let previous = self.previous.take();
        CURRENT.with(|current| {
            current.replace(previous);
        });
    }
}

fn native_default() -> Background {
    static DEFAULT: OnceLock<Background> = OnceLock::new();
    #[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
    let make = || Background::new(ThreadPool::new(ThreadPool::default_threads()));
    #[cfg(all(target_family = "wasm", target_os = "unknown"))]
    let make = || Background::new(PageExecutor);
    DEFAULT.get_or_init(make).clone()
}

/// The executor of a browser page, which has one thread and cannot block it.
///
/// Jobs run on the page thread from its microtask queue, after the caller
/// returns, so a long job delays the next frame; timers are `setTimeout`s.
/// [`Executor::park`] returns at once.
#[cfg(all(target_family = "wasm", target_os = "unknown"))]
#[derive(Clone, Copy, Debug, Default)]
pub struct PageExecutor;

#[cfg(all(target_family = "wasm", target_os = "unknown"))]
impl PageExecutor {
    /// `setTimeout` caps delays at 2^31 - 1 ms; longer waits re-arm.
    const MAX_TIMEOUT_MS: u128 = 0x7fff_ffff;

    #[allow(
        clippy::future_not_send,
        reason = "awaits a JavaScript promise on the page's only thread"
    )]
    async fn sleep(delay: Duration) {
        use wasm_bindgen::JsCast as _;
        let millis = delay.as_millis().min(Self::MAX_TIMEOUT_MS);
        #[allow(
            clippy::cast_precision_loss,
            reason = "bounded by MAX_TIMEOUT_MS, exact in an f64"
        )]
        let millis = wasm_bindgen::JsValue::from_f64(millis as f64);
        let timer = js_sys::Promise::new(&mut |resolve, _reject| {
            let global = js_sys::global();
            let armed = js_sys::Reflect::get(&global, &"setTimeout".into())
                .ok()
                .and_then(|f| f.dyn_into::<js_sys::Function>().ok())
                .is_some_and(|set_timeout| set_timeout.call2(&global, &resolve, &millis).is_ok());
            if !armed {
                let _ = resolve.call0(&wasm_bindgen::JsValue::NULL);
            }
        });
        let _ = wasm_bindgen_futures::JsFuture::from(timer).await;
    }
}

#[cfg(all(target_family = "wasm", target_os = "unknown"))]
impl Executor for PageExecutor {
    fn spawn(&self, name: &'static str, job: Job) -> Result<(), Unsupported> {
        wasm_bindgen_futures::spawn_local(async move { run_job(name, job) });
        Ok(())
    }

    fn wake_at(&self, deadline: Instant, wake: WakeHandle) {
        wasm_bindgen_futures::spawn_local(async move {
            loop {
                let now = tilo_clock::now();
                if deadline <= now {
                    break;
                }
                Self::sleep(deadline.saturating_duration_since(now)).await;
            }
            wake.wake();
        });
    }

    fn park(&self, _deadline: Instant) {}
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn run_job(name: &'static str, job: Job) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).is_err() {
        tracing::warn!(job = name, "background job panicked");
    }
}

/// A timer waiting in a [`BinaryHeap`], earliest deadline (then earliest
/// registration) first.
struct Timer {
    deadline: Instant,
    sequence: u64,
    wake: WakeHandle,
}

impl PartialEq for Timer {
    fn eq(&self, other: &Self) -> bool {
        self.deadline == other.deadline && self.sequence == other.sequence
    }
}
impl Eq for Timer {}
impl PartialOrd for Timer {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Timer {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (other.deadline, other.sequence).cmp(&(self.deadline, self.sequence))
    }
}

#[derive(Default)]
struct Timers {
    heap: BinaryHeap<Timer>,
    sequence: u64,
}

impl Timers {
    fn push(&mut self, deadline: Instant, wake: WakeHandle) {
        self.sequence = self.sequence.wrapping_add(1);
        self.heap.push(Timer {
            deadline,
            sequence: self.sequence,
            wake,
        });
    }

    fn pop_due(&mut self, now: Instant) -> Option<WakeHandle> {
        if self.heap.peek().is_some_and(|timer| timer.deadline <= now) {
            self.heap.pop().map(|timer| timer.wake)
        } else {
            None
        }
    }

    fn next_deadline(&self) -> Option<Instant> {
        self.heap.peek().map(|timer| timer.deadline)
    }
}

#[derive(Default)]
struct PoolState {
    queue: VecDeque<(&'static str, Job)>,
    threads: usize,
    idle: usize,
    timers: Timers,
    timer_thread: bool,
    /// Bumped whenever a job finishes or a timer fires, for [`Executor::park`].
    progress: u64,
}

struct PoolShared {
    state: Mutex<PoolState>,
    work: Condvar,
    timer: Condvar,
    progress: Condvar,
    max_threads: usize,
}

/// A bounded pool of named worker threads, grown on demand, plus a single
/// timer thread for [`Executor::wake_at`].
///
/// Workers and the timer thread live as long as the process; the pool is
/// meant to be created once per host, not per service.
#[derive(Clone)]
pub struct ThreadPool {
    shared: Arc<PoolShared>,
}

impl ThreadPool {
    /// A pool that runs at most `max_threads` jobs at once (at least one).
    pub fn new(max_threads: usize) -> Self {
        Self {
            shared: Arc::new(PoolShared {
                state: Mutex::new(PoolState::default()),
                work: Condvar::new(),
                timer: Condvar::new(),
                progress: Condvar::new(),
                max_threads: max_threads.max(1),
            }),
        }
    }

    /// Jobs are mostly blocking I/O (fetches, transport calls), so the
    /// default allows more workers than a CPU-bound pool would.
    pub fn default_threads() -> usize {
        std::thread::available_parallelism()
            .map_or(4, std::num::NonZeroUsize::get)
            .clamp(4, 16)
    }

    fn worker(shared: &PoolShared) {
        let mut state = lock(&shared.state);
        loop {
            if let Some((name, job)) = state.queue.pop_front() {
                drop(state);
                run_job(name, job);
                state = lock(&shared.state);
                state.progress = state.progress.wrapping_add(1);
                shared.progress.notify_all();
                continue;
            }
            state.idle = state.idle.saturating_add(1);
            state = shared
                .work
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
            state.idle = state.idle.saturating_sub(1);
        }
    }

    fn timer_loop(shared: &PoolShared) {
        let mut state = lock(&shared.state);
        loop {
            let now = tilo_clock::now();
            let mut fired = Vec::new();
            while let Some(wake) = state.timers.pop_due(now) {
                fired.push(wake);
            }
            if !fired.is_empty() {
                state.progress = state.progress.wrapping_add(1);
                shared.progress.notify_all();
                drop(state);
                // Wake sinks may re-enter `wake_at`; never call them locked.
                for wake in fired {
                    wake.wake();
                }
                state = lock(&shared.state);
                continue;
            }
            state = match state.timers.next_deadline() {
                Some(deadline) => {
                    let timeout = deadline.saturating_duration_since(now);
                    shared
                        .timer
                        .wait_timeout(state, timeout)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
                None => shared
                    .timer
                    .wait(state)
                    .unwrap_or_else(PoisonError::into_inner),
            };
        }
    }
}

impl Executor for ThreadPool {
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the pool's lock stays held across the spawn so the thread count and queue agree"
    )]
    fn spawn(&self, name: &'static str, job: Job) -> Result<(), Unsupported> {
        crate::panic_boundary::abort_outside_boundaries_in_release();
        let mut state = lock(&self.shared.state);
        state.queue.push_back((name, job));
        if state.idle >= state.queue.len() || state.threads >= self.shared.max_threads {
            self.shared.work.notify_one();
            return Ok(());
        }
        let index = state.threads;
        let shared = Arc::clone(&self.shared);
        let spawned = std::thread::Builder::new()
            .name(format!("ui-background-{index}"))
            .spawn(move || Self::worker(&shared));
        if spawned.is_ok() {
            state.threads = state.threads.saturating_add(1);
            return Ok(());
        }
        if state.threads > 0 {
            // Existing workers will get to it.
            self.shared.work.notify_one();
            return Ok(());
        }
        state.queue.pop_back();
        Err(Unsupported)
    }

    fn wake_at(&self, deadline: Instant, wake: WakeHandle) {
        let mut state = lock(&self.shared.state);
        state.timers.push(deadline, wake);
        if !state.timer_thread {
            let shared = Arc::clone(&self.shared);
            let spawned = std::thread::Builder::new()
                .name("ui-background-timer".into())
                .spawn(move || Self::timer_loop(&shared));
            match spawned {
                Ok(_) => state.timer_thread = true,
                Err(error) => {
                    // Without a timer thread nothing would ever fire; wake now
                    // so the caller re-checks its own deadline on a frame.
                    tracing::warn!(%error, "background timer thread unavailable");
                    let due = state.timers.heap.drain().map(|timer| timer.wake);
                    let due: Vec<_> = due.collect();
                    drop(state);
                    for wake in due {
                        wake.wake();
                    }
                    return;
                }
            }
        }
        self.shared.timer.notify_one();
    }

    fn park(&self, deadline: Instant) {
        let state = lock(&self.shared.state);
        let seen = state.progress;
        let timeout = deadline.saturating_duration_since(tilo_clock::now());
        let _state = self
            .shared
            .progress
            .wait_timeout_while(state, timeout, |state| state.progress == seen)
            .unwrap_or_else(PoisonError::into_inner);
    }
}

#[derive(Default)]
struct PumpedState {
    queue: VecDeque<(&'static str, Job)>,
    timers: Timers,
    now: Option<Instant>,
}

/// An executor that runs nothing until its owner pumps it.
///
/// Jobs queue until [`run_pending`](Self::run_pending); timers fire when
/// [`advance`](Self::advance) moves the virtual clock past their deadline.
/// Headless tests use it to make background completion deterministic, and a
/// host that drives work from its own loop can install it and pump it there.
#[derive(Clone, Default)]
pub struct Pumped {
    state: Arc<Mutex<PumpedState>>,
}

impl Pumped {
    /// A pumped executor whose virtual clock starts at the real current time.
    pub fn new() -> Self {
        Self::starting_at(tilo_clock::now())
    }

    pub fn starting_at(now: Instant) -> Self {
        Self {
            state: Arc::new(Mutex::new(PumpedState {
                now: Some(now),
                ..PumpedState::default()
            })),
        }
    }

    pub fn background(&self) -> Background {
        Background::new(self.clone())
    }

    /// Run queued jobs, including any they queue, until none remain.
    /// Returns how many ran.
    pub fn run_pending(&self) -> usize {
        let mut ran = 0_usize;
        loop {
            let next = lock(&self.state).queue.pop_front();
            let Some((name, job)) = next else {
                return ran;
            };
            run_job(name, job);
            ran = ran.saturating_add(1);
        }
    }

    /// Move the virtual clock forward by `by`, firing every timer that falls
    /// due in deadline order and running the work each one releases.
    pub fn advance(&self, by: Duration) {
        let now = lock(&self.state).now.unwrap_or_else(tilo_clock::now);
        let target = now.checked_add(by).unwrap_or(now);
        self.advance_to(target);
    }

    fn advance_to(&self, target: Instant) {
        self.run_pending();
        loop {
            let wake = {
                let mut state = lock(&self.state);
                match state.timers.next_deadline() {
                    Some(deadline) if deadline <= target => {
                        if state.now.is_none_or(|now| now < deadline) {
                            state.now = Some(deadline);
                        }
                        state.timers.pop_due(deadline)
                    }
                    _ => None,
                }
            };
            let Some(wake) = wake else { break };
            wake.wake();
            self.run_pending();
        }
        let mut state = lock(&self.state);
        if state.now.is_none_or(|now| now < target) {
            state.now = Some(target);
        }
    }

    /// Whether no job is queued. Armed timers do not count: a periodic
    /// refresh is always armed.
    pub fn is_idle(&self) -> bool {
        lock(&self.state).queue.is_empty()
    }

    /// The earliest armed timer, if any.
    pub fn next_deadline(&self) -> Option<Instant> {
        lock(&self.state).timers.next_deadline()
    }

    /// The virtual clock.
    pub fn now(&self) -> Instant {
        let mut state = lock(&self.state);
        *state.now.get_or_insert_with(tilo_clock::now)
    }
}

impl Executor for Pumped {
    fn spawn(&self, name: &'static str, job: Job) -> Result<(), Unsupported> {
        lock(&self.state).queue.push_back((name, job));
        Ok(())
    }

    fn wake_at(&self, deadline: Instant, wake: WakeHandle) {
        lock(&self.state).timers.push(deadline, wake);
    }

    fn now(&self) -> Instant {
        Self::now(self)
    }

    /// Runs queued work; with none queued, advances the virtual clock to the
    /// next timer or to `deadline`, whichever comes first.
    fn park(&self, deadline: Instant) {
        if self.run_pending() > 0 {
            return;
        }
        let target = self
            .next_deadline()
            .map_or(deadline, |next| next.min(deadline));
        self.advance_to(target);
    }
}

/// An executor that refuses all background work, for hosts that have none.
/// Timers still fire, on [`park`](Executor::park), so deadline-driven callers
/// in a synchronous driver make progress.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoBackground;

impl Executor for NoBackground {
    fn spawn(&self, _name: &'static str, _job: Job) -> Result<(), Unsupported> {
        Err(Unsupported)
    }

    fn wake_at(&self, _deadline: Instant, wake: WakeHandle) {
        // No clock to wait on: wake now so the caller re-checks its deadline
        // on the next frame.
        wake.wake();
    }

    fn park(&self, _deadline: Instant) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wake::WakeScope;

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;

    fn counting_scope() -> (WakeScope, Arc<AtomicUsize>) {
        let wakes = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&wakes);
        let mut scope = WakeScope::new();
        scope
            .bind(move |_| {
                seen.fetch_add(1, Ordering::SeqCst);
            })
            .unwrap();
        (scope, wakes)
    }

    #[test]
    fn thread_pool_runs_named_jobs_survives_panics_and_wakes_timers_in_order() {
        // Case `thread_pool_runs_jobs_on_named_threads`.
        {
            let pool = ThreadPool::new(2);
            let (tx, rx) = mpsc::channel();
            for _ in 0..4 {
                let tx = tx.clone();
                pool.spawn(
                    "test",
                    Box::new(move || {
                        let name = std::thread::current().name().map(str::to_owned);
                        tx.send(name).unwrap();
                    }),
                )
                .unwrap();
            }
            for _ in 0..4 {
                let name = tilo_test_env::recv("a worker thread's name", &rx).unwrap();
                assert!(name.starts_with("ui-background-"), "{name}");
            }
            assert!(lock(&pool.shared.state).threads <= 2);
        }

        // Case `thread_pool_survives_a_panicking_job`.
        {
            let pool = ThreadPool::new(1);
            pool.spawn("panics", Box::new(|| panic!("job failure")))
                .unwrap();
            let (tx, rx) = mpsc::channel();
            pool.spawn("after", Box::new(move || tx.send(()).unwrap()))
                .unwrap();
            tilo_test_env::recv("the task spawned after", &rx);
        }

        // Case `thread_pool_timers_wake_in_deadline_order`.
        {
            let pool = ThreadPool::new(1);
            let (tx, rx) = mpsc::channel();
            let bound = |label: &'static str| {
                let tx = tx.clone();
                let mut scope = WakeScope::new();
                scope
                    .bind(move |_| {
                        let _ = tx.send(label);
                    })
                    .unwrap();
                scope
            };
            let late = bound("late");
            let early = bound("early");
            let start = tilo_clock::now();
            pool.wake_at(start + Duration::from_millis(60), late.handle());
            pool.wake_at(start + Duration::from_millis(10), early.handle());
            assert_eq!(tilo_test_env::recv("the early wake", &rx), "early");
            assert_eq!(tilo_test_env::recv("the late wake", &rx), "late");
        }

        // Case `thread_pool_park_returns_when_a_job_finishes`.
        {
            let pool = ThreadPool::new(1);
            let (release, gate) = mpsc::channel::<()>();
            pool.spawn("gated", Box::new(move || gate.recv().unwrap()))
                .unwrap();
            release.send(()).unwrap();
            let started = tilo_clock::now();
            pool.park(started + Duration::from_secs(30));
            assert!(started.elapsed() < Duration::from_secs(10));
        }
    }

    #[test]
    fn pumped_executor_runs_only_when_pumped_on_a_virtual_clock() {
        // Case `pumped_runs_nothing_until_pumped`.
        {
            let pumped = Pumped::new();
            let ran = Arc::new(AtomicUsize::new(0));
            let background = pumped.background();
            let counter = Arc::clone(&ran);
            let requeue = background.clone();
            background
                .spawn("outer", move || {
                    counter.fetch_add(1, Ordering::SeqCst);
                    let inner = Arc::clone(&counter);
                    requeue
                        .spawn("inner", move || {
                            inner.fetch_add(1, Ordering::SeqCst);
                        })
                        .unwrap();
                })
                .unwrap();
            assert_eq!(ran.load(Ordering::SeqCst), 0);
            assert!(!pumped.is_idle());
            assert_eq!(
                pumped.run_pending(),
                2,
                "work queued by a job runs in the same pump"
            );
            assert!(pumped.is_idle());
            assert_eq!(ran.load(Ordering::SeqCst), 2);
        }

        // Case `pumped_timers_follow_the_virtual_clock`.
        {
            let start = tilo_clock::now();
            let pumped = Pumped::starting_at(start);
            let (scope, wakes) = counting_scope();
            pumped.wake_at(start + Duration::from_secs(2), scope.handle());
            pumped.advance(Duration::from_millis(1999));
            assert_eq!(wakes.load(Ordering::SeqCst), 0);
            assert_eq!(pumped.next_deadline(), Some(start + Duration::from_secs(2)));
            pumped.advance(Duration::from_millis(1));
            assert_eq!(wakes.load(Ordering::SeqCst), 1);
            assert_eq!(pumped.now(), start + Duration::from_secs(2));
            assert_eq!(pumped.next_deadline(), None);
        }

        // Case `pumped_park_jumps_to_the_next_timer_without_waiting`.
        {
            let start = tilo_clock::now();
            let pumped = Pumped::starting_at(start);
            let (scope, wakes) = counting_scope();
            pumped.wake_at(start + Duration::from_secs(60), scope.handle());
            pumped.park(start + Duration::from_secs(3600));
            assert_eq!(wakes.load(Ordering::SeqCst), 1);
            assert_eq!(pumped.now(), start + Duration::from_secs(60));
            pumped.park(start + Duration::from_secs(120));
            assert_eq!(pumped.now(), start + Duration::from_secs(120));
        }

        // Case `entered_executor_is_captured_and_restored`.
        {
            let pumped = Pumped::new();
            {
                let _guard = pumped.background().enter();
                Background::current().spawn("scoped", || {}).unwrap();
                {
                    let _inner = Background::new(NoBackground).enter();
                    assert_eq!(
                        Background::current().spawn("refused", || {}),
                        Err(Unsupported)
                    );
                }
                Background::current().spawn("scoped", || {}).unwrap();
            }
            assert_eq!(pumped.run_pending(), 2);
        }
    }
}
