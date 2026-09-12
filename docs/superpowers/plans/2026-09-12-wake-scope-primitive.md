# Wake Scope Primitive Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Supply and verify the portable lifetime/routing primitive needed to host independent app instances.

**Architecture:** A non-clone scope owns an open route; handles and queued events hold weak references to that exact incarnation. Construction captures a scoped thread-local context once, while publication and event acceptance use the captured route. Sink callbacks run outside locks.

**Tech Stack:** Rust standard library, shared `ui`, native macOS and Fedora nextest.

**Spec:** `docs/superpowers/specs/2026-09-12-macos-shell-port-design.md`, sections 4 and 11.

## Global Constraints

- This is the first independently testable part of the app-host lifetime work. It does not replace the current global `local_state::WakeHandle` yet and is not a complete host port. The subsequent integration must replace that implementation, capture routes at `LocalState::new`/`LatestRequest::new`, add App factory construction and receiver filtering, and migrate every production live constructor in the architectural wake-routing report.
- No platform event-loop, shell, GPU, new dependency, process-global destination, or live UI is needed for this primitive.
- Handles never retarget. Detached handles remain functional without a destination; closed scoped handles are inactive. Do not turn a dead weak route into a detached handle.
- `Default` for a wake handle must behave like `new`: capture the current construction scope. An explicit `detached` constructor is available for intentional detached execution.
- Close does not wait for publishers or callbacks. Authorization is a successful atomic compare-exchange on the open bit; work authorized before close may finish against old captured storage. Work authorized after close is rejected. A later integration enforces this at each actual publication.
- No route lock spans a sink or application callback. A queued event must be checked at its recipient against its current scope, even if the sender checked before enqueue.
- Scope guards are thread-affine and must be dropped in reverse entry order. Nested entry and unwind restore the previous context. Scope ownership is separate from the guard, so factory construction can return an App without retaining a borrow.
- Work in `/Users/user/dev/qos-macos-shell`, branch `codex/macos-shell`; native Cargo target `/Users/user/dev/qos/target/macos-shell`. Controller performs the same tests on Fedora at the committed SHA.

## Task 1: Implement weak instance wake routes and their lifecycle tests

**Files:**
- Create: `ui/src/wake.rs` (route, scope, weak handle, event and thread-affine guard).
- Modify: `ui/src/lib.rs` (only add `pub mod wake;` in module order).
- Tests: unit tests and compile-fail guard documentation in `ui/src/wake.rs`.

**Interfaces:**
- Produces `ui::wake::{WakeScope, WakeScopeGuard, WakeHandle, WakeEvent, WakeBindError}`.
- Existing `ui::WakeHandle` and `ui::local_state::WakeHandle` remain unchanged during this prerequisite; the later integration will re-export the new type from those existing paths.
- `WakeScope::new`, `handle`, `enter`, `bind`, `accepts`, and `close` have the exact signatures below.
- `WakeHandle::authorize_publication` is crate-private for the later LocalState/completion integration. It grants one operation, not a reusable right and not a promise that an event will be delivered.

