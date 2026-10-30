//! Instance-scoped background wake routing shared by native and headless hosts.
//!
//! `tilo_rt::wake`, `tilo_host::wake` and `ui::wake` re-export this crate. It
//! links no async runtime, so a document model can route wakes in a sandboxed
//! wasm build.
use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
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

/// What [`WakeHandle::settle`] follows.
#[derive(Default)]
struct Settling {
    /// Replies the owner is owed ([`Owed`]) that have not landed.
    owed: usize,
    /// A reply landed since the last pass.
    replied: bool,
    /// The scope was woken since the last pass.
    woken: bool,
}

struct Route {
    open: AtomicBool,
    destination: Mutex<Destination>,
    /// Signalled whenever the scope is woken, a reply lands or it closes:
    /// what [`WakeHandle::settle`] waits on.
    changed: Condvar,
}

thread_local! {
    static CURRENT: RefCell<Option<Weak<Route>>> = const { RefCell::new(None) };
}

/// The non-clone owner of one mount incarnation. Drop closes it.
pub struct WakeScope {
    route: Arc<Route>,
}

/// Why a platform destination could not be installed.
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
            }),
        }
    }

    pub fn handle(&self) -> WakeHandle {
        WakeHandle {
            route: Some(Arc::downgrade(&self.route)),
        }
    }

    /// Enter during construction or app callbacks; drop guards in reverse order.
    pub fn enter(&self) -> WakeScopeGuard {
        let previous = CURRENT.with(|current| current.replace(Some(Arc::downgrade(&self.route))));
        WakeScopeGuard {
            previous,
            thread_affinity: PhantomData,
        }
    }

    /// Enter intentional headless/no-host execution, masking an outer scope.
    /// Drop guards in reverse entry order, on this thread. Captures made here
    /// stay detached after the previous construction context is restored.
    pub fn enter_detached() -> WakeScopeGuard {
        let previous = CURRENT.with(|current| current.replace(None));
        WakeScopeGuard {
            previous,
            thread_affinity: PhantomData,
        }
    }

    /// Bind once and flush one wake if work arrived before startup.
    /// The sink must capture a sender/proxy, never an owning host reference.
    ///
    /// # Errors
    ///
    /// Returns [`WakeBindError::Closed`] when the scope has already closed, or
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

    /// Drain wakes recorded while no destination is bound, as
    /// `(pending, requires_sync)`. A hosted application worker has no event
    /// loop: its model pump is the consumer. `None` means the wakes are not
    /// observable here (a sink is bound and received them, or the scope is
    /// closed), so a caller must assume a model-dirty wake.
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

    /// Drain the owner's coalesced wake flags, including wakes delivered to
    /// its event sink. An embedded application uses these flags to distinguish
    /// presentation frames from model changes. Sink delivery and receiver-side
    /// authorization still run normally. A closed scope returns `None`.
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

    /// Whether a wake is waiting for its owner, without taking it: work
    /// the owner's next pass has to run.
    pub fn is_pending(&self) -> bool {
        self.route.open.load(Ordering::Acquire) && self.route.destination.lock_recover().pending
    }

    /// Whether the wake waiting for its owner asks for a model pass, read
    /// without taking it. An owner that drains wakes mid-pass asks this so
    /// the wake stays pending: the host prepares the pass that runs it.
    pub fn sync_pending(&self) -> Option<bool> {
        if !self.route.open.load(Ordering::Acquire) {
            return None;
        }
        Some(self.route.destination.lock_recover().pending_sync)
    }

    /// Receiver-side validation remains mandatory after sender-side checks.
    pub fn accepts(&self, event: &WakeEvent) -> bool {
        self.route.open.load(Ordering::Acquire)
            && Weak::ptr_eq(&Arc::downgrade(&self.route), &event.route)
    }

    /// Stop new authorizations without waiting for previously accepted work.
    pub fn close(&mut self) {
        self.route.open.swap(false, Ordering::AcqRel);
        // Nothing owed to a closed scope is waited for any more.
        drop(self.route.destination.lock_recover());
        self.route.changed.notify_all();
    }
}

