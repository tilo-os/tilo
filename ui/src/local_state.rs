//! Small, typed process-local state for retained application views.
//!
//! Durable/shareable values belong in the app's Automerge document. This
//! handle is for presentation state that should survive between widget events
//! without becoming part of persistence, collaboration, or undo history.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

type AppProxy = winit::event_loop::EventLoopProxy<crate::app::UserEvent>;

fn proxy_slot() -> &'static RwLock<Option<AppProxy>> {
    static PROXY: OnceLock<RwLock<Option<AppProxy>>> = OnceLock::new();
    PROXY.get_or_init(|| RwLock::new(None))
}

fn set_event_loop_proxy(proxy: Option<AppProxy>) {
    let mut slot = proxy_slot()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *slot = proxy;
}

pub(crate) struct EventLoopProxyGuard;

pub(crate) fn install_event_loop_proxy(proxy: AppProxy) -> EventLoopProxyGuard {
    set_event_loop_proxy(Some(proxy));
    EventLoopProxyGuard
}

impl Drop for EventLoopProxyGuard {
    fn drop(&mut self) {
        set_event_loop_proxy(None);
    }
}

fn wake_app() {
    let slot = proxy_slot()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(proxy) = slot.as_ref() {
        let _ = proxy.send_event(crate::app::UserEvent::Wake);
    }
}

/// Cloneable capability for background work to schedule one application
/// synchronization/redraw pass.
///
/// The standard live lifecycle installs the event-loop destination
/// automatically. A handle can be created before the window starts and is a
/// harmless no-op in a headless process; no app-owned proxy or global setup is
/// required.
#[derive(Clone, Copy, Debug, Default)]
pub struct WakeHandle;

impl WakeHandle {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Wake the live application event loop, if one is installed.
    pub fn wake(self) {
        wake_app();
    }
}

struct LocalStateInner<T> {
    value: RwLock<T>,
    revision: AtomicU64,
}

/// A cloneable handle to typed, process-local application state.
///
/// Clones refer to the same value. Mutations advance an internal revision so
/// bound widgets can skip formatting work when nothing changed.
pub struct LocalState<T> {
    inner: Arc<LocalStateInner<T>>,
}

impl<T> Clone for LocalState<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<T> LocalState<T> {
    pub fn new(value: T) -> Self {
        Self {
            inner: Arc::new(LocalStateInner {
                value: RwLock::new(value),
                revision: AtomicU64::new(0),
            }),
        }
    }

    /// Read the current value without exposing the backing lock.
    pub fn with<R>(&self, read: impl FnOnce(&T) -> R) -> R {
        let value = self
            .inner
            .value
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        read(&value)
    }

    /// Replace the value and invalidate widgets bound to this handle.
    ///
    /// At revision exhaustion this is a no-op. Use [`Self::try_set`] when the
    /// caller needs to observe that failure.
    pub fn set(&self, value: T) {
        let _ = self.try_set(value);
    }

    /// As [`Self::set`], reporting whether the replacement was published.
    /// Returns `false` once the revision namespace is exhausted; the value is
    /// then left untouched, so a bound widget cannot mistake a new value for
    /// one it already observed under a wrapped revision.
    pub fn try_set(&self, value: T) -> bool {
        self.replace(value).is_some()
    }

    fn replace(&self, value: T) -> Option<u64> {
        let mut current = self
            .inner
            .value
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let revision = self.next_revision()?;
        *current = value;
        self.inner.revision.store(revision, Ordering::Release);
        drop(current);
        Some(revision)
    }

    /// Mutate the value and invalidate widgets bound to this handle once.
    /// At revision exhaustion this is a no-op. Use [`Self::try_update`] when
    /// the caller needs to observe that failure.
    pub fn update(&self, update: impl FnOnce(&mut T)) {
        let _ = self.try_update(update);
    }

    /// As [`Self::update`], reporting whether `update` ran. Returns `false`
    /// at revision exhaustion without invoking `update`.
    pub fn try_update(&self, update: impl FnOnce(&mut T)) -> bool {
        let mut value = self
            .inner
            .value
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(revision) = self.next_revision() else {
            return false;
        };
        update(&mut value);
        self.inner.revision.store(revision, Ordering::Release);
        drop(value);
        true
    }

    pub(crate) fn revision(&self) -> u64 {
        self.inner.revision.load(Ordering::Acquire)
    }

