//! Instance-scoped background wake routing, shared by native and headless hosts:
//! a scope whose owner is woken when work lands for it. No async runtime is
//! linked, so a document model can route wakes in a sandboxed wasm build.
use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender, channel};
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use tilo_lock::MutexRecover as _;

type Sink = Arc<dyn Fn(WakeEvent) + Send + Sync>;

#[derive(Default)]
struct Destination {
    sink: Option<Sink>,
    pending: bool,
    pending_sync: bool,
    settling: Settling,
}

#[derive(Default)]
struct Settling {
    owed: usize,
    replied: bool,
    woken: bool,
}

struct Route {
    open: AtomicBool,
    destination: Mutex<Destination>,
    changed: Condvar,
    /// Bumped on every wake and every noted change, never reset, so comparing it
    /// before and after proves nothing reached the owner in between.
    revision: AtomicU64,
    crossed: AtomicU64,
    thread: std::thread::ThreadId,
    /// The scope this one was made inside, whose revision it moves as well.
    parent: Option<Weak<Route>>,
}

impl Route {
    fn moved(&self) {
        let crossed = std::thread::current().id() != self.thread;
        self.bump(crossed);
        let mut parent = self.parent.as_ref().and_then(Weak::upgrade);
        while let Some(route) = parent {
            route.bump(crossed);
            parent = route.parent.as_ref().and_then(Weak::upgrade);
        }
    }

    fn bump(&self, crossed: bool) {
        self.revision.fetch_add(1, Ordering::AcqRel);
        if crossed {
            self.crossed.fetch_add(1, Ordering::AcqRel);
        }
    }
}