impl Drop for WakeScope {
    fn drop(&mut self) {
        self.close();
    }
}

/// A thread-affine construction/dispatch guard, independent of an App borrow.
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

    /// The wake says only that a record's save moved on
    /// ([`WakeHandle::wake_save_progress`]): an owner that takes wakes
    /// apart may show the new save status without running its model.
    #[doc(hidden)]
    pub const fn save_progress(&self) -> bool {
        self.save_progress
    }

    /// Whether the wake asks its host for a frame as well as for its
    /// owner's service. One that says only that a save moved on does not:
    /// a save that lands in time shows nothing, and the owner's service
    /// says when it does show something new (the window's bar), so a
    /// keystroke's write landing draws no second frame after the key's.
    pub const fn asks_for_frame(&self) -> bool {
        !self.save_progress
    }
}

/// Captures one scope, or remains detached forever. Unlike a closed scope,
/// detached execution may publish local/headless state without waking a host.
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
        Self {
            route: CURRENT.with(|current| current.borrow().clone()),
        }
    }

    pub const fn detached() -> Self {
        Self { route: None }
    }

    pub fn is_active(&self) -> bool {
        match &self.route {
            None => true,
            Some(route) => route
                .upgrade()
                .is_some_and(|route| route.open.load(Ordering::Acquire)),
        }
    }

    /// Compare two original scoped routes while that route is open.
    /// Detached handles never match. This is transient scheduling metadata,
    /// not registration, document, callback, or publication authorization.
    /// A successful comparison does not prevent a concurrent close.
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

    /// Authorize one publication. A prior freshness check is not authorization.
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

    /// Wake the owner because a record's save moved on (a write landed or
    /// failed), and for nothing else. An owner that cannot tell wakes apart
    /// takes it as [`Self::wake`]; one that can shows the save status
    /// alone, without running its model again for a write that changed
    /// nothing it holds.
    pub fn wake_save_progress(&self) {
        self.send_wake(true, true);
    }

    /// Wake the owner to drain an event queue. The queue consumer determines
    /// whether the model changed; receiving a notification alone does not.
    #[doc(hidden)]
    pub fn notify_events(&self) {
        self.send_wake(false, false);
    }

    /// Owe the owner a reply: work it asked for off its thread whose answer
    /// it waits to take up, such as a worker's reply to a request. The work
    /// holds the [`Owed`] and drops it once its answer is queued for the
    /// owner (or it ends without one), which wakes the owner.
    ///
    /// What an action causes this way is part of the action: a host answers
    /// the action once every reply owed meanwhile landed and was taken up
    /// ([`Self::settle`]), so its effects are the action's, not the next
    /// one's. Owe only short work answered by itself: long-running work an
    /// action starts answers as a job instead, and nothing waits on a person
    /// (a chooser) or on a stream that does not end. A detached handle, or
    /// one whose scope closed, owes nothing.
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

    /// Whether a reply is owed to the scope that has not landed.
    pub fn owes(&self) -> bool {
        self.route
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some_and(|route| {
                route.open.load(Ordering::Acquire)
                    && route.destination.lock_recover().settling.owed > 0
            })
    }

    /// Forget the replies that landed so far: the owner's next pass takes
    /// them up, and [`Self::settled`] after it asks only about later ones.
    /// For an owner that cannot wait ([`Self::settle`]), such as a window
    /// whose frames also serve a person: it marks where a pass begins and
    /// asks once it ended.
    pub fn begin_pass(&self) {
        if let Some(route) = self.route.as_ref().and_then(Weak::upgrade) {
            let mut destination = route.destination.lock_recover();
            destination.settling.replied = false;
            destination.settling.woken = false;
        }
    }

    /// Whether nothing is owed and no reply landed since
    /// [`Self::begin_pass`]: a pass that began after it took up everything
    /// that was owed. A closed or detached scope is settled; one that is not
    /// is woken when a reply lands, so its owner asks again then.
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

    /// Run `pass`, one turn of the owner's loop that takes up whatever has
    /// landed for it, until no reply is owed ([`Self::owe`]) and a pass ran
    /// after the last one landed. Between passes it waits for the scope to
    /// be woken: it never polls, and returns at once when the scope closes.
    /// At least one pass runs: the one that takes up what the caller just
    /// did. A detached handle runs that one.
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
                // A reply that landed after the pass began is taken up by
                // the next one, even when it was the last owed.
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