    fn next_revision(&self) -> Option<u64> {
        // Every caller holds `value`'s write lock, which serializes revision
        // allocation. Publish only after replacing the protected value so a
        // binding that sees the new revision cannot read the old value.
        self.inner.revision.load(Ordering::Relaxed).checked_add(1)
    }

    /// Create a thread-safe writer that wakes the application after mutation.
    ///
    /// Create it wherever the state is created and move a clone into each
    /// worker. The standard [`App`](crate::App) lifecycle installs its event
    /// loop automatically; no proxy or string message needs to cross app code.
    pub fn writer(&self) -> LocalStateWriter<T>
    where
        T: Send + Sync + 'static,
    {
        LocalStateWriter {
            state: self.clone(),
            wake: Arc::new(wake_app),
        }
    }
}

/// Revision-aware widget-side half of a [`LocalState`] binding.
///
/// Kept crate-private so every retained control shares identical polling and
/// publication semantics without exposing implementation machinery to apps.
pub(crate) struct LocalBinding<T> {
    state: LocalState<T>,
    observed_revision: u64,
}

impl<T: Clone> LocalBinding<T> {
    pub(crate) fn new(state: &LocalState<T>) -> (T, Self) {
        // Read the revision first. If a worker mutates between these reads,
        // the value is already current and the next poll is merely redundant;
        // the reverse order could pair an old value with a new revision and
        // miss that mutation entirely.
        let observed_revision = state.revision();
        let value = state.with(Clone::clone);
        (
            value,
            Self {
                state: state.clone(),
                observed_revision,
            },
        )
    }

    pub(crate) fn poll(&mut self) -> Option<T> {
        let revision = self.state.revision();
        if revision == self.observed_revision {
            return None;
        }
        self.observed_revision = revision;
        Some(self.state.with(Clone::clone))
    }

    pub(crate) fn publish_by(&mut self, value: &T, equal: impl FnOnce(&T, &T) -> bool) {
        if !self.state.with(|current| equal(current, value))
            && let Some(revision) = self.state.replace(value.clone())
        {
            self.observed_revision = revision;
        }
    }
}

impl<T: Clone + PartialEq> LocalBinding<T> {
    pub(crate) fn publish(&mut self, value: &T) {
        self.publish_by(value, PartialEq::eq);
    }
}

/// A background-safe [`LocalState`] mutation handle with event-loop wakeup.
pub struct LocalStateWriter<T> {
    state: LocalState<T>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

/// Typed state for restartable background work where only the latest run may
/// publish a result. Starting or cancelling a run invalidates every older
/// [`TaskCompletion`].
pub struct LatestTask<T> {
    state: LocalState<T>,
    generation: Arc<Mutex<u64>>,
}

/// UI-thread inbox for worker results where only the newest request may win.
///
/// The owner keeps this value inside its table/list/domain model. Calling
/// [`Self::begin`] invalidates every older completion; an accepted completion
/// wakes the application and is drained with [`Self::try_take`]. This keeps
/// channels, event-loop proxies, and generation races out of app models.
pub struct LatestRequest<T> {
    sender: Sender<(u64, T)>,
    receiver: Receiver<(u64, T)>,
    generation: Arc<AtomicU64>,
    pending: Option<u64>,
}

impl<T> Default for LatestRequest<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> LatestRequest<T> {
    #[must_use]
    pub fn new() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            sender,
            receiver,
            generation: Arc::new(AtomicU64::new(0)),
            pending: None,
        }
    }

    /// Start a request, invalidating any older completion capability.
    ///
    /// # Panics
    ///
    /// Panics after the generation namespace is exhausted. This preserves the
    /// original infallible API without ever returning an aliased capability;
    /// long-lived hosts that need to handle exhaustion can use
    /// [`Self::try_begin`].
    pub fn begin(&mut self) -> RequestCompletion<T> {
        self.try_begin()
            .expect("latest-request generation namespace exhausted")
    }

    /// Fallible form of [`Self::begin`]. Returns `None` without changing state
    /// once no unique completion generation remains.
    pub fn try_begin(&mut self) -> Option<RequestCompletion<T>> {
        // Reserve `u64::MAX` as a terminal invalidation marker. It is never
        // handed to a completion capability, so an exhausted counter cannot
        // make an old completion current again.
        let generation = self
            .generation
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |generation| {
                generation.checked_add(1).filter(|next| *next != u64::MAX)
            })
            .ok()
            .and_then(|generation| generation.checked_add(1))?;
        self.pending = Some(generation);
        Some(RequestCompletion {
            sender: self.sender.clone(),
            generation: Arc::clone(&self.generation),
            request_generation: generation,
            wake: WakeHandle::new(),
        })
    }

    /// Invalidate outstanding work without publishing a replacement.
    ///
    /// At the terminal invalidation marker this is a safe no-op because no
    /// completion capability is ever issued for that reserved generation. Use
    /// [`Self::try_cancel`] when the caller needs to observe exhaustion.
    pub fn cancel(&mut self) {
        let _ = self.try_cancel();
    }

    /// Fallible form of [`Self::cancel`]. Returns `false` only after the
    /// terminal invalidation marker has already been reached.
    pub fn try_cancel(&mut self) -> bool {
        if self
            .generation
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |generation| {
                generation.checked_add(1)
            })
            .is_err()
        {
            return false;
        }
        self.pending = None;
        true
    }

    #[must_use]
    pub const fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Drain the accepted result, discarding messages from stale generations.
    pub fn try_take(&mut self) -> Option<T> {
        while let Ok((generation, value)) = self.receiver.try_recv() {
            if self.pending == Some(generation)
                && self.generation.load(Ordering::Acquire) == generation
            {
                self.pending = None;
                return Some(value);
            }
        }
        None
    }
}

