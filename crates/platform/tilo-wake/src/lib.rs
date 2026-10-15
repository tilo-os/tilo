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
use std::sync::{Arc, Mutex, Weak};
use tilo_lock::MutexRecover as _;

type Sink = Arc<dyn Fn(WakeEvent) + Send + Sync>;

#[derive(Default)]
struct Destination {
    sink: Option<Sink>,
    pending: bool,
    pending_sync: bool,
}

struct Route {
    open: AtomicBool,
    destination: Mutex<Destination>,
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
            self.handle().send_wake(requires_sync);
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

    /// Receiver-side validation remains mandatory after sender-side checks.
    pub fn accepts(&self, event: &WakeEvent) -> bool {
        self.route.open.load(Ordering::Acquire)
            && Weak::ptr_eq(&Arc::downgrade(&self.route), &event.route)
    }

    /// Stop new authorizations without waiting for previously accepted work.
    pub fn close(&mut self) {
        self.route.open.swap(false, Ordering::AcqRel);
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
}

impl WakeEvent {
    #[doc(hidden)]
    pub const fn requires_sync(&self) -> bool {
        self.requires_sync
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
        self.send_wake(true);
    }

    /// Wake the owner to drain an event queue. The queue consumer determines
    /// whether the model changed; receiving a notification alone does not.
    #[doc(hidden)]
    pub fn notify_events(&self) {
        self.send_wake(false);
    }

    fn send_wake(&self, requires_sync: bool) {
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
            destination.sink.as_ref().map(Arc::clone)
        };
        if let Some(sink) = sink {
            sink(WakeEvent {
                route: weak.clone(),
                requires_sync,
            });
        }
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
        // Case `queue_notifications_preserve_model_dirty_wakes_before_binding`.
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

        // Case `pending_wakes_coalesce_before_the_sink_is_bound`.
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

        // Case `binding_twice_cannot_replace_the_destination`.
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
    fn handles_keep_their_owner_and_never_keep_a_scope_alive() {
        // Case `captured_handles_keep_their_owner_across_threads_and_other_hosts`.
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

        // Case `detached_handles_never_attach_to_a_later_host`.
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

        // Case `construction_under_a_dropped_owner_stays_closed_not_detached`.
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

        // Case `workers_and_queued_events_do_not_keep_a_scope_or_sink_alive`.
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

            // Nested construction captures the inner route, then restores outer.
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
        // Case `a_closed_mount_and_its_replacement_reject_the_old_queued_event`.
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

        // Case `close_does_not_wait_for_an_already_authorized_operation`.
        {
            let mut scope = WakeScope::new();
            let handle = scope.handle();
            assert!(handle.authorize_publication());
            scope.close();
            assert!(!handle.authorize_publication());
            assert!(!handle.is_active());
            scope.close(); // idempotent
        }

        // Case `callbacks_run_outside_the_route_lock`.
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
        // Case `nested_entry_and_unwind_restore_the_previous_context`.
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

        // Case `detached_entry_masks_an_outer_scope_and_restores_it`.
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

        // Case `detached_entry_restores_an_outer_scope_after_unwind`.
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