/// A reply owed to a scope's owner ([`WakeHandle::owe`]). Dropping it says
/// the reply landed: queued for the owner, or the work ended without one.
/// It wakes the owner.
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

/// The answering end of a request made under a wake scope, such as a system
/// chooser's. Sending the answer wakes that scope's event loop, so an answer
/// that arrives while the app is idle is drawn without waiting for input.
pub struct Reply<T> {
    sender: Sender<T>,
    wake: WakeHandle,
}

/// A reply channel for a request made now, under the current wake scope.
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
        // The requester may have moved on (window closed); a dead channel
        // needs no wake.
        if self.sender.send(answer).is_ok() {
            self.wake.wake();
        }
    }
}

/// The worker's end of a [`waking_channel`]: every message it sends wakes
/// the scope that opened the channel, so the app drains it on its next sync
/// instead of pumping frames to poll for it.
pub struct WakingSender<T> {
    sender: Sender<T>,
    wake: WakeHandle,
}

impl<T> Clone for WakingSender<T> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            wake: self.wake.clone(),
        }
    }
}

impl<T> WakingSender<T> {
    /// Queue `message` and wake the owner to take it. A gone receiver (the
    /// window closed) is an error.
    pub fn send(&self, message: T) -> Result<(), std::sync::mpsc::SendError<T>> {
        self.sender.send(message)?;
        self.wake.wake();
        Ok(())
    }
}

impl<T> Drop for WakingSender<T> {
    /// A worker that ends without replying (it failed, or panicked) wakes
    /// the owner too, so the owner sees the channel disconnected instead of
    /// waiting forever.
    fn drop(&mut self) {
        self.wake.wake();
    }
}

/// "Wake me when this worker replies": a channel opened now, under the
/// current wake scope (the app's). A worker sends its replies through the
/// [`WakingSender`]; each one wakes the app's event loop, which drains the
/// [`Receiver`] in its next sync. A waiting app paints nothing until a reply
/// lands — `needs_animation_frame` is for motion, never for polling a worker.
#[must_use]
pub fn waking_channel<T>() -> (WakingSender<T>, Receiver<T>) {
    let (sender, receiver) = channel();
    (
        WakingSender {
            sender,
            wake: WakeHandle::new(),
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

    #[test]
    fn a_worker_reply_on_a_waking_channel_wakes_its_owner() {
        let (scope, rx) = bound_scope();
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
        // The worker's end went away with its thread: one more wake.
        assert!(rx.recv().is_ok());
        let (sender, replies) = {
            let _entry = scope.enter();
            waking_channel::<u32>()
        };
        drop(replies);
        assert!(sender.send(1).is_err(), "a gone receiver wakes nobody");
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
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
        // Clones and independent captures in the same actual scope match;
        // another scope does not, and comparisons do not deliver wakes.
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

        // Detached handles retain their existing publication behavior but
        // never correlate, including when captured under detached entry.
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

        // Closing invalidates correlation immediately and idempotently. A
        // replacement scope is a distinct route even while both are alive.
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

        // A comparison does not retain the route or its sink-captured resource.
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

        // A reply landing while the owner waits wakes it for a pass.
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

        // A reply landing during a pass, after it took up what was queued,
        // is taken up by one more, though nothing is owed any more.
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

        // Nothing owed: one pass, however often it wakes its own scope.
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

        // Closing the scope ends a wait on what it was owed.
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

        // Nobody is listening any more: no wake for an answer nobody reads.
        let (answer, answers) = {
            let _entry = scope.enter();
            reply::<()>()
        };
        drop(answers);
        answer.send(());
        assert!(matches!(wakes.try_recv(), Err(TryRecvError::Empty)));
    }
}