/// Single-use worker capability returned by [`LatestRequest::begin`].
pub struct RequestCompletion<T> {
    sender: Sender<(u64, T)>,
    generation: Arc<AtomicU64>,
    request_generation: u64,
    wake: WakeHandle,
}

impl<T> RequestCompletion<T> {
    /// Whether this request is still current. Workers can check between
    /// expensive units for cooperative cancellation.
    #[must_use]
    pub fn is_current(&self) -> bool {
        self.generation.load(Ordering::Acquire) == self.request_generation
    }

    /// Publish a result only if no newer request or cancellation won first.
    /// Returns whether the result was accepted and the app was woken.
    pub fn complete(self, value: T) -> bool {
        if !self.is_current() {
            return false;
        }
        if self.sender.send((self.request_generation, value)).is_err() {
            return false;
        }
        // `begin` may have won between the first freshness check and the
        // channel send. The inbox will discard that message; report it as
        // stale and avoid an unnecessary wake as well.
        if !self.is_current() {
            return false;
        }
        self.wake.wake();
        true
    }
}

impl<T> Clone for LatestTask<T> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            generation: Arc::clone(&self.generation),
        }
    }
}

impl<T> LatestTask<T> {
    #[must_use]
    pub fn new(initial: T) -> Self {
        Self {
            state: LocalState::new(initial),
            generation: Arc::new(Mutex::new(0)),
        }
    }

    /// Clone the observable state for `ModelView`, `Text`, or UI callbacks.
    #[must_use]
    pub fn state(&self) -> LocalState<T> {
        self.state.clone()
    }

    /// Begin a new run and return its background-safe completion capability.
    /// Any earlier capability becomes stale before `running` is published.
    ///
    /// # Panics
    ///
    /// Panics after the generation namespace is exhausted. This preserves the
    /// original infallible API without reissuing an aliased capability; use
    /// [`Self::try_begin`] when exhaustion must be handled explicitly.
    pub fn begin(&self, running: T) -> TaskCompletion<T>
    where
        T: Send + Sync + 'static,
    {
        self.try_begin(running)
            .expect("latest-task generation namespace exhausted")
    }

    /// Fallible form of [`Self::begin`]. Returns `None` without publishing the
    /// running state once no unique generation remains.
    pub fn try_begin(&self, running: T) -> Option<TaskCompletion<T>>
    where
        T: Send + Sync + 'static,
    {
        let mut generation = self
            .generation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let run = generation.checked_add(1).filter(|next| *next != u64::MAX)?;
        // Keep generation and state publication in one critical section so a
        // prior worker cannot pass its check between these operations.
        if !self.state.try_set(running) {
            return None;
        }
        *generation = run;
        drop(generation);
        Some(TaskCompletion {
            writer: self.state.writer(),
            generation: Arc::clone(&self.generation),
            run,
        })
    }

    /// Invalidate the current run and publish its replacement state. A worker
    /// that finishes afterward cannot overwrite this value.
    pub fn cancel(&self, cancelled: T) {
        let _ = self.try_cancel(cancelled);
    }