- [ ] Add the module declaration and the following tests first. Run the focused command and record the expected missing-type failure before adding the implementation. Retain the tests unchanged unless the controller rules on a concrete defect.

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{self, Receiver, TryRecvError};

    fn bound_scope() -> (WakeScope, Receiver<WakeEvent>) {
        let (tx, rx) = mpsc::channel();
        let mut scope = WakeScope::new();
        scope.bind(move |event| { tx.send(event).unwrap(); }).unwrap();
        (scope, rx)
    }

    #[test]
    fn captured_handles_keep_their_owner_across_threads_and_other_hosts() {
        let (mut a, a_rx) = bound_scope();
        let (b, b_rx) = bound_scope();
        let a_handle = { let _entry = a.enter(); WakeHandle::new() };
        let b_handle = { let _entry = b.enter(); WakeHandle::default() };
        std::thread::spawn(move || a_handle.wake()).join().unwrap();
        let a_event = a_rx.try_recv().unwrap();
        assert!(a.accepts(&a_event));
        assert!(!b.accepts(&a_event));
        assert!(matches!(b_rx.try_recv(), Err(TryRecvError::Empty)));
        a.close();
        b_handle.wake();
        assert!(b.accepts(&b_rx.try_recv().unwrap()));
    }

    #[test]
    fn pending_wakes_coalesce_before_the_sink_is_bound() {
        let mut scope = WakeScope::new();
        let worker = scope.handle();
        std::thread::spawn(move || {
            for _ in 0..8 { worker.wake(); }
        }).join().unwrap();
        let (tx, rx) = mpsc::channel();
        scope.bind(move |event| { tx.send(event).unwrap(); }).unwrap();
        assert!(scope.accepts(&rx.try_recv().unwrap()));
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
        scope.handle().wake();
        assert!(scope.accepts(&rx.try_recv().unwrap()));
    }

    #[test]
    fn binding_twice_cannot_replace_the_destination() {
        let (mut scope, rx) = bound_scope();
        assert_eq!(scope.bind(|_| panic!("replacement sink called")),
                   Err(WakeBindError::AlreadyBound));
        scope.handle().wake();
        assert!(scope.accepts(&rx.try_recv().unwrap()));
        scope.close();
        assert_eq!(scope.bind(|_| {}), Err(WakeBindError::Closed));
    }

    #[test]
    fn a_closed_mount_and_its_replacement_reject_the_old_queued_event() {
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

    #[test]
    fn close_does_not_wait_for_an_already_authorized_operation() {
        let mut scope = WakeScope::new();
        let handle = scope.handle();
        assert!(handle.authorize_publication());
        scope.close();
        assert!(!handle.authorize_publication());
        assert!(!handle.is_active());
        scope.close(); // idempotent
    }

    #[test]
    fn detached_handles_never_attach_to_a_later_host() {
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

    #[test]
    fn nested_entry_and_unwind_restore_the_previous_context() {
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

    #[test]
    fn workers_and_queued_events_do_not_keep_a_scope_or_sink_alive() {
        let resource = Arc::new(());
        let weak_resource = Arc::downgrade(&resource);
        let mut scope = WakeScope::new();
        let (tx, rx) = mpsc::channel();
        scope.bind(move |event| {
            let _resource = &resource;
            tx.send(event).unwrap();
        }).unwrap();
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

    #[test]
    fn construction_under_a_dropped_owner_stays_closed_not_detached() {
        let scope = WakeScope::new();
        let entry = scope.enter();
        drop(scope);
        let captured = WakeHandle::new();
        assert!(!captured.is_active());
        assert!(!captured.authorize_publication());
        drop(entry);
        assert!(WakeHandle::new().is_active());
    }

    #[test]
    fn callbacks_run_outside_the_route_lock() {
        let mut scope = WakeScope::new();
        let reentrant_handle = scope.handle();
        let first = Arc::new(AtomicBool::new(true));
        let (tx, rx) = mpsc::channel();
        scope.bind(move |event| {
            if first.swap(false, Ordering::AcqRel) { reentrant_handle.wake(); }
            tx.send(event).unwrap();
        }).unwrap();
        scope.handle().wake();
        assert!(scope.accepts(&rx.try_recv().unwrap()));
        assert!(scope.accepts(&rx.try_recv().unwrap()));
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
    }
}
```

- [ ] Implement the primitive below in `ui/src/wake.rs`. Keep documentation that distinguishes detached from closed and acceptance from delivered wake. Formatting and narrow compiler/lint corrections are allowed; any semantic change needs a controller ruling.

```rust
//! Instance-scoped background wake routing shared by native and headless hosts.
use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

type Sink = Arc<dyn Fn(WakeEvent) + Send + Sync>;

#[derive(Default)]
struct Destination {
    sink: Option<Sink>,
    pending: bool,
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
    fn default() -> Self { Self::new() }
}

impl WakeScope {
    pub fn new() -> Self {
        Self { route: Arc::new(Route {
            open: AtomicBool::new(true),
            destination: Mutex::new(Destination::default()),
        }) }
    }

    pub fn handle(&self) -> WakeHandle {
        WakeHandle { route: Some(Arc::downgrade(&self.route)) }
    }

    /// Enter during construction or app callbacks; drop guards in reverse order.
    pub fn enter(&self) -> WakeScopeGuard {
        let previous = CURRENT.with(|current| {
            current.replace(Some(Arc::downgrade(&self.route)))
        });
        WakeScopeGuard { previous, thread_affinity: PhantomData }
    }

    /// Bind once and flush one wake if work arrived before startup.
    /// The sink must capture a sender/proxy, never an owning host reference.
    #[allow(clippy::needless_pass_by_ref_mut,
        reason = "binding requires exclusive lifecycle-owner access despite interior route synchronization")]
    pub fn bind(&mut self, sink: impl Fn(WakeEvent) + Send + Sync + 'static)
        -> Result<(), WakeBindError>
    {
        let pending = {
            let mut destination = self.route.destination.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !self.route.open.load(Ordering::Acquire) {
                return Err(WakeBindError::Closed);
            }
            if destination.sink.is_some() {
                return Err(WakeBindError::AlreadyBound);
            }
            destination.sink = Some(Arc::new(sink));
            std::mem::take(&mut destination.pending)
        };
        if pending { self.handle().wake(); }
        Ok(())
    }

    /// Receiver-side validation remains mandatory after sender-side checks.
    pub fn accepts(&self, event: &WakeEvent) -> bool {
        self.route.open.load(Ordering::Acquire)
            && Weak::ptr_eq(&Arc::downgrade(&self.route), &event.route)
    }

    /// Stop new authorizations without waiting for previously accepted work.
    #[allow(clippy::needless_pass_by_ref_mut,
        reason = "closing requires exclusive lifecycle-owner access and never holds a publisher lock")]
    pub fn close(&mut self) {
        self.route.open.swap(false, Ordering::AcqRel);
    }
}