thread_local! {
    static CURRENT: RefCell<Option<Weak<Route>>> = const { RefCell::new(None) };
    /// How deep the running passes of a component that wires work off the frame are.
    static FEEDS_COMPONENT: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Marks the running component as one that wires work off the frame, so a debug
/// build fails where such a wake is made.
#[doc(hidden)]
#[must_use]
pub struct FeedsComponent(());

impl FeedsComponent {
    pub fn begin() -> Self {
        FEEDS_COMPONENT.with(|feeds| feeds.set(feeds.get().saturating_add(1)));
        Self(())
    }
}

impl Drop for FeedsComponent {
    fn drop(&mut self) {
        FEEDS_COMPONENT.with(|feeds| feeds.set(feeds.get().saturating_sub(1)));
    }
}

pub struct WakeScope {
    route: Arc<Route>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum WakeBindError {
    #[error("wake scope already has a destination")]
    AlreadyBound,
    #[error("wake scope is closed")]
    Closed,
}

impl Default for WakeScope {
    fn default() -> Self {
        Self::new()
    }
}

impl WakeScope {
    pub fn new() -> Self {
        Self {
            route: Arc::new(Route {
                open: AtomicBool::new(true),
                destination: Mutex::new(Destination::default()),
                changed: Condvar::new(),
                revision: AtomicU64::new(0),
                crossed: AtomicU64::new(0),
                thread: std::thread::current().id(),
                parent: CURRENT.with(|current| current.borrow().clone()),
            }),
        }
    }

    pub fn revision(&self) -> u64 {
        self.route.revision.load(Ordering::Acquire)
    }

    pub fn handle(&self) -> WakeHandle {
        WakeHandle {
            route: Some(Arc::downgrade(&self.route)),
        }
    }

    pub fn enter(&self) -> WakeScopeGuard {
        let previous = CURRENT.with(|current| current.replace(Some(Arc::downgrade(&self.route))));
        WakeScopeGuard {
            previous,
            thread_affinity: PhantomData,
        }
    }

    pub fn enter_detached() -> WakeScopeGuard {
        let previous = CURRENT.with(|current| current.replace(None));
        WakeScopeGuard {
            previous,
            thread_affinity: PhantomData,
        }
    }

    /// Bind a destination for this scope's wakes, flushing work that arrived before
    /// startup. The sink must capture a sender or proxy, never an owning host
    /// reference.
    ///
    /// # Errors
    ///
    /// [`WakeBindError::Closed`] once the scope has closed,
    /// [`WakeBindError::AlreadyBound`] when a destination is already installed.
    pub fn bind(
        &mut self,
        sink: impl Fn(WakeEvent) + Send + Sync + 'static,
    ) -> Result<(), WakeBindError> {
        let (pending, requires_sync) = {
            let mut destination = self.route.destination.lock_recover();
            if !self.route.open.load(Ordering::Acquire) {
                return Err(WakeBindError::Closed);
            }
            if destination.sink.is_some() {
                return Err(WakeBindError::AlreadyBound);
            }
            destination.sink = Some(Arc::new(sink));
            (
                std::mem::take(&mut destination.pending),
                std::mem::take(&mut destination.pending_sync),
            )
        };
        if pending {
            self.handle().send_wake(requires_sync, false);
        }
        Ok(())
    }

    /// Wakes recorded while no destination was bound, as `(pending, requires_sync)`.
    /// `None` when they are not observable here, so a caller must assume a
    /// model-dirty wake.
    pub fn take_unbound(&self) -> Option<(bool, bool)> {
        let mut destination = self.route.destination.lock_recover();
        if !self.route.open.load(Ordering::Acquire) || destination.sink.is_some() {
            return None;
        }
        Some((
            std::mem::take(&mut destination.pending),
            std::mem::take(&mut destination.pending_sync),
        ))
    }

    /// Take the owner's coalesced wake flags, including the wakes its event sink
    /// received; `None` once the scope has closed.
    pub fn take_pending(&self) -> Option<(bool, bool)> {
        let mut destination = self.route.destination.lock_recover();
        if !self.route.open.load(Ordering::Acquire) {
            return None;
        }
        Some((
            std::mem::take(&mut destination.pending),
            std::mem::take(&mut destination.pending_sync),
        ))
    }

    pub fn is_pending(&self) -> bool {
        self.route.open.load(Ordering::Acquire) && self.route.destination.lock_recover().pending
    }

    /// Whether the waiting wake asks for a model pass, without taking it: an owner
    /// that drains wakes mid-pass keeps it pending.
    pub fn sync_pending(&self) -> Option<bool> {
        if !self.route.open.load(Ordering::Acquire) {
            return None;
        }
        Some(self.route.destination.lock_recover().pending_sync)
    }

    pub fn accepts(&self, event: &WakeEvent) -> bool {
        self.route.open.load(Ordering::Acquire)
            && Weak::ptr_eq(&Arc::downgrade(&self.route), &event.route)
    }

    pub fn close(&mut self) {
        self.route.open.swap(false, Ordering::AcqRel);
        drop(self.route.destination.lock_recover());
        self.route.changed.notify_all();
    }
}

impl Drop for WakeScope {
    fn drop(&mut self) {
        self.close();
    }
}

/// A thread-affine construction and dispatch guard, independent of an App
/// borrow.
///
/// ```compile_fail
/// let scope = tilo_rt::wake::WakeScope::new();
/// let guard = scope.enter();
/// std::thread::spawn(move || drop(guard));
/// ```
pub struct WakeScopeGuard {
    previous: Option<Weak<Route>>,
    thread_affinity: PhantomData<Rc<()>>,
}

impl Drop for WakeScopeGuard {
    fn drop(&mut self) {
        CURRENT.with(|current| {
            current.replace(self.previous.take());
        });
    }
}

/// A weak identity for a queued wake; it never owns an app or its scope.
#[derive(Clone, Debug)]
pub struct WakeEvent {
    route: Weak<Route>,
    requires_sync: bool,
    save_progress: bool,
}

impl WakeEvent {
    #[doc(hidden)]
    pub const fn requires_sync(&self) -> bool {
        self.requires_sync
    }

    /// Says only that a record's save moved on, so an owner may show the new status
    /// without running its model.
    #[doc(hidden)]
    pub const fn save_progress(&self) -> bool {
        self.save_progress
    }

    /// Whether the wake also asks for a frame. A save-progress wake does not, so a
    /// keystroke's write landing draws no second frame.
    pub const fn asks_for_frame(&self) -> bool {
        !self.save_progress
    }
}

#[derive(Clone, Debug)]
pub struct WakeHandle {
    route: Option<Weak<Route>>,
}

impl Default for WakeHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl WakeHandle {
    #[doc(hidden)]
    pub fn accepts(&self, event: &WakeEvent) -> bool {
        self.is_active()
            && self
                .route
                .as_ref()
                .is_some_and(|route| Weak::ptr_eq(route, &event.route))
    }

    #[doc(hidden)]
    pub fn enter_if_open(&self) -> Result<WakeScopeGuard, WakeBindError> {
        let route = self.route.as_ref().ok_or(WakeBindError::Closed)?;
        if !route
            .upgrade()
            .is_some_and(|route| route.open.load(Ordering::Acquire))
        {
            return Err(WakeBindError::Closed);
        }
        let previous = CURRENT.with(|current| current.replace(Some(route.clone())));
        Ok(WakeScopeGuard {
            previous,
            thread_affinity: PhantomData,
        })
    }

    pub fn new() -> Self {
        let handle = Self {
            route: CURRENT.with(|current| current.borrow().clone()),
        };
        debug_assert!(
            !(handle.is_detached() && FEEDS_COMPONENT.with(std::cell::Cell::get) > 0),
            "work a component starts captured a wake that reaches no component: take the wake \
             from the component that runs now, not from a construction that already ended"
        );
        handle
    }

    pub const fn detached() -> Self {
        Self { route: None }
    }

    pub fn scope_key(&self) -> Option<usize> {
        self.route
            .as_ref()
            .map(|route| route.as_ptr().cast::<()>() as usize)
    }

    pub const fn is_detached(&self) -> bool {
        self.route.is_none()
    }

    pub fn is_active(&self) -> bool {
        match &self.route {
            None => true,
            Some(route) => route
                .upgrade()
                .is_some_and(|route| route.open.load(Ordering::Acquire)),
        }
    }

    /// Compare two original scoped routes while the route is open; detached handles
    /// never match. Scheduling metadata, not authorization.
    #[doc(hidden)]
    pub fn same_open_scope(&self, other: &Self) -> bool {
        let (Some(route), Some(other_route)) = (&self.route, &other.route) else {
            return false;
        };
        Weak::ptr_eq(route, other_route)
            && route
                .upgrade()
                .is_some_and(|route| route.open.load(Ordering::Acquire))
    }

    #[doc(hidden)]
    pub fn authorize_publication(&self) -> bool {
        match &self.route {
            None => true,
            Some(route) => route.upgrade().is_some_and(|route| {
                route
                    .open
                    .compare_exchange(true, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
            }),
        }
    }

    pub fn wake(&self) {
        self.send_wake(true, false);
    }

    /// Note on the owner's thread that something it reads changed, without waking
    /// it: a pass that is running or due takes it up.
    pub fn note_change(&self) {
        if let Some(route) = self.route.as_ref().and_then(Weak::upgrade)
            && route.open.load(Ordering::Acquire)
        {
            route.moved();
        }
    }

    pub fn revision(&self) -> Option<u64> {
        let route = self.route.as_ref()?.upgrade()?;
        route
            .open
            .load(Ordering::Acquire)
            .then(|| route.revision.load(Ordering::Acquire))
    }

    pub fn crossed(&self) -> Option<u64> {
        let route = self.route.as_ref()?.upgrade()?;
        route
            .open
            .load(Ordering::Acquire)
            .then(|| route.crossed.load(Ordering::Acquire))
    }

    /// Wake the owner because a record's save moved on, and for nothing else.
    pub fn wake_save_progress(&self) {
        self.send_wake(true, true);
    }

    #[doc(hidden)]
    pub fn notify_events(&self) {
        self.send_wake(false, false);
    }

    /// Owe the owner a reply: the work holds the [`Owed`] and drops it once its
    /// answer is queued.
    pub fn owe(&self) -> Owed {
        let route = self.route.as_ref().filter(|route| {
            route.upgrade().is_some_and(|route| {
                let mut destination = route.destination.lock_recover();
                let open = route.open.load(Ordering::Acquire);
                if open {
                    destination.settling.owed = destination.settling.owed.saturating_add(1);
                }
                open
            })
        });
        Owed {
            route: route.cloned(),
        }
    }

    pub fn owes(&self) -> bool {
        self.route
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some_and(|route| {
                route.open.load(Ordering::Acquire)
                    && route.destination.lock_recover().settling.owed > 0
            })
    }

    /// Mark where a pass begins, so [`Self::settled`] asks only about replies that
    /// land after it.
    pub fn begin_pass(&self) {
        if let Some(route) = self.route.as_ref().and_then(Weak::upgrade) {
            let mut destination = route.destination.lock_recover();
            destination.settling.replied = false;
            destination.settling.woken = false;
        }
    }

    /// Whether nothing is owed and no reply landed since [`Self::begin_pass`].
    pub fn settled(&self) -> bool {
        self.route
            .as_ref()
            .and_then(Weak::upgrade)
            .is_none_or(|route| {
                let destination = route.destination.lock_recover();
                !route.open.load(Ordering::Acquire)
                    || (destination.settling.owed == 0 && !destination.settling.replied)
            })
    }

    /// Run `pass` until nothing is owed and a pass ran after the last reply landed.
    /// One pass always runs, then it waits to be woken; it never polls.
    pub fn settle(&self, mut pass: impl FnMut()) {
        let Some(route) = self.route.as_ref().and_then(Weak::upgrade) else {
            pass();
            return;
        };
        loop {
            self.begin_pass();
            pass();
            let mut destination = route.destination.lock_recover();
            loop {
                if !route.open.load(Ordering::Acquire) {
                    return;
                }
                if destination.settling.replied {
                    break;
                }
                if destination.settling.owed == 0 {
                    return;
                }
                if destination.settling.woken {
                    break;
                }
                destination = route
                    .changed
                    .wait(destination)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        }
    }

    fn send_wake(&self, requires_sync: bool, save_progress: bool) {
        if !self.authorize_publication() {
            return;
        }
        let Some(weak) = &self.route else {
            return;
        };
        let Some(route) = weak.upgrade() else {
            return;
        };
        let sink = {
            let mut destination = route.destination.lock_recover();
            if !route.open.load(Ordering::Acquire) {
                return;
            }
            destination.pending = true;
            destination.pending_sync |= requires_sync;
            destination.settling.woken = true;
            route.moved();
            destination.sink.as_ref().map(Arc::clone)
        };
        route.changed.notify_all();
        if let Some(sink) = sink {
            sink(WakeEvent {
                route: weak.clone(),
                requires_sync,
                save_progress,
            });
        }
    }
}

#[must_use = "dropping it at once says the reply already landed"]
pub struct Owed {
    route: Option<Weak<Route>>,
}

impl std::fmt::Debug for Owed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Owed").finish_non_exhaustive()
    }
}

impl Drop for Owed {
    fn drop(&mut self) {
        let Some(weak) = self.route.take() else {
            return;
        };
        let Some(route) = weak.upgrade() else {
            return;
        };
        {
            let mut destination = route.destination.lock_recover();
            destination.settling.owed = destination.settling.owed.saturating_sub(1);
            destination.settling.replied = true;
        }
        route.changed.notify_all();
        WakeHandle { route: Some(weak) }.wake();
    }
}

/// The answering end of a request made under a wake scope; sending the answer
/// wakes that scope's event loop.
pub struct Reply<T> {
    sender: Sender<T>,
    wake: WakeHandle,
}

pub fn reply<T>() -> (Reply<T>, Receiver<T>) {
    let (sender, receiver) = channel();
    (
        Reply {
            sender,
            wake: WakeHandle::new(),
        },
        receiver,
    )
}

impl<T> Reply<T> {
    pub fn send(self, answer: T) {
        if self.sender.send(answer).is_ok() {
            self.wake.wake();
        }
    }
}

/// Wakes the component that opened a [`waking_channel`] for every message,
/// before the message can be received and again once it can.
struct Reporting {
    wake: WakeHandle,
}

impl Reporting {
    fn new() -> Self {
        Self {
            wake: WakeHandle::new(),
        }
    }

    fn announce<T>(
        &self,
        queue: impl FnOnce() -> Result<(), std::sync::mpsc::SendError<T>>,
    ) -> Result<(), std::sync::mpsc::SendError<T>> {
        self.wake.wake();
        let queued = queue();
        if queued.is_ok() {
            self.wake.wake();
        }
        queued
    }
}

impl Clone for Reporting {
    fn clone(&self) -> Self {
        Self {
            wake: self.wake.clone(),
        }
    }
}

/// The worker's end of a [`waking_channel`]: every send wakes the scope that
/// opened it.
pub struct WakingSender<T> {
    sender: Sender<T>,
    reporting: Reporting,
}

impl<T> Clone for WakingSender<T> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            reporting: self.reporting.clone(),
        }
    }
}