    /// Fallible form of [`Self::cancel`].
    pub fn try_cancel(&self, cancelled: T) -> bool {
        let mut generation = self
            .generation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(next) = generation.checked_add(1) else {
            return false;
        };
        // Invalidating the run and publishing cancellation are one atomic
        // ordering boundary with respect to `TaskCompletion::complete`.
        if !self.state.try_set(cancelled) {
            return false;
        }
        *generation = next;
        drop(generation);
        true
    }
}

/// A single-use capability returned by [`LatestTask::begin`].
pub struct TaskCompletion<T> {
    writer: LocalStateWriter<T>,
    generation: Arc<Mutex<u64>>,
    run: u64,
}

impl<T> TaskCompletion<T> {
    /// Whether this run is still the latest. Long-running workers can check
    /// this between units of work for cooperative cancellation.
    #[must_use]
    pub fn is_current(&self) -> bool {
        let generation = self
            .generation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *generation == self.run
    }

    /// Publish `value` only if no newer run or cancellation won the race.
    /// Returns whether the value was accepted and the application was woken.
    pub fn complete(self, value: T) -> bool {
        let generation = self
            .generation
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *generation != self.run {
            return false;
        }
        // Hold the generation guard until the accepted value is committed;
        // begin/cancel cannot interleave after the freshness check.
        let accepted = self.writer.try_set(value);
        drop(generation);
        accepted
    }
}

impl<T> Clone for LocalStateWriter<T> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            wake: Arc::clone(&self.wake),
        }
    }
}

impl<T> LocalStateWriter<T> {
    /// Replace the value and schedule synchronization and redraw.
    /// At revision exhaustion this is a no-op. Use [`Self::try_set`] to learn
    /// whether a wake was scheduled.
    pub fn set(&self, value: T) {
        let _ = self.try_set(value);
    }

    /// As [`Self::set`], reporting whether the replacement was published.
    pub fn try_set(&self, value: T) -> bool {
        if !self.state.try_set(value) {
            return false;
        }
        (self.wake)();
        true
    }

    /// Mutate the value once and schedule synchronization and redraw.
    /// At revision exhaustion this is a no-op. Use [`Self::try_update`] to
    /// learn whether a wake was scheduled.
    pub fn update(&self, update: impl FnOnce(&mut T)) {
        let _ = self.try_update(update);
    }

    /// As [`Self::update`], reporting whether the mutation was published.
    pub fn try_update(&self, update: impl FnOnce(&mut T)) -> bool {
        if !self.state.try_update(update) {
            return false;
        }
        (self.wake)();
        true
    }

    #[cfg(test)]
    fn for_test(state: &LocalState<T>, wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            state: state.clone(),
            wake: Arc::new(wake),
        }
    }
}

impl<T: Clone> LocalState<T> {
    pub fn get(&self) -> T {
        self.with(Clone::clone)
    }
}

