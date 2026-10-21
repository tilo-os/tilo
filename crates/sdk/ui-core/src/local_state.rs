//! Small, typed process-local state for retained application views.
//!
//! Durable/shareable values belong in the app's Automerge document. This
//! handle is for presentation state that should survive between widget events
//! without becoming part of persistence, collaboration, or undo history.

use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, RwLock};

use tilo_lock::{MutexRecover as _, RwLockRecover as _};
use tokio::task::AbortHandle;

pub use crate::wake::WakeHandle;

struct LocalStateInner<T> {
    value: RwLock<T>,
    revision: AtomicU64,
    wake: WakeHandle,
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
                wake: WakeHandle::new(),
            }),
        }
    }

    /// Read the current value without exposing the backing lock.
    pub fn with<R>(&self, read: impl FnOnce(&T) -> R) -> R {
        let value = self.inner.value.read_recover();
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
    /// Returns `false` when the captured owner is closed or the revision
    /// namespace is exhausted. The value is then left untouched, so a bound
    /// widget cannot mistake a new value for one it already observed under a
    /// wrapped revision.
    pub fn try_set(&self, value: T) -> bool {
        self.replace(value).is_some()
    }

    fn replace(&self, value: T) -> Option<u64> {
        if !self.inner.wake.authorize_publication() {
            return None;
        }
        let mut current = self.inner.value.write_recover();
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
    /// when the captured owner is closed or at revision exhaustion, without
    /// invoking `update`.
    pub fn try_update(&self, update: impl FnOnce(&mut T)) -> bool {
        if !self.inner.wake.authorize_publication() {
            return false;
        }
        let mut value = self.inner.value.write_recover();
        let Some(revision) = self.next_revision() else {
            return false;
        };
        update(&mut value);
        self.inner.revision.store(revision, Ordering::Release);
        drop(value);
        true
    }

    /// Monotonic local edit revision for correlating asynchronous receipts.
    pub fn revision(&self) -> u64 {
        self.inner.revision.load(Ordering::Acquire)
    }

    fn next_revision(&self) -> Option<u64> {
        // Every caller holds `value`'s write lock, which serializes revision
        // allocation. Publish only after replacing the protected value so a
        // binding that sees the new revision cannot read the old value.
        self.inner.revision.load(Ordering::Relaxed).checked_add(1)
    }

    /// Create a thread-safe writer that wakes this state's captured owner after
    /// mutation.
    ///
    /// The owner is captured once by [`LocalState::new`], so writers requested
    /// under another app keep the original destination. State created outside
    /// a wake scope is permanently detached. Live apps should construct state
    /// and roots inside an [`App`](crate::App) factory or explicit scope.
    pub fn writer(&self) -> LocalStateWriter<T>
    where
        T: Send + Sync + 'static,
    {
        LocalStateWriter {
            state: self.clone(),
            wake: self.inner.wake.clone(),
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
    wake: WakeHandle,
}

/// Typed state for restartable background work where only the latest run may
/// publish a result. Starting or cancelling a run invalidates every older
/// [`TaskCompletion`].
pub struct LatestTask<T> {
    state: LocalState<T>,
    generation: Arc<Mutex<u64>>,
    task: Arc<Mutex<Option<AbortHandle>>>,
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
    wake: WakeHandle,
    task: Option<AbortHandle>,
}

impl<T> Drop for LatestRequest<T> {
    fn drop(&mut self) {
        self.abort_task();
    }
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
            wake: WakeHandle::new(),
            task: None,
        }
    }

    fn abort_task(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }

    /// Start a request, invalidating any older completion capability.
    ///
    /// After the generation namespace is exhausted this returns a permanently
    /// stale capability which cannot publish. This preserves the original
    /// infallible API without aliasing a live generation; long-lived hosts that
    /// need to observe exhaustion can use [`Self::try_begin`].
    pub fn begin(&mut self) -> RequestCompletion<T> {
        match self.try_begin() {
            Some(completion) => completion,
            None => RequestCompletion {
                sender: self.sender.clone(),
                generation: Arc::clone(&self.generation),
                request_generation: 0,
                wake: self.wake.clone(),
            },
        }
    }

    /// Fallible form of [`Self::begin`]. Returns `None` without changing state
    /// when the captured owner is closed or no unique completion generation
    /// remains.
    pub fn try_begin(&mut self) -> Option<RequestCompletion<T>> {
        if !self.wake.authorize_publication() {
            return None;
        }
        self.abort_task();
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
            wake: self.wake.clone(),
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
        self.abort_task();
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
    pub fn is_pending(&self) -> bool {
        self.wake.is_active() && self.pending.is_some()
    }

    /// Drain the accepted result, discarding messages from stale generations.
    pub fn try_take(&mut self) -> Option<T> {
        if !self.wake.is_active() {
            self.pending = None;
            while self.receiver.try_recv().is_ok() {}
            return None;
        }
        while let Ok((generation, value)) = self.receiver.try_recv() {
            if self.pending == Some(generation)
                && self.generation.load(Ordering::Acquire) == generation
                && self.wake.is_active()
            {
                self.pending = None;
                return Some(value);
            }
        }
        None
    }
}

impl<T: Send + 'static> LatestRequest<T> {
    /// Start a request whose result is `future`'s output.
    ///
    /// The future runs on the app runtime ([`tilo_rt::app`]), never on the
    /// calling UI thread. Its output wakes the event loop and is drained with
    /// [`Self::try_take`]. A newer request, a cancellation or dropping this
    /// inbox aborts the future at its next await point.
    ///
    /// Returns `false` without spawning when the owner is closed or the
    /// generation namespace is exhausted (see [`Self::try_begin`]).
    pub fn spawn<F>(&mut self, future: F) -> bool
    where
        F: Future<Output = T> + Send + 'static,
    {
        let Some(completion) = self.try_begin() else {
            return false;
        };
        let task = tilo_rt::app().spawn(async move {
            let value = future.await;
            completion.complete(value);
        });
        self.task = Some(task.abort_handle());
        true
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
        self.request_generation != 0
            && self.wake.is_active()
            && self.generation.load(Ordering::Acquire) == self.request_generation
    }

    /// Publish a result only if no newer request or cancellation won first.
    /// Returns whether the result was accepted. The captured owner may close
    /// after authorization and suppress notification without revoking it.
    pub fn complete(self, value: T) -> bool {
        if self.request_generation == 0
            || self.generation.load(Ordering::Acquire) != self.request_generation
            || !self.wake.authorize_publication()
        {
            return false;
        }
        if self.sender.send((self.request_generation, value)).is_err() {
            return false;
        }
        // Check only generation here: a close after successful authorization may
        // suppress notification, but does not revoke the accepted publication.
        if self.generation.load(Ordering::Acquire) != self.request_generation {
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
            task: Arc::clone(&self.task),
        }
    }
}

impl<T> LatestTask<T> {
    #[must_use]
    pub fn new(initial: T) -> Self {
        Self {
            state: LocalState::new(initial),
            generation: Arc::new(Mutex::new(0)),
            task: Arc::new(Mutex::new(None)),
        }
    }

    /// Abort the future started by [`Self::spawn`], if one is running, and
    /// remember `next` as the one to abort later.
    fn replace_task(&self, next: Option<AbortHandle>) {
        let previous = std::mem::replace(&mut *self.task.lock_recover(), next);
        if let Some(previous) = previous {
            previous.abort();
        }
    }

    /// Publish `running`, then run `future` on the app runtime
    /// ([`tilo_rt::app`]) and publish its output, waking the event loop.
    ///
    /// The future never runs on the calling UI thread. Starting another run
    /// or cancelling aborts it at its next await point, and its output is
    /// discarded if it finishes anyway.
    ///
    /// Returns `false` without spawning when [`Self::try_begin`] refuses.
    pub fn spawn<F>(&self, running: T, future: F) -> bool
    where
        T: Send + Sync + 'static,
        F: Future<Output = T> + Send + 'static,
    {
        let Some(completion) = self.try_begin(running) else {
            return false;
        };
        let task = tilo_rt::app().spawn(async move {
            let value = future.await;
            completion.complete(value);
        });
        self.replace_task(Some(task.abort_handle()));
        true
    }

    /// Clone the observable state for `ModelView`, `Text`, or UI callbacks.
    #[must_use]
    pub fn state(&self) -> LocalState<T> {
        self.state.clone()
    }

    /// Begin a new run and return its background-safe completion capability.
    /// Any earlier capability becomes stale before `running` is published.
    ///
    /// After the generation namespace is exhausted this returns a permanently
    /// stale capability which cannot publish. This preserves the original
    /// infallible API without reissuing an aliased generation; use
    /// [`Self::try_begin`] when exhaustion must be handled explicitly.
    pub fn begin(&self, running: T) -> TaskCompletion<T>
    where
        T: Send + Sync + 'static,
    {
        match self.try_begin(running) {
            Some(completion) => completion,
            None => TaskCompletion {
                writer: self.state.writer(),
                generation: Arc::clone(&self.generation),
                run: 0,
            },
        }
    }

    /// Fallible form of [`Self::begin`]. `Some` means the running state and a
    /// unique task generation were accepted for publication; it does not mean
    /// that a wake was delivered or scheduled. Returns `None` without
    /// publishing when the captured owner is closed, the state revision
    /// namespace is exhausted, or the task generation namespace is exhausted.
    pub fn try_begin(&self, running: T) -> Option<TaskCompletion<T>>
    where
        T: Send + Sync + 'static,
    {
        let mut generation = self.generation.lock_recover();
        let run = generation.checked_add(1).filter(|next| *next != u64::MAX)?;
        self.replace_task(None);
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
        let mut generation = self.generation.lock_recover();
        let Some(next) = generation.checked_add(1) else {
            return false;
        };
        self.replace_task(None);
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
        if self.run == 0 || !self.writer.wake.is_active() {
            return false;
        }
        let generation = self.generation.lock_recover();
        *generation == self.run
    }

    /// Publish `value` only if no newer run or cancellation won the race.
    /// Returns whether the value was accepted. The captured owner may close
    /// after authorization and suppress notification without revoking it.
    pub fn complete(self, value: T) -> bool {
        let generation = self.generation.lock_recover();
        if self.run == 0 || *generation != self.run {
            return false;
        }
        // state.try_set authorizes once and publishes while begin/cancel cannot
        // change the generation. Never invoke the sink while holding this lock.
        let accepted = self.writer.state.try_set(value);
        drop(generation);
        if accepted {
            self.writer.wake.wake();
        }
        accepted
    }
}

impl<T> Clone for LocalStateWriter<T> {
    fn clone(&self) -> Self {
        Self {
            state: self.state.clone(),
            wake: self.wake.clone(),
        }
    }
}

impl<T> LocalStateWriter<T> {
    /// Replace the value and request synchronization and redraw.
    ///
    /// The mutation is accepted only while the captured owner is open and its
    /// revision namespace is not exhausted. The return value from
    /// [`Self::try_set`] reports publication acceptance, not wake delivery or
    /// scheduling.
    pub fn set(&self, value: T) {
        let _ = self.try_set(value);
    }

    /// As [`Self::set`], reporting whether the replacement was accepted for
    /// publication. `false` means the captured owner is closed or the
    /// revision namespace is exhausted; no wake delivery or scheduling is
    /// implied by `true`.
    pub fn try_set(&self, value: T) -> bool {
        if !self.state.try_set(value) {
            return false;
        }
        self.wake.wake();
        true
    }

    /// Mutate the value once and request synchronization and redraw.
    ///
    /// The mutation is accepted only while the captured owner is open and its
    /// revision namespace is not exhausted. The return value from
    /// [`Self::try_update`] reports publication acceptance, not wake delivery
    /// or scheduling.
    pub fn update(&self, update: impl FnOnce(&mut T)) {
        let _ = self.try_update(update);
    }

    /// As [`Self::update`], reporting whether the mutation was accepted for
    /// publication. `false` means the captured owner is closed or the
    /// revision namespace is exhausted; the callback is not invoked. `true`
    /// does not mean that a wake was delivered or scheduled.
    pub fn try_update(&self, update: impl FnOnce(&mut T)) -> bool {
        if !self.state.try_update(update) {
            return false;
        }
        self.wake.wake();
        true
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
    use std::sync::mpsc::TryRecvError;

    use super::*;
    use crate::wake::{WakeEvent, WakeScope};

    fn bound_scope() -> (WakeScope, Receiver<WakeEvent>) {
        let (tx, rx) = mpsc::channel();
        let mut scope = WakeScope::new();
        scope
            .bind(move |event| {
                let _ = tx.send(event);
            })
            .unwrap();
        (scope, rx)
    }

    #[test]
    fn local_state_shares_value_and_revision_across_clones_threads_and_bindings() {
        // Case `clones_share_typed_value_and_revision`.
        {
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

        // Case `text_sink_accepts_text_input_callback_shape`.
        {
            let state = LocalState::new(String::new());
            let mut sink = state.text_sink();
            sink("漢字");
            assert_eq!(state.get(), "漢字");
        }

        // Case `widget_binding_polls_and_publishes_without_echoing_its_own_write`.
        {
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

        // Case `local_state_can_cross_a_worker_thread`.
        {
            let state = LocalState::new(String::from("loading"));
            let worker = state.clone();
            std::thread::spawn(move || worker.set(String::from("ready")))
                .join()
                .unwrap();
            assert_eq!(state.get(), "ready");
            assert_eq!(state.revision(), 1);
        }

        // Case `background_writer_wakes_once_after_each_completed_mutation`.
        {
            let (scope, rx) = bound_scope();
            let state = {
                let _entry = scope.enter();
                LocalState::new(vec![1])
            };
            let writer = state.writer();

            writer.update(|values| values.push(2));
            writer.set(vec![3]);
            assert_eq!(state.get(), vec![3]);
            assert_eq!(state.revision(), 2);
            assert_eq!(rx.try_iter().count(), 2);
        }

        // Case `writer_publication_before_binding_flushes_one_pending_event`.
        {
            let mut scope = WakeScope::new();
            let state = {
                let _entry = scope.enter();
                LocalState::new(0)
            };
            let writer = state.writer();
            std::thread::spawn(move || {
                writer.set(1);
                writer.set(2);
            })
            .join()
            .unwrap();
            let (tx, rx) = mpsc::channel();
            scope
                .bind(move |event| {
                    let _ = tx.send(event);
                })
                .unwrap();
            assert!(scope.accepts(&rx.try_recv().unwrap()));
            assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
            assert_eq!(state.get(), 2);
        }
    }

    #[test]
    fn latest_task_and_request_reject_stale_or_cancelled_completions() {
        // Case `latest_task_rejects_an_older_completion`.
        {
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

        // Case `latest_task_cancel_prevents_a_late_worker_overwrite`.
        {
            let task = LatestTask::new("idle");
            let state = task.state();
            let completion = task.begin("loading");

            assert!(task.try_cancel("cancelled"));

            assert!(!completion.is_current());
            assert!(!completion.complete("late result"));
            assert_eq!(state.get(), "cancelled");
        }

        // Case `latest_request_drains_only_the_current_completion`.
        {
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

        // Case `latest_request_cancel_rejects_late_work`.
        {
            let mut request = LatestRequest::new();
            let completion = request.begin();

            assert!(request.try_cancel());

            assert!(!completion.is_current());
            assert!(!completion.complete(42));
            assert!(!request.is_pending());
            assert_eq!(request.try_take(), None);
        }
    }

    #[test]
    fn exhausted_revisions_and_generations_are_never_reissued() {
        // Case `exhausted_local_state_revision_leaves_value_and_callback_untouched`.
        {
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

        // Case `exhausted_latest_request_never_reissues_a_completion_generation`.
        {
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
            let stale = request.begin();
            assert!(!stale.is_current());
            assert!(!stale.complete(()));
        }

        // Case `exhausted_latest_task_does_not_publish_a_new_run`.
        {
            let task = LatestTask::new("idle");
            let state = task.state();
            *task.generation.lock().unwrap() = u64::MAX - 1;

            assert!(task.try_begin("running").is_none());
            assert_eq!(state.get(), "idle");
            assert!(task.try_cancel("cancelled"));
            assert_eq!(state.get(), "cancelled");
            assert!(!task.try_cancel("later"));
            assert_eq!(state.get(), "cancelled");
            let inert_completion = task.begin("never published");
            assert!(!inert_completion.is_current());
            assert!(!inert_completion.complete("late"));
            assert_eq!(state.get(), "cancelled");
        }

        // Case `closed_zero_generation_fallbacks_are_permanently_stale`.
        {
            let mut scope = WakeScope::new();
            let (task, mut request) = {
                let _entry = scope.enter();
                (LatestTask::new(0), LatestRequest::<u32>::new())
            };
            scope.close();
            let task_fallback = task.begin(1);
            let request_fallback = request.begin();
            assert!(!task_fallback.is_current());
            assert!(!request_fallback.is_current());
            assert!(!task_fallback.complete(2));
            assert!(!request_fallback.complete(2));
            assert_eq!(*task.generation.lock().unwrap(), 0);
            assert_eq!(request.generation.load(Ordering::Acquire), 0);
            assert_eq!(task.state().get(), 0);
        }
    }

    #[test]
    fn writers_tasks_and_requests_keep_the_state_creation_owner() {
        // Case `writer_keeps_creation_owner_when_requested_under_another_scope`.
        {
            let (mut a, a_rx) = bound_scope();
            let (b, b_rx) = bound_scope();
            let state = {
                let _entry = a.enter();
                LocalState::new(0)
            };
            let writer = {
                let _entry = b.enter();
                state.writer()
            };
            assert!(
                std::thread::spawn(move || writer.try_set(1))
                    .join()
                    .unwrap()
            );
            assert_eq!(state.get(), 1);
            assert_eq!(state.revision(), 1);
            assert!(a.accepts(&a_rx.try_recv().unwrap()));
            assert!(matches!(b_rx.try_recv(), Err(TryRecvError::Empty)));
            a.close();
            b.handle().wake();
            assert!(b.accepts(&b_rx.try_recv().unwrap()));
        }

        // Case `task_begin_under_another_scope_keeps_the_state_creation_owner`.
        {
            let (a, a_rx) = bound_scope();
            let (b, b_rx) = bound_scope();
            let task = {
                let _entry = a.enter();
                LatestTask::new(0)
            };
            let completion = {
                let _entry = b.enter();
                task.begin(1)
            };
            assert!(
                std::thread::spawn(move || completion.complete(2))
                    .join()
                    .unwrap()
            );
            assert!(a.accepts(&a_rx.try_recv().unwrap()));
            assert!(matches!(b_rx.try_recv(), Err(TryRecvError::Empty)));
            assert_eq!(task.state().get(), 2);
        }

        // Case `request_begin_keeps_owner_and_closed_inbox_discards_queued_result`.
        {
            let (mut a, a_rx) = bound_scope();
            let (b, b_rx) = bound_scope();
            let mut request = {
                let _entry = a.enter();
                LatestRequest::new()
            };
            let completion = {
                let _entry = b.enter();
                request.begin()
            };
            assert!(
                std::thread::spawn(move || completion.complete(42))
                    .join()
                    .unwrap()
            );
            let event = a_rx.try_recv().unwrap();
            assert!(a.accepts(&event));
            assert!(matches!(b_rx.try_recv(), Err(TryRecvError::Empty)));
            a.close();
            assert_eq!(request.try_take(), None);
            assert!(!request.is_pending());
            assert!(!a.accepts(&event));
        }

        // Case `update_may_publish_second_state_and_close_its_scope`.
        {
            let (mut scope, rx) = bound_scope();
            let (first, second) = {
                let _entry = scope.enter();
                (LocalState::new(0), LocalState::new(0))
            };
            assert!(first.writer().try_update(|value| {
                assert!(second.writer().try_set(2));
                scope.close();
                *value = 1;
            }));
            assert_eq!(first.get(), 1);
            assert_eq!(second.get(), 2);
            let old = rx.try_recv().unwrap();
            assert!(!scope.accepts(&old));
            assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
        }
    }

    #[test]
    fn closing_the_state_rejects_later_publications_and_work() {
        // Case `closed_state_rejects_direct_writer_and_binding_publications`.
        {
            let (mut scope, rx) = bound_scope();
            let state = {
                let _entry = scope.enter();
                LocalState::new(7)
            };
            let writer = state.writer();
            let (_, mut binding) = LocalBinding::new(&state);
            scope.close();
            assert!(!state.try_set(8));
            assert!(!state.try_update(|_| panic!("closed direct callback ran")));
            assert!(!writer.try_set(9));
            assert!(!writer.try_update(|_| panic!("closed writer callback ran")));
            binding.publish(&10);
            assert_eq!(state.get(), 7);
            assert_eq!(state.revision(), 0);
            assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
        }

        // Case `authorized_update_can_finish_after_nonblocking_close`.
        {
            let (mut scope, rx) = bound_scope();
            let state = {
                let _entry = scope.enter();
                LocalState::new(1)
            };
            let writer = state.writer();
            let (entered_tx, entered_rx) = mpsc::channel();
            let (resume_tx, resume_rx) = mpsc::channel();
            let worker = std::thread::spawn(move || {
                writer.try_update(|value| {
                    entered_tx.send(()).unwrap();
                    tilo_test_env::recv("the resume signal", &resume_rx);
                    *value = 2;
                })
            });
            tilo_test_env::recv("the mutation callback to start", &entered_rx);
            // If close waits for the mutation callback, the hang-bounded wait fails the test.
            scope.close();
            let replacement_scope = WakeScope::new();
            let replacement = {
                let _entry = replacement_scope.enter();
                LocalState::new(100)
            };
            resume_tx.send(()).unwrap();
            assert!(worker.join().unwrap());
            assert_eq!(state.get(), 2);
            assert_eq!(state.revision(), 1);
            assert_eq!(replacement.get(), 100);
            assert_eq!(replacement.revision(), 0);
            assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
        }

        // Case `close_rejects_task_and_request_without_explicit_cancel`.
        {
            let (mut scope, rx) = bound_scope();
            let (task, mut request) = {
                let _entry = scope.enter();
                (LatestTask::new(0), LatestRequest::new())
            };
            let task_completion = task.begin(1);
            let request_completion = request.begin();
            scope.close();
            assert!(!task_completion.is_current());
            assert!(!request_completion.is_current());
            assert!(!task_completion.complete(2));
            assert!(!request_completion.complete(3));
            assert_eq!(task.state().get(), 1);
            assert!(!request.is_pending());
            assert_eq!(request.try_take(), None);
            assert!(task.try_begin(4).is_none());
            assert!(request.try_begin().is_none());
            assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
        }

        // Case `dropped_owner_leaves_all_retained_capabilities_closed`.
        {
            let (state, task, mut request, task_completion, request_completion) = {
                let scope = WakeScope::new();
                let _entry = scope.enter();
                let state = LocalState::new(0);
                let task = LatestTask::new(0);
                let mut request = LatestRequest::new();
                let task_completion = task.begin(1);
                let request_completion = request.begin();
                (state, task, request, task_completion, request_completion)
            };
            assert!(!state.writer().try_set(2));
            assert!(!task_completion.complete(2));
            assert!(!request_completion.complete(2));
            assert_eq!(task.state().get(), 1);
            assert_eq!(request.try_take(), None);
        }
    }

    #[test]
    fn spawned_work_runs_off_the_ui_thread_and_wakes_after_unlocking() {
        // Case `task_wake_sink_runs_after_generation_lock_is_released`.
        {
            let mut scope = WakeScope::new();
            let task = {
                let _entry = scope.enter();
                LatestTask::new(0)
            };
            let reentrant_task = task.clone();
            scope
                .bind(move |_| {
                    assert!(reentrant_task.try_cancel(3));
                })
                .unwrap();
            assert!(task.begin(1).complete(2));
            assert_eq!(task.state().get(), 3);
        }

        // Case `spawned_work_never_runs_on_the_ui_thread`.
        {
            on_ui_thread(|scope, wakes| {
                let ui_thread = std::thread::current().id();
                let mut request = LatestRequest::new();
                // Held until the checks below, so the result cannot already
                // be there when they look.
                let (release, gate) = tokio::sync::oneshot::channel::<()>();
                assert!(request.spawn(async move {
                    // Cross an await point, so a runtime driven from the caller
                    // would have to be polled again here.
                    let _ = gate.await;
                    let current = std::thread::current();
                    (
                        current.id(),
                        current.name().map(str::to_owned),
                        tilo_rt::is_ui_thread(),
                    )
                }));
                // Spawning neither entered a runtime on the UI thread nor ran the
                // future inline.
                assert!(!tilo_rt::in_runtime());
                assert!(request.try_take().is_none());
                release.send(()).unwrap();

                let event = wakes.recv_timeout(tilo_test_env::hang_bound()).unwrap();
                assert!(scope.accepts(&event));
                let (ran_on, name, ran_on_ui) = request.try_take().unwrap();
                assert_ne!(ran_on, ui_thread);
                assert_eq!(ran_on, tilo_rt::app().thread_id());
                assert_eq!(name.as_deref(), Some("tilo-rt"));
                assert!(!ran_on_ui);
                // The UI thread can never block on the runtime either.
                assert!(matches!(
                    tilo_rt::app().block_on(async {}),
                    Err(tilo_rt::Error::UiThread)
                ));
            });
        }

        // Case `spawned_results_wake_the_event_loop_and_stale_runs_are_aborted`.
        {
            on_ui_thread(|scope, wakes| {
                let mut request = LatestRequest::new();
                let (started, started_rx) = mpsc::channel();
                let (_never, hold) = tokio::sync::oneshot::channel::<()>();
                let (dropped, dropped_rx) = mpsc::channel();
                let guard = DropSignal(dropped);
                assert!(request.spawn(async move {
                    let _guard = guard;
                    started.send(()).unwrap();
                    let _ = hold.await;
                    "stale"
                }));
                started_rx
                    .recv_timeout(tilo_test_env::hang_bound())
                    .unwrap();
                // A newer request supersedes the parked one: it is aborted, and
                // only the newer result reaches the UI.
                assert!(request.spawn(async { "fresh" }));
                dropped_rx
                    .recv_timeout(tilo_test_env::hang_bound())
                    .unwrap();
                let event = wakes.recv_timeout(tilo_test_env::hang_bound()).unwrap();
                assert!(scope.accepts(&event));
                assert_eq!(request.try_take(), Some("fresh"));

                let task = LatestTask::new("idle");
                let state = task.state();
                let (release, gate) = tokio::sync::oneshot::channel::<()>();
                assert!(task.spawn("loading", async move {
                    let _ = gate.await;
                    "loaded"
                }));
                // The running state is published before the future runs.
                assert_eq!(state.get(), "loading");
                release.send(()).unwrap();
                let event = wakes.recv_timeout(tilo_test_env::hang_bound()).unwrap();
                assert!(scope.accepts(&event));
                assert_eq!(state.get(), "loaded");
            });
        }
    }

    /// Stands in for the event loop: a thread marked as the UI thread whose
    /// wake scope is bound to a channel, as the native runner binds it to
    /// its event-loop proxy.
    fn on_ui_thread(body: impl FnOnce(&WakeScope, &Receiver<WakeEvent>) + Send + 'static) {
        std::thread::Builder::new()
            .name("ui-event-loop".into())
            .spawn(move || {
                tilo_rt::mark_ui_thread();
                let (scope, wakes) = bound_scope();
                let _entry = scope.enter();
                body(&scope, &wakes);
            })
            .unwrap()
            .join()
            .unwrap();
    }

    struct DropSignal(Sender<()>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            let _ = self.0.send(());
        }
    }
}

/// The queue a view's controls write what they ask for to, and its app
/// applies in one ordered place after the event: a click, a key and an
/// accessibility action all become the same intent. Clones share the queue.
pub struct Intents<T>(std::rc::Rc<std::cell::RefCell<Vec<T>>>);

impl<T> Clone for Intents<T> {
    fn clone(&self) -> Self {
        Self(std::rc::Rc::clone(&self.0))
    }
}

impl<T> Default for Intents<T> {
    fn default() -> Self {
        Self(std::rc::Rc::default())
    }
}

impl<T> Intents<T> {
    /// Queue `intent`.
    pub fn push(&self, intent: T) {
        self.0.borrow_mut().push(intent);
    }

    /// Everything queued, oldest first, leaving the queue empty.
    #[must_use]
    pub fn take(&self) -> Vec<T> {
        std::mem::take(&mut *self.0.borrow_mut())
    }
}

impl<T: Clone + 'static> Intents<T> {
    /// A callback that queues `intent` each time it runs, for a control's
    /// `on_view` / `on_effect`.
    pub fn sender(&self, intent: T) -> impl FnMut() + 'static {
        let intents = self.clone();
        move || intents.push(intent.clone())
    }
}