impl<T> WakingSender<T> {
    pub fn send(&self, message: T) -> Result<(), std::sync::mpsc::SendError<T>> {
        let sender = &self.sender;
        self.reporting.announce(|| sender.send(message))
    }
}

impl<T> Drop for WakingSender<T> {
    /// A worker that ends without replying wakes the owner too, so it sees the
    /// channel disconnected.
    fn drop(&mut self) {
        self.reporting.wake.wake();
    }
}

/// The worker's end of a [`waking_sync_channel`], which queues no faster than
/// its owner takes.
pub struct WakingSyncSender<T> {
    sender: SyncSender<T>,
    reporting: Reporting,
}

impl<T> Clone for WakingSyncSender<T> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            reporting: self.reporting.clone(),
        }
    }
}

impl<T> WakingSyncSender<T> {
    pub fn send(&self, message: T) -> Result<(), std::sync::mpsc::SendError<T>> {
        let sender = &self.sender;
        self.reporting.announce(|| sender.send(message))
    }
}

impl<T> Drop for WakingSyncSender<T> {
    fn drop(&mut self) {
        self.reporting.wake.wake();
    }
}

/// A channel opened under the current wake scope: every reply a worker sends
/// wakes the app's loop, which drains it on its next sync.
#[must_use]
pub fn waking_channel<T>() -> (WakingSender<T>, Receiver<T>) {
    let (sender, receiver) = channel();
    (
        WakingSender {
            sender,
            reporting: Reporting::new(),
        },
        receiver,
    )
}