impl LocalState<String> {
    /// A directly usable [`TextInput`](crate::widget::TextInput) callback.
    ///
    /// This keeps the common input-to-local-state path allocation and closure
    /// boilerplate out of application code.
    pub fn text_sink(&self) -> impl FnMut(&str) + 'static {
        let state = self.clone();
        move |value| {
            state.set(value.to_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn clones_share_typed_value_and_revision() {
        let state = LocalState::new(vec![1, 2]);
        let clone = state.clone();
        assert_eq!(clone.get(), vec![1, 2]);
        assert_eq!(state.revision(), 0);

        clone.update(|items| items.push(3));
        assert_eq!(state.get(), vec![1, 2, 3]);
        assert_eq!(state.revision(), 1);

        state.set(vec![4]);
        assert_eq!(clone.get(), vec![4]);
        assert_eq!(clone.revision(), 2);
    }

    #[test]
    fn text_sink_accepts_text_input_callback_shape() {
        let state = LocalState::new(String::new());
        let mut sink = state.text_sink();
        sink("漢字");
        assert_eq!(state.get(), "漢字");
    }

    #[test]
    fn widget_binding_polls_and_publishes_without_echoing_its_own_write() {
        let state = LocalState::new(String::from("one"));
        let (initial, mut binding) = LocalBinding::new(&state);
        assert_eq!(initial, "one");
        assert_eq!(binding.poll(), None);

        state.set(String::from("two"));
        assert_eq!(binding.poll().as_deref(), Some("two"));
        assert_eq!(binding.poll(), None);

        binding.publish(&String::from("three"));
        assert_eq!(state.get(), "three");
        assert_eq!(binding.poll(), None, "a widget does not echo its own write");
    }

    #[test]
    fn local_state_can_cross_a_worker_thread() {
        let state = LocalState::new(String::from("loading"));
        let worker = state.clone();
        std::thread::spawn(move || worker.set(String::from("ready")))
            .join()
            .unwrap();
        assert_eq!(state.get(), "ready");
        assert_eq!(state.revision(), 1);
    }

    #[test]
    fn background_writer_wakes_once_after_each_completed_mutation() {
        let state = LocalState::new(vec![1]);
        let wakes = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&wakes);
        let writer = LocalStateWriter::for_test(&state, move || {
            observed.fetch_add(1, Ordering::Relaxed);
        });

        writer.update(|values| values.push(2));
        writer.set(vec![3]);
        assert_eq!(state.get(), vec![3]);
        assert_eq!(state.revision(), 2);
        assert_eq!(wakes.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn latest_task_rejects_an_older_completion() {
        let task = LatestTask::new(0);
        let state = task.state();
        let first = task.begin(1);
        let second = task.begin(2);

        assert!(!first.is_current());
        assert!(second.is_current());
        assert!(!first.complete(10));
        assert_eq!(state.get(), 2);
        assert!(second.complete(20));
        assert_eq!(state.get(), 20);
    }

    #[test]
    fn latest_task_cancel_prevents_a_late_worker_overwrite() {
        let task = LatestTask::new("idle");
        let state = task.state();
        let completion = task.begin("loading");

        assert!(task.try_cancel("cancelled"));

        assert!(!completion.is_current());
        assert!(!completion.complete("late result"));
        assert_eq!(state.get(), "cancelled");
    }

    #[test]
    fn latest_request_drains_only_the_current_completion() {
        let mut request = LatestRequest::new();
        let first = request.begin();
        let second = request.begin();

        assert!(!first.is_current());
        assert!(second.is_current());
        assert!(!first.complete("stale"));
        assert!(request.is_pending());
        assert!(second.complete("ready"));
        assert_eq!(request.try_take(), Some("ready"));
        assert!(!request.is_pending());
        assert_eq!(request.try_take(), None);
    }

    #[test]
    fn latest_request_cancel_rejects_late_work() {
        let mut request = LatestRequest::new();
        let completion = request.begin();

        assert!(request.try_cancel());

        assert!(!completion.is_current());
        assert!(!completion.complete(42));
        assert!(!request.is_pending());
        assert_eq!(request.try_take(), None);
    }

    #[test]
    fn exhausted_local_state_revision_leaves_value_and_callback_untouched() {
        let state = LocalState::new(String::from("before"));
        state.inner.revision.store(u64::MAX, Ordering::Release);
        let called = AtomicUsize::new(0);

        assert!(!state.try_set(String::from("after")));
        assert!(!state.try_update(|value| {
            called.fetch_add(1, Ordering::Relaxed);
            value.push_str(" mutated");
        }));
        assert_eq!(state.get(), "before");
        assert_eq!(called.load(Ordering::Relaxed), 0);
        assert_eq!(state.revision(), u64::MAX);
    }

    #[test]
    fn exhausted_latest_request_never_reissues_a_completion_generation() {
        let mut request = LatestRequest::<()>::new();
        request.generation.store(u64::MAX - 1, Ordering::Release);

        assert!(request.try_begin().is_none());
        assert!(!request.is_pending());
        assert!(
            request.try_cancel(),
            "the terminal invalidation is still available"
        );
        assert_eq!(request.generation.load(Ordering::Acquire), u64::MAX);
        assert!(!request.try_cancel());
        assert!(request.try_begin().is_none());
    }

    #[test]
    fn exhausted_latest_task_does_not_publish_a_new_run() {
        let task = LatestTask::new("idle");
        let state = task.state();
        *task.generation.lock().unwrap() = u64::MAX - 1;

        assert!(task.try_begin("running").is_none());
        assert_eq!(state.get(), "idle");
        assert!(task.try_cancel("cancelled"));
        assert_eq!(state.get(), "cancelled");
        assert!(!task.try_cancel("later"));
        assert_eq!(state.get(), "cancelled");
    }
}