impl Drop for WakeScope {
    fn drop(&mut self) { self.close(); }
}

/// A thread-affine construction/dispatch guard, independent of an App borrow.
///
/// ```compile_fail
/// let scope = ui::wake::WakeScope::new();
/// let guard = scope.enter();
/// std::thread::spawn(move || drop(guard));
/// ```
pub struct WakeScopeGuard {
    previous: Option<Weak<Route>>,
    thread_affinity: PhantomData<Rc<()>>,
}

impl Drop for WakeScopeGuard {
    fn drop(&mut self) {
        CURRENT.with(|current| { current.replace(self.previous.take()); });
    }
}

/// A weak identity for a queued wake; it never owns an app or its scope.
#[derive(Clone, Debug)]
pub struct WakeEvent { route: Weak<Route> }

/// Captures one scope, or remains detached forever. Unlike a closed scope,
/// detached execution may publish local/headless state without waking a host.
#[derive(Clone, Debug)]
pub struct WakeHandle { route: Option<Weak<Route>> }

impl Default for WakeHandle {
    fn default() -> Self { Self::new() }
}

impl WakeHandle {
    pub fn new() -> Self {
        Self { route: CURRENT.with(|current| current.borrow().clone()) }
    }

    pub const fn detached() -> Self { Self { route: None } }

    pub fn is_active(&self) -> bool {
        match &self.route {
            None => true,
            Some(route) => route.upgrade()
                .is_some_and(|route| route.open.load(Ordering::Acquire)),
        }
    }

    /// Authorize one publication. A prior freshness check is not authorization.
    pub(crate) fn authorize_publication(&self) -> bool {
        match &self.route {
            None => true,
            Some(route) => route.upgrade().is_some_and(|route| {
                route.open.compare_exchange(true, true, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
            }),
        }
    }

    pub fn wake(&self) {
        if !self.authorize_publication() { return; }
        let Some(weak) = &self.route else { return; };
        let Some(route) = weak.upgrade() else { return; };
        let sink = {
            let mut destination = route.destination.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !route.open.load(Ordering::Acquire) { return; }
            match &destination.sink {
                Some(sink) => Some(Arc::clone(sink)),
                None => { destination.pending = true; None }
            }
        };
        if let Some(sink) = sink { sink(WakeEvent { route: weak.clone() }); }
    }
}
```

Wake authorizes before dispatch and rechecks the open bit under the destination lock. This is not authorization for a future state mutation; that mutation must authorize separately.
- [ ] Run `CARGO_TARGET_DIR=/Users/user/dev/qos/target/macos-shell cargo nextest run -p ui --lib --locked --profile ci -E 'test(wake::tests::)'`. Require all ten tests, with no GPU adapter requirement.
- [ ] Run `CARGO_TARGET_DIR=/Users/user/dev/qos/target/macos-shell cargo test -p ui --doc --locked 'wake::WakeScopeGuard'` and require the compile-fail guard test to pass.
- [ ] Format only `ui/src/wake.rs`; run `cargo clippy -p ui --lib --locked -- -D warnings` with the same target. Keep `lib.rs` to its module declaration change.
- [ ] Commit only those two files as `feat(ui): add instance-scoped wake lifetime primitives`. Report native RED/GREEN, doc test and clippy evidence. Controller syncs the committed SHA to Fedora and runs the same scoped tests/doc test and clippy.

## Controller integration boundary

The next implementation plan consumes this API to replace production global
routing in LocalState, request/task completion and App. Its acceptance tests
must cover callback publication/close races using real local state, current
generation preservation, constructor ownership, all production live startup
sites and event-loop recipient filtering. The primitive tests do not prove
that integration, and no app-host or port completion may be claimed here.