/// [`waking_channel`] with room for `bound` messages, so a worker sending costly
/// replies waits for its owner to take them.
#[must_use]
pub fn waking_sync_channel<T>(bound: usize) -> (WakingSyncSender<T>, Receiver<T>) {
    let (sender, receiver) = std::sync::mpsc::sync_channel(bound);
    (
        WakingSyncSender {
            sender,
            reporting: Reporting::new(),
        },
        receiver,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{self, Receiver, TryRecvError};

    #[test]
    fn a_bound_owner_observes_model_wakes_without_losing_sink_delivery() {
        let (mut scope, rx) = bound_scope();
        assert_eq!(scope.take_pending(), Some((false, false)));
        scope.handle().notify_events();
        assert!(!rx.recv().unwrap().requires_sync());
        assert_eq!(scope.take_pending(), Some((true, false)));
        scope.handle().wake();
        scope.handle().notify_events();
        assert!(rx.recv().unwrap().requires_sync());
        assert!(!rx.recv().unwrap().requires_sync());
        assert_eq!(scope.take_pending(), Some((true, true)));
        assert_eq!(scope.take_pending(), Some((false, false)));
        assert_eq!(scope.take_unbound(), None);
        scope.close();
        scope.handle().wake();
        assert_eq!(scope.take_pending(), None);
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn every_wake_and_noted_change_moves_the_revision_whoever_drains_the_wakes() {
        let (scope, _rx) = bound_scope();
        let handle = scope.handle();
        let start = handle.revision().unwrap();
        handle.notify_events();
        let _ = scope.take_pending();
        assert_ne!(
            handle.revision(),
            Some(start),
            "a drained wake still moved it"
        );
        let woken = handle.revision();
        handle.note_change();
        assert_ne!(handle.revision(), woken, "a noted change moves it");
        assert_eq!(scope.take_pending(), Some((false, false)), "without waking");
        assert_eq!(WakeHandle::detached().revision(), None);
        let noted = scope.revision();
        drop(scope);
        handle.note_change();
        assert_eq!(handle.revision(), None, "a closed scope has none");
        assert!(noted >= start + 2);
    }

    #[test]
    fn only_a_move_from_another_thread_counts_as_crossed() {
        let (scope, _rx) = bound_scope();
        let handle = scope.handle();
        handle.notify_events();
        handle.note_change();
        assert_eq!(handle.crossed(), Some(0), "the owner's own thread");
        let remote = handle.clone();
        std::thread::spawn(move || remote.notify_events())
            .join()
            .unwrap();
        assert_eq!(handle.crossed(), Some(1), "a wake from a worker");
    }

    #[test]
    fn a_scope_made_inside_another_moves_the_enclosing_revision() {
        let outer = WakeScope::new();
        let inner = {
            let _entry = outer.enter();
            WakeScope::new()
        };
        let sibling = WakeScope::new();
        let (before, beside) = (outer.revision(), sibling.revision());
        inner.handle().note_change();
        assert_ne!(outer.revision(), before, "the enclosing scope moved");
        assert_eq!(sibling.revision(), beside, "a scope beside it did not");
        let moved = outer.revision();
        outer.handle().note_change();
        assert_eq!(
            inner.revision(),
            1,
            "an enclosing change moves no scope inside it"
        );
        assert_ne!(outer.revision(), moved);
    }

    #[test]
    fn wakes_coalesce_before_binding_and_bind_only_once() {
        {
            for model_changed in [false, true] {
                let mut scope = WakeScope::new();
                scope.handle().notify_events();
                if model_changed {
                    scope.handle().wake();
                }
                scope.handle().notify_events();
                let (tx, rx) = mpsc::channel();
                scope
                    .bind(move |event| {
                        tx.send(event).unwrap();
                    })
                    .unwrap();
                let event = rx.try_recv().unwrap();
                assert_eq!(event.requires_sync(), model_changed);
                assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
            }
        }

        {
            let mut scope = WakeScope::new();
            let worker = scope.handle();
            std::thread::spawn(move || {
                for _ in 0..8 {
                    worker.wake();
                }
            })
            .join()
            .unwrap();
            let (tx, rx) = mpsc::channel();
            scope
                .bind(move |event| {
                    tx.send(event).unwrap();
                })
                .unwrap();
            assert!(scope.accepts(&rx.try_recv().unwrap()));
            assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
            scope.handle().wake();
            assert!(scope.accepts(&rx.try_recv().unwrap()));
        }

        {
            let (mut scope, rx) = bound_scope();
            assert_eq!(
                scope.bind(|_| panic!("replacement sink called")),
                Err(WakeBindError::AlreadyBound)
            );
            scope.handle().wake();
            assert!(scope.accepts(&rx.try_recv().unwrap()));
            scope.close();
            assert_eq!(scope.bind(|_| {}), Err(WakeBindError::Closed));
        }
    }

    fn bound_scope() -> (WakeScope, Receiver<WakeEvent>) {
        let (tx, rx) = mpsc::channel();
        let mut scope = WakeScope::new();
        scope
            .bind(move |event| {
                tx.send(event).unwrap();
            })
            .unwrap();
        (scope, rx)
    }

    fn drain(rx: &Receiver<WakeEvent>) {
        while rx.try_recv().is_ok() {}
    }

    #[test]
    fn a_worker_reply_on_a_waking_channel_wakes_its_owner() {
        let (scope, rx) = bound_scope();
        {
            let (sender, replies) = {
                let _entry = scope.enter();
                waking_channel::<u32>()
            };
            std::thread::spawn(move || sender.send(7).unwrap())
                .join()
                .unwrap();
            assert!(
                rx.recv().unwrap().requires_sync(),
                "the reply woke the owner"
            );
            assert_eq!(replies.try_recv().unwrap(), 7);
            assert!(rx.recv().is_ok());
            drain(&rx);
        }
        {
            let (sender, replies) = {
                let _entry = scope.enter();
                waking_channel::<u32>()
            };
            drop(replies);
            assert!(sender.send(1).is_err(), "the message reaches nobody");
            assert!(rx.try_recv().is_ok(), "the attempt was announced");
            assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
            drop(sender);
            drain(&rx);
        }
        {
            let (sender, replies) = {
                let _entry = scope.enter();
                waking_channel::<u32>()
            };
            std::thread::spawn(move || drop(sender)).join().unwrap();
            assert!(
                rx.recv().is_ok(),
                "a worker that ends without a reply wakes too"
            );
            assert!(matches!(
                replies.try_recv(),
                Err(std::sync::mpsc::TryRecvError::Disconnected)
            ));
            drain(&rx);
        }
    }

    #[test]
    fn a_worker_reply_on_a_bounded_waking_channel_wakes_its_owner() {
        let (scope, rx) = bound_scope();
        let (sender, replies) = {
            let _entry = scope.enter();
            waking_sync_channel::<u32>(1)
        };
        std::thread::spawn(move || sender.send(7).unwrap())
            .join()
            .unwrap();
        assert!(
            rx.recv().unwrap().requires_sync(),
            "the reply woke its owner before it could be received"
        );
        assert_eq!(replies.try_recv().unwrap(), 7);
        assert!(rx.recv().is_ok());
    }

    #[test]
    #[should_panic(expected = "reaches no component")]
    fn a_wake_wired_for_a_running_component_reaches_its_component() {
        let _feeds = super::FeedsComponent::begin();
        let _detached = WakeHandle::new();
    }

    #[test]
    fn a_wake_wired_inside_a_scope_reaches_its_component() {
        let (scope, rx) = bound_scope();
        let feeds = super::FeedsComponent::begin();
        let entry = scope.enter();
        let handle = WakeHandle::new();
        drop(entry);
        drop(feeds);
        handle.wake();
        assert!(scope.accepts(&rx.try_recv().unwrap()));
    }

    #[test]
    fn handles_keep_their_owner_and_never_keep_a_scope_alive() {
        {
            let (mut a, a_rx) = bound_scope();
            let (b, b_rx) = bound_scope();
            let a_handle = {
                let _entry = a.enter();
                WakeHandle::new()
            };
            let b_handle = {
                let _entry = b.enter();
                WakeHandle::default()
            };
            std::thread::spawn(move || a_handle.wake()).join().unwrap();
            let a_event = a_rx.try_recv().unwrap();
            assert!(a.accepts(&a_event));
            assert!(!b.accepts(&a_event));
            assert!(matches!(b_rx.try_recv(), Err(TryRecvError::Empty)));
            a.close();
            b_handle.wake();
            assert!(b.accepts(&b_rx.try_recv().unwrap()));
        }

        {
            let detached = WakeHandle::new();
            let (scope, rx) = bound_scope();
            let _entry = scope.enter();
            let explicit = WakeHandle::detached();
            assert!(detached.is_active());
            assert!(detached.authorize_publication());
            assert!(explicit.authorize_publication());
            detached.wake();
            explicit.wake();
            assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
            WakeHandle::new().wake();
            assert!(scope.accepts(&rx.try_recv().unwrap()));
        }

        {
            let scope = WakeScope::new();
            let entry = scope.enter();
            drop(scope);
            let captured = WakeHandle::new();
            assert!(!captured.is_active());
            assert!(!captured.authorize_publication());
            drop(entry);
            assert!(WakeHandle::new().is_active());
        }

        {
            let resource = Arc::new(());
            let weak_resource = Arc::downgrade(&resource);
            let mut scope = WakeScope::new();
            let (tx, rx) = mpsc::channel();
            scope
                .bind(move |event| {
                    std::hint::black_box(&resource);
                    tx.send(event).unwrap();
                })
                .unwrap();
            let handle = scope.handle();
            handle.wake();
            let event = rx.try_recv().unwrap();
            drop(scope);
            assert!(weak_resource.upgrade().is_none());
            assert!(!handle.is_active());
            assert!(!handle.authorize_publication());
            let replacement = WakeScope::new();
            assert!(!replacement.accepts(&event));
        }
    }

    #[test]
    fn same_open_scope_matches_only_the_original_open_scoped_route() {
        {
            let (scope, rx) = bound_scope();
            let (other_scope, other_rx) = bound_scope();
            let captured = {
                let _entry = scope.enter();
                WakeHandle::new()
            };
            let clone = captured.clone();
            let same_scope = {
                let _entry = scope.enter();
                WakeHandle::new()
            };
            let other = {
                let _entry = other_scope.enter();
                WakeHandle::new()
            };
            assert!(captured.same_open_scope(&clone));
            assert!(clone.same_open_scope(&captured));
            assert!(captured.same_open_scope(&same_scope));
            assert!(!captured.same_open_scope(&other));
            assert!(!other.same_open_scope(&captured));
            assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
            assert!(matches!(other_rx.try_recv(), Err(TryRecvError::Empty)));

            let outer = {
                let _entry = scope.enter();
                let outer = WakeHandle::new();
                let inner = {
                    let _nested = other_scope.enter();
                    WakeHandle::new()
                };
                let restored_outer = WakeHandle::new();
                assert!(outer.same_open_scope(&restored_outer));
                assert!(!inner.same_open_scope(&outer));
                outer
            };
            assert!(outer.same_open_scope(&captured));
            assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
            assert!(matches!(other_rx.try_recv(), Err(TryRecvError::Empty)));
        }

        // Detached handles never correlate, even when captured under detached entry.
        {
            let explicit = WakeHandle::detached();
            let (scope, rx) = bound_scope();
            let detached = {
                let _entry = scope.enter();
                let _mask = WakeScope::enter_detached();
                WakeHandle::new()
            };
            let scoped = {
                let _entry = scope.enter();
                WakeHandle::new()
            };
            for handle in [&explicit, &detached] {
                assert!(handle.is_active());
                assert!(handle.authorize_publication());
                assert!(!handle.same_open_scope(handle));
                assert!(!handle.same_open_scope(&handle.clone()));
                assert!(!handle.same_open_scope(&scoped));
            }
            assert!(!explicit.same_open_scope(&detached));
            assert!(!scoped.same_open_scope(&explicit));
            assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
        }

        {
            let (mut scope, _) = bound_scope();
            let old = scope.handle();
            let old_clone = old.clone();
            assert!(old.same_open_scope(&old_clone));
            scope.close();
            assert!(!old.same_open_scope(&old_clone));
            scope.close();
            assert!(!old.same_open_scope(&old_clone));
            let (replacement, _) = bound_scope();
            let fresh = replacement.handle();
            assert!(!old.same_open_scope(&fresh));
            assert!(!fresh.same_open_scope(&old));
        }

        {
            let resource = Arc::new(());
            let weak_resource = Arc::downgrade(&resource);
            let (tx, _rx) = mpsc::channel();
            let mut scope = WakeScope::new();
            scope
                .bind(move |event| {
                    std::hint::black_box(&resource);
                    tx.send(event).unwrap();
                })
                .unwrap();
            let handle = scope.handle();
            let clone = handle.clone();
            assert!(handle.same_open_scope(&clone));
            drop(scope);
            assert!(weak_resource.upgrade().is_none());
            assert!(!handle.same_open_scope(&clone));
            let (replacement, _) = bound_scope();
            assert!(!handle.same_open_scope(&replacement.handle()));
        }
    }

    #[test]
    fn closing_rejects_stale_events_without_blocking_or_holding_the_lock() {
        {
            let (mut old, rx) = bound_scope();
            let old_handle = old.handle();
            old_handle.wake();
            let queued = rx.try_recv().unwrap();
            assert!(old.accepts(&queued));
            old.close();
            let replacement = WakeScope::new();
            assert!(!old.accepts(&queued));
            assert!(!replacement.accepts(&queued));
            assert!(!old_handle.is_active());
            assert!(!old_handle.authorize_publication());
            old_handle.wake();
            assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
        }

        {
            let mut scope = WakeScope::new();
            let handle = scope.handle();
            assert!(handle.authorize_publication());
            scope.close();
            assert!(!handle.authorize_publication());
            assert!(!handle.is_active());
            scope.close(); // idempotent
        }

        {
            let mut scope = WakeScope::new();
            let reentrant_handle = scope.handle();
            let first = Arc::new(AtomicBool::new(true));
            let (tx, rx) = mpsc::channel();
            scope
                .bind(move |event| {
                    if first.swap(false, Ordering::AcqRel) {
                        reentrant_handle.wake();
                    }
                    tx.send(event).unwrap();
                })
                .unwrap();
            scope.handle().wake();
            assert!(scope.accepts(&rx.try_recv().unwrap()));
            assert!(scope.accepts(&rx.try_recv().unwrap()));
            assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
        }
    }

    #[test]
    fn entry_masks_and_restores_the_outer_context_even_on_unwind() {
        {
            let (a, a_rx) = bound_scope();
            let (b, b_rx) = bound_scope();
            {
                let _outer = a.enter();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let _inner = b.enter();
                    WakeHandle::new().wake();
                    panic!("exercise scope restoration");
                }));
                assert!(result.is_err());
                WakeHandle::new().wake();
            }
            assert!(a.accepts(&a_rx.try_recv().unwrap()));
            assert!(b.accepts(&b_rx.try_recv().unwrap()));
            WakeHandle::new().wake();
            assert!(matches!(a_rx.try_recv(), Err(TryRecvError::Empty)));
            assert!(matches!(b_rx.try_recv(), Err(TryRecvError::Empty)));
        }

        {
            let (outer, rx) = bound_scope();
            let _outer = outer.enter();
            let detached = {
                let _detached = WakeScope::enter_detached();
                let handle = WakeHandle::new();
                handle.wake();
                handle
            };
            assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
            assert!(detached.authorize_publication());
            detached.wake();
            assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
            WakeHandle::new().wake();
            assert!(outer.accepts(&rx.try_recv().unwrap()));
        }

        {
            let (outer, rx) = bound_scope();
            let _outer = outer.enter();
            let result = std::panic::catch_unwind(|| {
                let _detached = WakeScope::enter_detached();
                WakeHandle::new().wake();
                panic!("detached callback failed");
            });
            assert!(result.is_err());
            assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
            WakeHandle::new().wake();
            assert!(outer.accepts(&rx.try_recv().unwrap()));
        }
    }

    #[test]
    fn settling_waits_for_every_owed_reply_to_be_taken_up() {
        let (scope, _wakes) = bound_scope();
        let handle = scope.handle();
        let queue = Arc::new(Mutex::new(Vec::new()));
        let mut taken = Vec::new();

        let owed = handle.owe();
        assert!(handle.owes());
        let worker = {
            let queue = Arc::clone(&queue);
            std::thread::spawn(move || {
                queue.lock_recover().push(1);
                drop(owed);
            })
        };
        handle.settle(|| taken.append(&mut queue.lock_recover()));
        worker.join().unwrap();
        assert_eq!(taken, [1]);
        assert!(!handle.owes());

        let mut owed = Some(handle.owe());
        let mut passes = 0;
        handle.settle(|| {
            passes += 1;
            taken.append(&mut queue.lock_recover());
            if let Some(owed) = owed.take() {
                let queue = Arc::clone(&queue);
                std::thread::spawn(move || {
                    queue.lock_recover().push(2);
                    drop(owed);
                })
                .join()
                .unwrap();
            }
        });
        assert_eq!((taken.as_slice(), passes), ([1, 2].as_slice(), 2));

        let mut passes = 0;
        handle.settle(|| {
            passes += 1;
            handle.wake();
        });
        assert_eq!(passes, 1);
    }

    #[test]
    fn an_owner_that_cannot_wait_asks_after_each_pass() {
        let (scope, _wakes) = bound_scope();
        let handle = scope.handle();
        assert!(handle.settled());
        let owed = handle.owe();
        handle.begin_pass();
        assert!(!handle.settled(), "a reply is owed");
        drop(owed);
        assert!(
            !handle.settled(),
            "the reply landed after the pass began: the next pass takes it up"
        );
        handle.begin_pass();
        assert!(handle.settled());
    }

    #[test]
    fn a_closed_or_detached_scope_owes_nothing() {
        let mut passes = 0;
        let detached = WakeHandle::detached();
        let owed = detached.owe();
        assert!(!detached.owes());
        detached.settle(|| passes += 1);
        assert_eq!(passes, 1);
        drop(owed);

        let (mut scope, _wakes) = bound_scope();
        let handle = scope.handle();
        let owed = handle.owe();
        let mut passes = 0;
        handle.settle(|| {
            passes += 1;
            scope.close();
        });
        assert_eq!(passes, 1);
        assert!(!handle.owes());
        assert!(scope.handle().owe().route.is_none());
        drop(owed);
    }

    #[test]
    fn a_reply_wakes_the_scope_that_asked_once_answered() {
        let (scope, wakes) = bound_scope();
        let (answer, answers) = {
            let _entry = scope.enter();
            reply()
        };
        assert!(matches!(wakes.try_recv(), Err(TryRecvError::Empty)));
        std::thread::spawn(move || answer.send("picked"))
            .join()
            .unwrap();
        assert_eq!(answers.try_recv(), Ok("picked"));
        let wake = wakes.try_recv().unwrap();
        assert!(scope.accepts(&wake));
        assert!(wake.requires_sync());

        let (answer, answers) = {
            let _entry = scope.enter();
            reply::<()>()
        };
        drop(answers);
        answer.send(());
        assert!(matches!(wakes.try_recv(), Err(TryRecvError::Empty)));
    }
}
