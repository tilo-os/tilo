# Production Wake Scope Integration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the production process-global UI wake destination with captured instance scopes throughout local state, completion helpers, and every live App startup/dispatch path.

**Architecture:** Consume the existing `ui::wake` primitive. State captures its owner at construction; publication independently authorizes against that lifetime, and the receiving App rejects closed/foreign events. App owns the scope before its factory runs and enters it around callbacks, while explicitly detached runners suppress ambient capture.

**Tech Stack:** Rust standard library synchronization, shared `ui`, winit, existing nextest configuration; native Apple Silicon macOS and Fedora.

**Spec:** `docs/superpowers/specs/2026-09-12-macos-shell-port-design.md` §§4, 11; prerequisite `docs/superpowers/plans/2026-09-12-wake-scope-primitive.md`; architectural findings `.superpowers/sdd/2026-09-12-macos-shell-build-foundation/app-wake-routing-design.md`.

**Status:** Candidate for controller review. Inspected `/Users/xaver/dev/qos-macos-shell` at `bd43559855a49a271bf50b73a92f1bcba883b4a8` on `codex/macos-shell`. This planning pass runs no builds/tests, changes no source, performs no VM operations, and makes no git mutations. Line numbers below name that baseline; function names are authoritative after edits.

## Global Constraints

Relevant specification requirements, copied verbatim:

- “Background work captures its instance lifetime at construction; pre-mount wakes are retained until binding, and completions never switch their destination to another instance.”
- “Reopening a persistent instance ID creates a fresh lifetime token, so an old completion cannot target the new mount.”
- “Keep standalone startup automatic; apps should not install their own platform event-loop proxy.”
- “Apps and `ui` must not depend on `shell` to request a service.”
- “Benchmarks and tests must not load the user's real state or secrets.”
- “Run both hosts from the same git commit and lockfile, with separate native build directories.”
- “Keep relevant CI green before integration.”

Integration requirements:

- No new dependency, platform implementation, renderer change, document schema, or speculative `ApplicationDefinition` API. `ui::wake` remains independent of winit and shell.
- Keep generation zero invalid for completion capabilities, reserve `u64::MAX` for terminal invalidation, and retain checked LocalState revision allocation. Closed construction must not turn the zero fallback capability into a current operation.
- Authorize each actual publication once. A successful atomic authorization permits that operation to finish against its captured old storage even if close follows; a false result must not mutate storage/revision or invoke the update closure. Authorization is independent of advisory freshness checks.
- No route mutex or generation mutex spans a wake sink; no route mutex spans a state update callback. Retain the existing per-state write lock around `try_update`'s mutation closure. Reentrant mutation of the *same* state remains the existing limitation.
- A true publication result means accepted publication, not guaranteed live wake delivery. Detached publications remain valid without waking any host. Closed weak routes never become detached.
- Guards are `!Send`; every guard must drop in reverse entry order. Never store one in App or carry it to a worker/async suspension.
- The Task 2 cutover is indivisible: do not commit/export/review an intermediate public-handle swap, constructor migration subset, or unbound live App. Keep the old complete behavior until the complete new behavior can be tested.
- Every runtime test command uses `.superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py`; the controller creates it before task dispatch from the isolated-validation section. This applies to every extracted task brief.
- Native build target: `CARGO_TARGET_DIR=/Users/xaver/dev/qos/target/macos-shell`. Controller alone runs the corresponding Fedora commands at the committed SHA. Apply repository nextest/GPU rules to broader checks.

## Controller rulings and bounded scope

Controller accepted two additions to the earlier routing design:

1. `WakeScope::enter_detached() -> WakeScopeGuard` explicitly clears the ambient construction context for a detached callback and restores it on drop. A no-op guard would incorrectly capture an outer app.
2. `App::from_scoped_parts(WakeScope, State, Box<dyn Widget>) -> App` transfers an existing owner without creating a fresh scope. This supports Word Processor and galleries that build document state and widgets together, and custom native setup that needs concrete engine handles. Callers must enter that same scope **before** constructing either part. The ordinary factory constructors remain the preferred simple API.

Controller also accepted rejecting `App::run_with_setup` on a detached App before creating a native event loop. Its existing API already panics on unrecoverable startup failure. A missed live constructor must fail clearly instead of silently losing wakes. These are candidate-design rulings, not implementation approval.

This slice establishes App-owned routing, including the hosted shell's outer App, and a reusable scope/factory/dispatch seam. It does **not** yet give each existing shell tile a production component scope. That needs the following component-contract slice: factory/catalogue identity, per-mount scope before real Notes/Files construction, every AppWidget callback entered, mount-table receiver lookup and incarnation filtering, fresh state per reopen, persistence/services, and the complete app capability map. The conformance model below tests the wake contract for that seam; it is not evidence that real embedded tiles already use it. Headless Fedora, native compositor, real Notes/Files parity, cross-host collaboration, and the full host matrix remain required for the broader port.

## File responsibility map

| File | Responsibility |
| --- | --- |
| `ui/src/wake.rs` | Existing primitive plus explicit detached entry and its tests; no routing redesign |
| `ui/src/local_state.rs` | Capture, lifetime authorization, task/request generation semantics, channel draining, portable publication regression tests |
| `ui/src/app.rs` | Owner transfer/factories, callback context, native sink bind, receiver filtering, close/drop, App-level regression tests |
| `ui/src/launch.rs` | Scoped standard construction/configuration and explicitly detached CLI utility/headless dispatch; testable construction helper |
| `ui/src/lib.rs` | Re-export one canonical WakeHandle under both established public paths; expose scope types |
| `ui/src/clipboard.rs`; `apps/settings/src/panes/updates.rs` | Three `Copy` to `Clone` consumers |
| Nine custom live construction sites listed in Task 2 | Enter before live root-producing setup and transfer scope to App |
| Eight `ui/examples` files listed in Task 2 | Same live startup contract, with activity timeline shots explicitly detached |
| `ui/src/persist.rs` and App documentation | Factory examples and detached/live semantics |
| `ui/tests/wake_scope_host.rs`; `ui/tests/it.rs` | Portable multi-mount/multi-host conformance and registration in the existing consolidated integration binary |

---

## Isolated validation environment

The real App initializer calls `Theme::configured`, `settings_store::settings_path`, collaboration discovery and floating-service discovery. Disabled document autosave alone does not isolate those readers. All runtime verification commands in this plan must run through the following controller-owned scratch helper, preserving HOME and the build toolchain while replacing application data/configuration paths.

Create `.superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py` with:

```python
#!/usr/bin/env python3
import os
from pathlib import Path
import subprocess
import sys
import tempfile

if len(sys.argv) < 2:
    raise SystemExit("usage: run-isolated.py COMMAND [ARG ...]")
with tempfile.TemporaryDirectory(prefix="qos-wake-validation-") as directory:
    environment = {key: value for key, value in os.environ.items()
                   if not key.startswith(("QOS_", "QUICK_", "XDG_"))}
    for name in ("CONFIG_HOME", "DATA_HOME", "STATE_HOME", "CACHE_HOME", "RUNTIME_DIR"):
        path = Path(directory) / name.lower()
        path.mkdir(mode=0o700)
        environment["XDG_" + name] = str(path)
    result = subprocess.run(sys.argv[1:], env=environment, check=False)
    raise SystemExit(result.returncode)
```

Prefix every nextest run, cargo test (including doctests and examples), and native-startup regression command below with `python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py`; keep each command’s target-directory environment and arguments. For example:

```bash
CARGO_TARGET_DIR=/Users/xaver/dev/qos/target/macos-shell python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo nextest run -p ui --lib --locked --profile ci -E 'test(wake::tests::)'
```

The controller supplies the identical helper to Fedora in scratch; its isolated target directory remains `/home/parallels/qos-target`. `cargo check` and clippy need no runtime discovery, but may use the same helper. Preserve Vulkan/Metal selection variables already required for broader graphics checks; this helper removes only qOS/Quick runtime inputs and replaces XDG paths. No test changes a parent process’s environment or creates a user profile.

### Task 1: Add explicit detached callback entry

**Files:** Modify/test `ui/src/wake.rs`.

**Interfaces:** Consumes existing `CURRENT`, `WakeScopeGuard`, `WakeScope::enter`. Produces `pub fn WakeScope::enter_detached() -> WakeScopeGuard`; no production routing or public handle re-export changes yet.

- [ ] Add these tests to the existing `wake::tests` module:

```rust
#[test]
fn detached_entry_masks_an_outer_scope_and_restores_it() {
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

#[test]
fn detached_entry_restores_an_outer_scope_after_unwind() {
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
```

- [ ] Run RED: `CARGO_TARGET_DIR=/Users/xaver/dev/qos/target/macos-shell python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo nextest run -p ui --lib --locked --profile ci -E 'test(wake::tests::detached_entry)'`. Expected compiler failure: no `WakeScope::enter_detached`.
- [ ] Add this method to `impl WakeScope` beside `enter`:

```rust
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
```

- [ ] Run GREEN with `CARGO_TARGET_DIR=/Users/xaver/dev/qos/target/macos-shell python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo nextest run -p ui --lib --locked --profile ci -E 'test(wake::tests::)'`, then `CARGO_TARGET_DIR=/Users/xaver/dev/qos/target/macos-shell python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo test -p ui --doc --locked 'wake::WakeScopeGuard'`. Require the 12 primitive tests and the thread-affinity compile-fail doctest.
- [ ] Format only the modified file with `rustfmt --edition 2024 ui/src/wake.rs`; run `CARGO_TARGET_DIR=/Users/xaver/dev/qos/target/macos-shell cargo clippy -p ui --lib --locked -- -D warnings`.
- [ ] Commit only `ui/src/wake.rs` with message `feat(ui): add explicit detached wake entry`. Controller mirrors this SHA and runs the same tests/doc test/clippy on Fedora before Task 2 is accepted. Existing global routing is still completely intact at this boundary.

### Task 2: Cut over production publication and every live App entry together

**Execution order:** Add the test code in 2A, 2G and 2H first and record their RED results. Then perform 2B–2F as one coordinated implementation, and run 2I. The sections below group code by responsibility; no partial cutover is a review/commit boundary.

**Files:** Modify `ui/src/local_state.rs`, `ui/src/app.rs`, `ui/src/launch.rs`, `ui/src/lib.rs`, `ui/src/clipboard.rs`, `ui/src/persist.rs`, `apps/settings/src/panes/updates.rs`; custom constructors in `apps/browser/src/main.rs`, `apps/terminal/src/main.rs`, `apps/osk/src/main.rs`, `apps/pdf_viewer/src/main.rs`, `apps/word-processor/src/main.rs`, `crates/session/installer/src/main.rs`, `crates/session/greeter/src/main.rs`, `shell/src/compositor/hosted.rs`; examples `ui/examples/{activity_timeline,channel_browser,conflict_picker,dashboard,gallery,identity_flow_demo,responsive_gallery,widget_showcase}.rs`. Add the test code below inside `local_state.rs`, `app.rs`, and `launch.rs`.

**Interfaces:**

- Consumes the complete `ui::wake::{WakeScope, WakeScopeGuard, WakeHandle, WakeEvent, WakeBindError}` primitive, including Task 1's detached entry.
- `ui::WakeHandle`, `ui::local_state::WakeHandle`, and `ui::wake::WakeHandle` become the identical `Clone`, non-`Copy` type.
- Public App additions:

```rust
pub fn from_factory(state: State, factory: impl FnOnce() -> Box<dyn Widget>) -> Self;
pub fn try_from_factory<E>(
    state: State,
    factory: impl FnOnce() -> Result<Box<dyn Widget>, E>,
) -> Result<Self, E>;
pub fn from_scoped_parts(
    wake_scope: crate::wake::WakeScope,
    state: State,
    root_widget: Box<dyn Widget>,
) -> Self;
pub fn enter_wake_scope(&self) -> crate::wake::WakeScopeGuard;
```

- Private App additions: `with_parts(Option<WakeScope>, State, Box<dyn Widget>, CollaborationManager) -> Self`; `accept_wake(&mut self, &WakeEvent) -> bool`; `close_wake_scope(&mut self)`.
- `UserEvent::Wake(crate::wake::WakeEvent)` carries a mount incarnation. It is no longer a unit variant.
- Private LaunchSpec helper `build_native_app(spec: &LaunchSpec, args: &LaunchArgs) -> Result<App, LaunchError>` constructs/configures without running an event loop; `run_native_app` calls it and then `run`.
- `LocalState::{try_set,try_update}`, writer operations, and task/request completion return acceptance, including lifetime/revision/generation rejection. Existing `set`, `update`, `begin`, and `cancel` signatures remain available. `LatestRequest::is_pending` ceases to be `const` because a closed owner's request is not pending.

#### 2A. Add failing production state tests before touching routing

- [ ] In `local_state::tests`, import `crate::wake::{WakeEvent, WakeScope}` and `std::sync::mpsc::TryRecvError`; add this helper and tests. They use real state and captured destinations, never the old `LocalStateWriter::for_test` callback.

```rust
fn bound_scope() -> (WakeScope, Receiver<WakeEvent>) {
    let (tx, rx) = mpsc::channel();
    let mut scope = WakeScope::new();
    scope.bind(move |event| { let _ = tx.send(event); }).unwrap();
    (scope, rx)
}

#[test]
fn writer_keeps_creation_owner_when_requested_under_another_scope() {
    let (mut a, a_rx) = bound_scope();
    let (b, b_rx) = bound_scope();
    let state = { let _entry = a.enter(); LocalState::new(0) };
    let writer = { let _entry = b.enter(); state.writer() };
    assert!(std::thread::spawn(move || writer.try_set(1)).join().unwrap());
    assert_eq!(state.get(), 1);
    assert_eq!(state.revision(), 1);
    assert!(a.accepts(&a_rx.try_recv().unwrap()));
    assert!(matches!(b_rx.try_recv(), Err(TryRecvError::Empty)));
    a.close();
    b.handle().wake();
    assert!(b.accepts(&b_rx.try_recv().unwrap()));
}

#[test]
fn closed_state_rejects_direct_writer_and_binding_publications() {
    let (mut scope, rx) = bound_scope();
    let state = { let _entry = scope.enter(); LocalState::new(7) };
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

#[test]
fn authorized_update_can_finish_after_nonblocking_close() {
    use std::time::Duration;
    let (mut scope, rx) = bound_scope();
    let state = { let _entry = scope.enter(); LocalState::new(1) };
    let writer = state.writer();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let worker = std::thread::spawn(move || writer.try_update(|value| {
        entered_tx.send(()).unwrap();
        resume_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        *value = 2;
    }));
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    // If close waits for the mutation callback, its 5s timeout fails the test.
    scope.close();
    let replacement_scope = WakeScope::new();
    let replacement = { let _entry = replacement_scope.enter(); LocalState::new(100) };
    resume_tx.send(()).unwrap();
    assert!(worker.join().unwrap());
    assert_eq!(state.get(), 2);
    assert_eq!(state.revision(), 1);
    assert_eq!(replacement.get(), 100);
    assert_eq!(replacement.revision(), 0);
    assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
}

#[test]
fn update_may_publish_second_state_and_close_its_scope() {
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

#[test]
fn task_begin_under_another_scope_keeps_the_state_creation_owner() {
    let (a, a_rx) = bound_scope();
    let (b, b_rx) = bound_scope();
    let task = { let _entry = a.enter(); LatestTask::new(0) };
    let completion = { let _entry = b.enter(); task.begin(1) };
    assert!(std::thread::spawn(move || completion.complete(2)).join().unwrap());
    assert!(a.accepts(&a_rx.try_recv().unwrap()));
    assert!(matches!(b_rx.try_recv(), Err(TryRecvError::Empty)));
    assert_eq!(task.state().get(), 2);
}

#[test]
fn close_rejects_task_and_request_without_explicit_cancel() {
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

#[test]
fn closed_zero_generation_fallbacks_are_permanently_stale() {
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

#[test]
fn request_begin_keeps_owner_and_closed_inbox_discards_queued_result() {
    let (mut a, a_rx) = bound_scope();
    let (b, b_rx) = bound_scope();
    let mut request = { let _entry = a.enter(); LatestRequest::new() };
    let completion = { let _entry = b.enter(); request.begin() };
    assert!(std::thread::spawn(move || completion.complete(42)).join().unwrap());
    let event = a_rx.try_recv().unwrap();
    assert!(a.accepts(&event));
    assert!(matches!(b_rx.try_recv(), Err(TryRecvError::Empty)));
    a.close();
    assert_eq!(request.try_take(), None);
    assert!(!request.is_pending());
    assert!(!a.accepts(&event));
}

#[test]
fn writer_publication_before_binding_flushes_one_pending_event() {
    let mut scope = WakeScope::new();
    let state = { let _entry = scope.enter(); LocalState::new(0) };
    let writer = state.writer();
    std::thread::spawn(move || { writer.set(1); writer.set(2); }).join().unwrap();
    let (tx, rx) = mpsc::channel();
    scope.bind(move |event| { let _ = tx.send(event); }).unwrap();
    assert!(scope.accepts(&rx.try_recv().unwrap()));
    assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
    assert_eq!(state.get(), 2);
}

#[test]
fn dropped_owner_leaves_all_retained_capabilities_closed() {
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

#[test]
fn task_wake_sink_runs_after_generation_lock_is_released() {
    let mut scope = WakeScope::new();
    let task = { let _entry = scope.enter(); LatestTask::new(0) };
    let reentrant_task = task.clone();
    scope.bind(move |_| {
        assert!(reentrant_task.try_cancel(3));
    }).unwrap();
    assert!(task.begin(1).complete(2));
    assert_eq!(task.state().get(), 3);
}
```

- [ ] Run RED: `CARGO_TARGET_DIR=/Users/xaver/dev/qos/target/macos-shell python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo nextest run -p ui --lib --locked --profile ci -E 'test(local_state::tests::)'`. Expect routing/channel/closed-publication assertions to fail with the old implementation. Record discovered tests and failures; do not weaken test deadlines or expectations.

#### 2B. Replace the production local-state implementation

- [ ] Delete `AppProxy`, `proxy_slot`, `set_event_loop_proxy`, `EventLoopProxyGuard`, `install_event_loop_proxy`, `wake_app`, and the old unit `WakeHandle` from `local_state.rs:10-67`. Remove `OnceLock` from imports. Add `pub use crate::wake::WakeHandle;` so the old module path remains valid.
- [ ] Add `wake: WakeHandle` to `LocalStateInner<T>`, initialize it once in `LocalState::new` with `WakeHandle::new()`, and make writer clones retain that handle. Use these exact changed bodies:

```rust
// The new field is inside the shared inner, not captured at writer() time.
struct LocalStateInner<T> {
    value: RwLock<T>,
    revision: AtomicU64,
    wake: WakeHandle,
}

// LocalState::new
pub fn new(value: T) -> Self {
    Self {
        inner: Arc::new(LocalStateInner {
            value: RwLock::new(value),
            revision: AtomicU64::new(0),
            wake: WakeHandle::new(),
        }),
    }
}

// LocalState::replace: insert before taking the value write lock.
if !self.inner.wake.authorize_publication() {
    return None;
}

// LocalState::try_update: insert before taking the value write lock.
if !self.inner.wake.authorize_publication() {
    return false;
}

// LocalState::writer
pub fn writer(&self) -> LocalStateWriter<T>
where
    T: Send + Sync + 'static,
{
    LocalStateWriter {
        state: self.clone(),
        wake: self.inner.wake.clone(),
    }
}

pub struct LocalStateWriter<T> {
    state: LocalState<T>,
    wake: WakeHandle,
}

impl<T> Clone for LocalStateWriter<T> {
    fn clone(&self) -> Self {
        Self { state: self.state.clone(), wake: self.wake.clone() }
    }
}

// Replace the two `(self.wake)();` calls with `self.wake.wake();`.
// Do not add a second lifetime authorization in writer methods: state already
// authorizes the actual mutation. Wake authorization only gates notification.
```

Keep `next_revision`, `replace`'s storage ordering, and `try_update`'s existing write-lock/callback/revision ordering otherwise unchanged. This covers direct LocalState mutation and `LocalBinding::publish_by`, not just background writers. Direct LocalState mutations keep their existing no-wake behavior; writer/completion paths schedule wakes.

- [ ] Remove `LocalStateWriter::for_test`. Replace the existing `background_writer_wakes_once_after_each_completed_mutation` test's injected callback setup with a bound scope, construct the state under its guard, then use `state.writer()`. After its two mutations assert `rx.try_iter().count() == 2`; retain value and revision assertions. `AtomicUsize` remains needed by the exhaustion callback test.
- [ ] Add `wake: WakeHandle` to `LatestRequest<T>`, initialize at `new`, and clone that field into *both* successful and fallback `RequestCompletion` values. Begin must not recapture TLS. Insert `if !self.wake.authorize_publication() { return None; }` at the start of `try_begin`, before `fetch_update`; retain its checked atomic generation allocator and reserved terminal marker.
- [ ] Replace the following request methods:

```rust
// LatestRequest
#[must_use]
pub fn is_pending(&self) -> bool {
    self.wake.is_active() && self.pending.is_some()
}

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

// RequestCompletion
#[must_use]
pub fn is_current(&self) -> bool {
    self.request_generation != 0
        && self.wake.is_active()
        && self.generation.load(Ordering::Acquire) == self.request_generation
}

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
```

`try_cancel` retains existing generation invalidation semantics, including while closed; cancellation itself publishes no result. `try_take` is a consumer check, not a second channel-send authorization. A closed inbox drops previously queued values and never supplies them to a replacement mount.

- [ ] For `TaskCompletion::is_current`, return false for `run == 0` or inactive `self.writer.wake` before locking generation; retain equality check under the existing mutex. Replace `complete` with:

```rust
pub fn complete(self, value: T) -> bool {
    let generation = self.generation.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
```

Leave LatestTask's begin/cancel critical sections intact: they publish running/cancelled state through `LocalState::try_set`, which now supplies authorization. If it rejects, generation and state remain unchanged. `begin` may return its existing `run: 0` fallback; the explicit zero check above makes it permanently stale even when a closed owner never issued a generation.

- [ ] Update all affected API docs in `local_state.rs`: false includes closed owner or revision/generation exhaustion; true is acceptance only; direct handles retain their creation owner; `new` outside any scope is detached permanently. Remove all claims about process-global automatic proxy installation. Keep examples explaining App factory startup.
- [ ] In `lib.rs`, remove `WakeHandle` from the `pub use local_state` list and add:

```rust
pub use wake::{WakeBindError, WakeEvent, WakeHandle, WakeScope, WakeScopeGuard};
```

The module-local `pub use` in `local_state.rs` preserves that established path. Do not create two implementations.
- [ ] Change each of the three lines `let wake = self.wake;` to `let wake = self.wake.clone();`: `ui/src/clipboard.rs:981`; `apps/settings/src/panes/updates.rs:1563,1609`. No channel/protocol rewrite. The Linux clipboard backend is constructed in AppHandler `resumed`, which Task 2D scopes before backend installation.

#### 2C. Make App own the scope before construction

- [ ] Add the field `wake_scope: Option<crate::wake::WakeScope>` as the **first** App field. Refactor the existing `with_collaboration_manager` initialization body into private `with_parts`, passing `wake_scope` into the existing `Self { ... }` initializer. Keep every existing App field initializer and startup projection unchanged. Enter the supplied/explicitly detached context at the beginning of `with_parts`, before theme/render/widget setup:

```rust
fn with_parts(
    wake_scope: Option<crate::wake::WakeScope>,
    state: State,
    root_widget: Box<dyn Widget>,
    collab_manager: CollaborationManager,
) -> Self {
    let _entry = wake_scope.as_ref().map_or_else(
        crate::wake::WakeScope::enter_detached,
        crate::wake::WakeScope::enter,
    );
    let configured_theme = Theme::configured();
    let mut app = Self {
        wake_scope,
        state,
        root_widget,
        text_renderer: TextRenderer::new(),
        taffy: TaffyTree::new(),
        root_node: None,
        node_map: std::collections::HashMap::new(),
        layout_dirty: true,
        structure_dirty: false,
        nav_gestures: crate::gesture::NavGestureRecognizer::new(),
        needs_sync: true,
        // Finalised below, once base_theme/a11y/text_scale all exist.
        last_frame_size: None,
        last_layout_px: None,
        theme: configured_theme.clone(),
        debug_mode: cfg!(debug_assertions) && std::env::var("QUICK_UI_DEBUG").is_ok(),
        focused_node: None,
        focusable_order: Vec::new(),
        autofocus_first: false,
        autofocus_on_structure_change: false,
        appearance_reconcile: false,
        theme_store_stamp: settings_store::settings_path()
            .and_then(|path| std::fs::metadata(path).ok()?.modified().ok()),
        last_theme_store_check: Instant::now(),
        modifiers: winit::event::Modifiers::default(),
        interaction: crate::interaction::InteractionEnvironment::default(),
        platform: crate::platform::PlatformConventions::default(),
        last_stable_focus_idx: None,
        last_stable_metadata: None,
        collab_manager,
        local_author: crate::collaboration::local_user_id(),
        presence_ctx: Self::initial_presence_context(),
        permission_ctx: PermissionContext::permissive(),
        presence_broadcast_scope: None,
        pointer_space: None,
        followed_peer: None,
        last_pointer_broadcast: None,
        last_focus_presence: None,
        last_location_presence: None,
        last_view_presence: None,
        presence_broadcast_sink: None,
        frame_tick: None,
        current_cursor: CursorIcon::Default,
        tooltip_host: crate::widget::TooltipHost::new(),
        // Auto-enable if the shell mirrored QOS_A11Y_* into this sandbox.
        screen_reader: if Self::screen_reader_env_enabled()
            || Self::persisted_accessibility_bool("screen_reader")
        {
            Some(Self::configured_screen_reader())
        } else {
            None
        },
        screen_reader_modifier_down: false,
        live_region_last: std::collections::HashMap::new(),
        focus_trap_active: false,
        focus_before_trap: None,
        base_theme: configured_theme,
        a11y: crate::a11y::A11ySettings::default(),
        text_scale: 1.0,
        ui_scale: 1.0,
        fullscreen_requested: false,
        fullscreen_active: false,
        autosave: None,
        records: None,
        room_index: None,
        action_palette: None,
        action_registry: qos_actions::ActionRegistry::new(),
        action_transport: Box::new(qos_actions::LocalTransport::new()),
        resource_cache: RefCell::new(None),
        local_resource_cache: RefCell::new(None),
        resource_generation: Cell::new(0),
        permission_generation: Cell::new(0),
        last_control_availability: None,
        last_published_actions: None,
        action_directory_cursors: std::collections::HashMap::new(),
        last_published_continuation: None,
        pending_continuation_restore: crate::collaboration::continuation_restore_env(),
        pending_continuation_view: None,
        directed_invocations: std::collections::HashMap::new(),
        local_invocations: std::collections::HashMap::new(),
        next_broker_request_id: Some(0),
        semantic_tree: crate::a11y_tree::SemanticTree::new(),
        profiler: crate::profiling::FrameProfiler::from_env(),
        window_title: None,
        window_app_id: None,
        floating_surface_client:
            crate::floating_surface::FloatingSurfaceServiceClient::discover(),
        last_floating_presentations: std::collections::HashMap::new(),
        last_active_floating_ids: std::collections::HashSet::new(),
        prepared_floating_presentations: std::collections::HashSet::new(),
        floating_coordinate_offsets: std::collections::HashMap::new(),
        suspended_floating_presentations: std::collections::HashSet::new(),
        floating_host_messages: Vec::new(),
    };

    app.a11y.high_contrast = Self::high_contrast_env_enabled();
    app.a11y.reduce_motion = Self::reduce_motion_env_enabled();
    app.a11y.screen_reader = app.screen_reader.is_some();
    app.text_scale = Self::text_scale_env().unwrap_or(1.0);
    app.ui_scale = Self::ui_scale_env().unwrap_or(1.0);
    app.rebuild_theme();

    // The scope is also what sends `Arrived`, so an app that never set
    // one would never catch up on anything written before it opened.
    // No room means no scope.
    if let Some(scope) = PresenceKey::for_local_workspace() {
        app.set_presence_broadcast_scope(Some(scope));
    }
    app.presence_ctx.set_surface_id(crate::local_surface_id());
    // Apply launcher-provided continuation before the first widget sync or
    // redraw.  Waiting for an input event left a restored app blank on a
    // quiet freshly-created window.
    app.apply_pending_continuation_restore();
    // The first report is level-triggered.  Sending it here makes a
    // launched app visible to continuity capture even if it receives no
    // input events before the next peer asks for a checkpoint.
    app.publish_continuation_metadata();
    app
}
```

The complete initializer above retains the baseline `app.rs:680-799` behavior and adds only scope ownership/entry. Apply its existing narrow too-many-lines allowance to `with_parts`.

- [ ] Implement public constructors and entry with these bodies:

```rust
pub fn new(state: State, root_widget: Box<dyn Widget>) -> Self {
    let _entry = crate::wake::WakeScope::enter_detached();
    Self::with_collaboration_manager(state, root_widget, CollaborationManager::new())
}

pub fn with_collaboration_manager(
    state: State,
    root_widget: Box<dyn Widget>,
    collab_manager: CollaborationManager,
) -> Self {
    Self::with_parts(None, state, root_widget, collab_manager)
}

pub fn from_factory(state: State, factory: impl FnOnce() -> Box<dyn Widget>) -> Self {
    let scope = crate::wake::WakeScope::new();
    let _entry = scope.enter();
    let root = factory();
    Self::from_scoped_parts(scope, state, root)
}

pub fn try_from_factory<E>(
    state: State,
    factory: impl FnOnce() -> Result<Box<dyn Widget>, E>,
) -> Result<Self, E> {
    let scope = crate::wake::WakeScope::new();
    let _entry = scope.enter();
    let root = factory()?;
    Ok(Self::from_scoped_parts(scope, state, root))
}

pub fn from_scoped_parts(
    wake_scope: crate::wake::WakeScope,
    state: State,
    root_widget: Box<dyn Widget>,
) -> Self {
    // Enter before CollaborationManager::new as well as all App projections.
    let _entry = wake_scope.enter();
    Self::with_parts(Some(wake_scope), state, root_widget, CollaborationManager::new())
}

pub fn enter_wake_scope(&self) -> crate::wake::WakeScopeGuard {
    self.wake_scope.as_ref().map_or_else(
        crate::wake::WakeScope::enter_detached,
        crate::wake::WakeScope::enter,
    )
}

fn close_wake_scope(&mut self) {
    if let Some(scope) = self.wake_scope.as_mut() {
        scope.close();
    }
}

fn accept_wake(&mut self, event: &crate::wake::WakeEvent) -> bool {
    if !self.wake_scope.as_ref().is_some_and(|scope| scope.accepts(event)) {
        return false;
    }
    self.needs_sync = true;
    true
}
```

The `state: State` argument to `from_factory`/`try_from_factory` has already been evaluated before the new scope exists. These helpers scope the root factory and App setup, not earlier state/model construction. Use `from_scoped_parts` with a scope entered before setup whenever document and widget construction are coupled or a state loader creates wake-capable objects. Standard LaunchSpec and all audited custom live paths do this.

On factory error or unwind, `_entry` drops before local `scope`, restoring TLS and then closing the owner. On success the same owner moves into App; a guard stores only a weak identity and does not borrow the moved value. Never substitute `WakeScope::new()` inside `from_scoped_parts`.

- [ ] Document `App::new` and `with_collaboration_manager` as explicitly detached **execution** for prebuilt headless/test roots. They cannot change handles already constructed under another scope. In headless callers nested inside a host, put `WakeScope::enter_detached` around construction too. Document `from_scoped_parts`'s preconstruction rule and one-time binding; no retroactive adoption of detached/foreign captures.
- [ ] Implement `Drop for App` to close before any field is dropped. Enter that now-closed scope while destroying the actual root so destructor-created state cannot capture an ambient parent app:

```rust
impl Drop for App {
    fn drop(&mut self) {
        self.close_wake_scope();
        let _entry = self.enter_wake_scope();
        let root = std::mem::replace(
            &mut self.root_widget,
            Box::new(crate::widget::Container::new()),
        );
        drop(root);
    }
}
```

Adding `Drop` forbids moving individual owned fields out of App (Rust E0509). The inspected by-value methods `with_autosave`, `with_window_title`, `with_window_app_id`, and `with_records` mutate fields then return the whole App; `run`/`run_with_setup` transfer the whole App, including `app: self` into AppHandler. No inspected method destructures App or moves an owned field out without `take`/`replace`. Preserve this property, and require the all-target compile gate to catch test-only or newly added moves. Any necessary extraction must use `Option::take`/`mem::replace` while preserving the same scope owner; never work around Drop by forgetting the App.

The replacement is an empty framework Container solely to permit owned root teardown under the guard. It receives no app callbacks. Do not flush autosave or unmount native views a second time from Drop; the normal `exiting` lifecycle retains those operations. `wake_scope` being the first field additionally drops the sink before the remaining App fields.

#### 2D. Bind the native destination and cover callback boundaries

- [ ] Change `UserEvent::Wake` to `Wake(crate::wake::WakeEvent)`; retain `Debug, Clone` on UserEvent.
- [ ] In `run_with_setup`, before event-loop creation reject a detached owner, then bind **that existing owner** to a cloned proxy and enter the App scope around `setup` and `init_layout`. Replace only the old global guard installation; keep existing AccessKit proxy and AppHandler construction:

```rust
assert!(
    self.wake_scope.is_some(),
    "live App requires from_factory, try_from_factory, or from_scoped_parts"
);
let event_loop = EventLoop::<UserEvent>::with_user_event().build().unwrap();
let proxy = event_loop.create_proxy();
let wake_proxy = proxy.clone();
self.wake_scope.as_mut().unwrap().bind(move |event| {
    let _ = wake_proxy.send_event(UserEvent::Wake(event));
}).expect("App wake scope must be open and unbound at native startup");
let a11y_proxy = proxy.clone();
{
    let _entry = self.enter_wake_scope();
    setup(proxy);
    self.init_layout();
}
```

Extend the existing localized startup-panic lint allowance to the assertion/expect as required by repository lints; do not weaken crate-wide lints. Pending factory wakes flush through the real proxy on bind; App starts `needs_sync = true` even without a pending event.

- [ ] At the beginning of `AppHandler::user_event`, enter the App context and handle `Wake(event)` **before** the current `let Some(window)` early return:

```rust
let _entry = self.app.enter_wake_scope();
let event = match event {
    UserEvent::Wake(wake) => {
        if self.app.accept_wake(&wake) {
            if let Some(window) = self.window.as_ref() {
                window.request_redraw();
            }
        }
        return;
    }
    other => other,
};
```

Remove the old mutating Wake arm from the subsequent match. That exhaustive match still needs `UserEvent::Wake(_) => {}` because Rust does not narrow enum types after the early match. Its arm is an inert already-handled case. Foreign/closed events never set `needs_sync`, request redraw, or create a window/app. Current events set dirty even when no window exists yet.

- [ ] Put `let _entry = self.app.enter_wake_scope();` at the top of `AppHandler::{resumed, window_event, about_to_wait}` and every existing private `AppHandler` instance method that calls app/widget code (`sync_audience_presentation`, `close_audience_presentation`, `handle_audience_redraw`, `sync_ime`, `create_native_floating_window`, `suspend_native_floating_window`, `close_native_floating_window`, `apply_floating_host_messages`, `recover_stalled_floating_windows`, `sync_native_floating_geometry`, `handle_floating_redraw`, `dispatch_floating_event`, `handle_redraw_requested`, `handle_other_window_event`). Nested entries are valid and return guards hold no App borrow. `init_render` is renderer-only and needs no guard.
- [ ] At the start of `AppHandler::exiting`, in this order, add:

```rust
self.app.close_wake_scope();
let _entry = self.app.enter_wake_scope();
```

Then run the existing AccessKit shutdown, autosave flush, native-view unmount, audience teardown, floating-render shutdown, and primary render shutdown. Work spawned from unmount captures the closed owner. Closing a floating/audience window alone does **not** close App; those methods only enter the scope. Preserve suspension behavior.

- [ ] Add `let _entry = self.enter_wake_scope();` at the beginning of these callable App boundaries, before user-defined widget/provider/transport calls:

```text
collect_resources; control_action_coverage; open_action_palette;
set_action_transport; set_text_scale; set_ui_scale;
set_interaction_environment; set_platform_conventions;
set_high_contrast; toggle_high_contrast; set_reduce_motion; set_theme;
set_screen_reader_enabled; set_screen_reader;
set_presence_broadcast_scope; broadcast_presence;
try_follow_click; set_permission_ctx; follow; unfollow;
poll_and_apply_collab_events; pump_collab_presence; apply_collab_event_for_test;
build_a11y_tree_update; dispatch_a11y_action; refresh_semantic_tree;
invoke_semantic_action; init_layout; refresh_focus_order; rebuild_structure;
with_records; record_tick; reconcile_appearance
```

Also enter in `FreezeThaw for App::{capture_view,restore_view}`; these can invoke widget code without passing AppHandler. `serialize_state`/`restore_state` only manipulate State and require no callback entry. The closure-only registration/accessor methods return/store values and invoke no application callback; their callers should use `enter_wake_scope` if the *arguments themselves* construct state. `LaunchSpec.configure_app` below does so. Existing private App functions reached through these guarded entry points inherit scope. Audit any `root_widget`/`node_map` call added or moved during implementation against this boundary map; add a guard to a newly exposed entry before considering the task complete.

#### 2E. Standard launch and explicit detached CLI execution

- [ ] Extract `build_native_app` from `run_native_app`. Establish the owner before `open_state_with_args`, `open_state`, `live_root`, text-scale application and `configure_app`:

```rust
fn build_native_app(spec: &LaunchSpec, args: &LaunchArgs) -> Result<App, LaunchError> {
    let scope = crate::wake::WakeScope::new();
    let _entry = scope.enter();
    let (state, autosave) = spec.open_state_with_args.map_or_else(
        || (spec.open_state)(spec.app_id),
        |open| open(spec.app_id, args),
    );
    let root = spec.live_root(args)?;
    let mut app = App::from_scoped_parts(scope, state, root).with_autosave(autosave);
    if let Some(text_scale) = args.text_scale {
        app.set_text_scale(text_scale);
    }
    if let Some(configure) = spec.configure_app {
        let _configure_entry = app.enter_wake_scope();
        configure(&mut app);
    }
    Ok(app)
}

fn run_native_app(spec: &LaunchSpec, args: &LaunchArgs) -> Result<(), LaunchError> {
    build_native_app(spec, args)?.run();
    Ok(())
}
```

- [ ] Put `let _detached = crate::wake::WakeScope::enter_detached();` at the start of `run_with_args`, before argument parsing/command dispatch. This explicitly keeps command/shot/responsive/headless/native-audit factories detached even when a tool invokes CLI dispatch under another host's callback. Native construction creates/enters its fresh child scope; each custom live_handler below does the same. This is construction policy, not background-worker retargeting.

#### 2F. Migrate every custom live constructor and example in this same task

Each row uses **existing** construction/setup statements inside an explicitly entered owner. Add the exact prelude before the earliest indicated statement:

```rust
let wake_scope = ui::wake::WakeScope::new();
let _wake_entry = wake_scope.enter();
```

Replace the App constructor at that row with its listed expression. Do not create the scope after constructing a widget, stateful engine, model, or root. The function-local guard may span the run call: handler guards are nested and dropped before it, and it contains no App borrow. Preserve special engine/PTY/input/auth/install payload hooks unchanged.

| File/function | Prelude insertion | Exact replacement constructor |
| --- | --- | --- |
| `apps/browser/src/main.rs::run_live` | First statement, before `browser_widget(args)` | `ui::App::from_scoped_parts(wake_scope, state, Box::new(widget))` |
| `apps/terminal/src/main.rs::run_live` | First statement, before `TerminalWidget::spawn_default()` | `ui::App::from_scoped_parts(wake_scope, state, Box::new(widget))` |
| `apps/osk/src/main.rs::run_always_visible` | First statement, before `default_injector()` | `ui::App::from_scoped_parts(wake_scope, state, Box::new(widget))` |
| `apps/osk/src/main.rs::run_auto_show` | First statement, before `Arc::new(Mutex::new(im))` | `ui::App::from_scoped_parts(wake_scope, state, Box::new(widget))` |
| `apps/pdf_viewer/src/main.rs::live` | First statement, before `RecordSet::open(APP_ID)` | `ui::App::from_scoped_parts(wake_scope, ui::State::new(), build_widget(args)?)` |
| `apps/word-processor/src/main.rs::start_live` | First statement, before `build(file_arg(args))?` | `App::from_scoped_parts(wake_scope, state, widget)` |
| `crates/session/installer/src/main.rs::main` live branch | After screenshot branch returns, before `ProxySlot` creation | `ui::App::from_scoped_parts(wake_scope, state, Box::new(widget))` |
| `crates/session/greeter/src/main.rs::main` live branch | After screenshot branch returns, before `proxy_slot` creation | `ui::App::from_scoped_parts(wake_scope, state, Box::new(widget))` |
| `shell/src/compositor/hosted.rs::run_with_full_options` | First statement, before options destructuring and all tree/overlay-host setup | `App::from_scoped_parts(wake_scope, state, root)` |
| `ui/examples/channel_browser.rs::run` | First statement, before entries/model/browser construction | `App::from_scoped_parts(wake_scope, state, Box::new(browser))` |
| `ui/examples/conflict_picker.rs::run` | First statement, before conflict/banners construction | `App::from_scoped_parts(wake_scope, state, root_widget)` |
| `ui/examples/dashboard.rs::run` | First statement, before state/widget setup | `App::from_scoped_parts(wake_scope, state, root)` |
| `ui/examples/gallery.rs::run` | First statement, before state and `build_gallery(&mut state, ...)` | `App::from_scoped_parts(wake_scope, state, root_widget)` |
| `ui/examples/identity_flow_demo.rs::run` | First statement, before store/IdentityFlow construction | `App::from_scoped_parts(wake_scope, state, Box::new(flow))` |
| `ui/examples/responsive_gallery.rs::run` | First statement, before state load and root construction | `App::from_scoped_parts(wake_scope, state, root_widget)` |
| `ui/examples/widget_showcase.rs::run` | First statement, before state/tree/widget setup | `App::from_scoped_parts(wake_scope, state, root)` |

For installer and greeter screenshot branches, add an explicit `let _detached = ui::wake::WakeScope::enter_detached();` at `main` entry; the later live entry nests inside it. Their headless render code remains unchanged.

Activity timeline branches between live and shot **after** building its root. At `run` entry, add a detached guard. Immediately after parsing `shot`, add:

```rust
let wake_scope = shot.is_none().then(ui::wake::WakeScope::new);
let _wake_entry = wake_scope.as_ref().map(ui::wake::WakeScope::enter);
```

Keep document/history/widget creation after that entry. Replace only its final match:

```rust
match (shot, wake_scope) {
    (Some(out), _) => render_shot(root_widget, (860, 660), Path::new(&out)),
    (None, Some(scope)) => App::from_scoped_parts(scope, State::new(), root_widget).run(),
    (None, None) => unreachable!("live activity timeline always allocates its scope"),
}
```

Use a narrow invariant-explaining lint allowance if examples deny `unreachable!`. All guards remain in LIFO order. The shot branch is explicitly detached and never binds a host.

The following baseline prebuilt App sites intentionally remain detached: `apps/windows/src/main.rs:113`, `apps/tube/src/main.rs:1769`, `apps/file-explorer/src/files_app.rs:11698`, `crates/session/greeter/src/greeter_app.rs:2297,2823`, all `ui` unit/integration test prebuilt constructors, `apps/contacts/tests/diag_focus.rs:25`, and `shell/src/bin/shots/mode.rs:406`. They are tests or headless. They must not call native `run`. Do not mechanically turn them into live factories.

- [ ] Update the two documentation snippets in `ui/src/persist.rs` and the snippet in `App::with_autosave` to use this form:

```rust
let (state, autosave) = ui::persist::open("dev.qos.my-app");
ui::App::from_factory(state, my_app::app_root)
    .with_autosave(autosave)
    .run();
```

The no-persistence snippet is `ui::App::from_factory(ui::State::new(), my_app::app_root).run();`. Do not demonstrate a closure that merely returns an already-created root.

#### 2G. Add App factory, receiver, callback and teardown regression tests

- [ ] Add `#[cfg(test)] mod wake_integration_tests` to `app.rs` with the following fixture and tests. These exercise production App methods without constructing a native event loop or GPU renderer.

```rust
#[cfg(test)]
mod wake_integration_tests {
    use super::*;
    use crate::{LatestRequest, LocalState};
    use crate::wake::{WakeEvent, WakeHandle, WakeScope};
    use crate::view::{FreezeThaw, ViewContribution};
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::mpsc::{self, Receiver, TryRecvError};

    fn bound_scope() -> (WakeScope, Receiver<WakeEvent>) {
        let (tx, rx) = mpsc::channel();
        let mut scope = WakeScope::new();
        scope.bind(move |event| { let _ = tx.send(event); }).unwrap();
        (scope, rx)
    }

    type Captures = Rc<RefCell<Vec<(&'static str, LatestRequest<u32>)>>>;

    struct CaptureRoot { captures: Captures }
    impl CaptureRoot {
        fn capture(&self, label: &'static str) {
            self.captures.borrow_mut().push((label, LatestRequest::new()));
        }
    }
    impl Drop for CaptureRoot {
        fn drop(&mut self) { self.capture("drop"); }
    }
    impl Widget for CaptureRoot {
        fn sync(&mut self, _: &mut automerge::AutoCommit, _: &automerge::ObjId) {
            self.capture("sync");
        }
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _: &Theme,
            node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            self.capture("layout");
            let node = taffy.new_leaf(taffy::Style::default()).unwrap();
            node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
            node
        }
        fn apply_layout(&mut self, _: &TaffyTree<()>, _: NodeId, _: Point) {}
        fn draw(
            &mut self, _: &mut vello::Scene, _: vello::kurbo::Affine,
            _: &mut TextRenderer, _: &Theme, _: &PresenceCtx,
        ) {}
        fn handle_event(
            &mut self, _: &WindowEvent, _: f64, _: &winit::event::Modifiers,
            _: &mut automerge::AutoCommit, _: &automerge::ObjId, _: &mut bool,
            _: &mut Option<NodeId>, _: NodeId, _: &TaffyTree<()>,
        ) { self.capture("event"); }
        fn name(&self) -> &str { "capture-root" }
        fn get_rect(&self) -> vello::kurbo::Rect { vello::kurbo::Rect::ZERO }
        fn view_state(&self) -> Option<ViewContribution> {
            self.capture("capture-view");
            Some(ViewContribution::default())
        }
        fn restore_view_state(&mut self, _: &ViewContribution) {
            self.capture("restore-view");
        }
    }

    #[test]
    fn factory_state_stays_owned_before_binding_and_after_return() {
        let mut captured = None;
        let mut app = App::from_factory(State::new(), || {
            let state = LocalState::new(0);
            state.writer().set(1);
            captured = Some(state);
            Box::new(crate::widget::Text::new("root"))
        });
        let state = captured.unwrap();
        let (tx, rx) = mpsc::channel();
        app.wake_scope.as_mut().unwrap().bind(move |event| {
            let _ = tx.send(event);
        }).unwrap();
        let pending = rx.try_recv().unwrap();
        app.needs_sync = false;
        assert!(app.accept_wake(&pending));
        assert!(app.needs_sync);
        assert_eq!(state.get(), 1);
        drop(app);
        assert!(!state.writer().try_set(2));
    }

    #[test]
    fn transfer_preserves_the_exact_owner_and_rejects_foreign_events() {
        let (scope, rx) = bound_scope();
        let (other, other_rx) = bound_scope();
        let state = { let _entry = scope.enter(); LocalState::new(0) };
        let mut app = App::from_scoped_parts(
            scope, State::new(), Box::new(crate::widget::Text::new("root")),
        );
        state.writer().set(1);
        let current = rx.try_recv().unwrap();
        other.handle().wake();
        let foreign = other_rx.try_recv().unwrap();
        app.needs_sync = false;
        assert!(!app.accept_wake(&foreign));
        assert!(!app.needs_sync);
        assert!(app.accept_wake(&current));
        assert!(app.needs_sync);
        app.close_wake_scope();
        app.needs_sync = false;
        assert!(!app.accept_wake(&current));
        assert!(!app.needs_sync);
    }

    #[test]
    fn fallible_factory_error_closes_captures_and_restores_parent_context() {
        let (outer, rx) = bound_scope();
        let _outer = outer.enter();
        let mut captured = None;
        let result: Result<App, &'static str> = App::try_from_factory(State::new(), || {
            captured = Some(LocalState::new(0));
            Err("factory failed")
        });
        assert!(matches!(result, Err("factory failed")));
        assert!(!captured.unwrap().writer().try_set(1));
        WakeHandle::new().wake();
        assert!(outer.accepts(&rx.try_recv().unwrap()));
    }

    #[test]
    fn factory_unwind_closes_captures_and_restores_parent_context() {
        let (outer, rx) = bound_scope();
        let _outer = outer.enter();
        let mut captured = None;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            App::from_factory(State::new(), || {
                captured = Some(LocalState::new(0));
                panic!("factory panicked");
            })
        }));
        assert!(result.is_err());
        assert!(!captured.unwrap().writer().try_set(1));
        WakeHandle::new().wake();
        assert!(outer.accepts(&rx.try_recv().unwrap()));
    }

    #[test]
    fn callback_created_requests_keep_app_owner_when_begun_elsewhere() {
        let (scope, rx) = bound_scope();
        let (other, other_rx) = bound_scope();
        let captures = Captures::default();
        let root = {
            let _entry = scope.enter();
            Box::new(CaptureRoot { captures: Rc::clone(&captures) })
        };
        let mut app = App::from_scoped_parts(scope, State::new(), root);
        {
            let _other = other.enter();
            app.init_layout();
            app.refresh_focus_order();
            let view = app.capture_view();
            app.restore_view(&view);
        }
        let mut requests = std::mem::take(&mut *captures.borrow_mut());
        for label in ["layout", "sync", "capture-view", "restore-view"] {
            assert!(requests.iter().any(|(actual, _)| *actual == label));
        }
        for (_, mut request) in requests.drain(..) {
            let completion = { let _other = other.enter(); request.begin() };
            assert!(std::thread::spawn(move || completion.complete(7)).join().unwrap());
            assert!(app.accept_wake(&rx.try_recv().unwrap()));
            assert_eq!(request.try_take(), Some(7));
        }
        assert!(matches!(other_rx.try_recv(), Err(TryRecvError::Empty)));
        {
            let _other = other.enter();
            drop(app);
        }
        let mut after_drop = std::mem::take(&mut *captures.borrow_mut());
        assert!(after_drop.iter().any(|(label, _)| *label == "drop"));
        for (_, request) in &mut after_drop {
            assert!(request.try_begin().is_none());
        }
        assert!(matches!(other_rx.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn detached_app_callbacks_mask_an_ambient_host() {
        let (outer, rx) = bound_scope();
        let _outer = outer.enter();
        let captures = Captures::default();
        let mut app = App::new(State::new(), Box::new(CaptureRoot {
            captures: Rc::clone(&captures),
        }));
        app.refresh_focus_order();
        let mut requests = std::mem::take(&mut *captures.borrow_mut());
        assert!(!requests.is_empty());
        for (_, request) in &mut requests {
            assert!(request.begin().complete(1));
            assert_eq!(request.try_take(), Some(1));
        }
        assert!(matches!(rx.try_recv(), Err(TryRecvError::Empty)));
        WakeHandle::new().wake();
        assert!(outer.accepts(&rx.try_recv().unwrap()));
    }

    #[test]
    #[should_panic(expected = "live App requires from_factory")]
    fn detached_native_startup_fails_before_creating_an_event_loop() {
        App::new(State::new(), Box::new(crate::widget::Text::new("root"))).run();
    }
}
```

For the fixture's required ten-argument `handle_event`, use the repository's existing narrow `clippy::too_many_arguments` test allowance. No unsafe scope operations are needed; the raw node-map insertion is the normal Widget layout contract.

- [ ] Before adding the App implementation sections 2C–2F, run RED with `CARGO_TARGET_DIR=/Users/xaver/dev/qos/target/macos-shell python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo nextest run -p ui --lib --locked --profile ci -E 'test(app::wake_integration_tests::)'`. Expect missing App factory/entry/receiver APIs. Sequence this RED step immediately after adding these tests, before implementing 2C; the numbered sections group instructions by responsibility, not permission to skip RED.

#### 2H. Prove standard LaunchSpec construction/configuration share the owner

- [ ] Add to `launch::tests`:

```rust
thread_local! {
    static LAUNCH_WAKE_CAPTURES: std::cell::RefCell<Vec<crate::WakeHandle>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

fn capture_launch_wake() {
    LAUNCH_WAKE_CAPTURES.with(|captures| captures.borrow_mut().push(crate::WakeHandle::new()));
}

fn scoped_open_state(_: &'static str) -> (State, crate::persist::Autosave) {
    capture_launch_wake();
    (State::new(), crate::persist::Autosave::disabled("wake routing test"))
}

fn scoped_root() -> Box<dyn Widget> {
    capture_launch_wake();
    Box::new(crate::widget::Text::new("scope test"))
}

fn scoped_configure(_: &mut App) {
    capture_launch_wake();
}

#[test]
fn standard_live_factory_and_configuration_capture_the_same_app_lifetime() {
    LAUNCH_WAKE_CAPTURES.with(|captures| captures.borrow_mut().clear());
    let spec = LaunchSpec::new("dev.qos.wake-test", scoped_root)
        .open_state(scoped_open_state)
        .configure_app(scoped_configure);
    let args = parse_args_extended(&strings(&["app"]), &[], &[], &[], &[], 0).unwrap();
    let mut outer = crate::WakeScope::new();
    let _outer = outer.enter();
    let app = build_native_app(&spec, &args).unwrap();
    let captured = LAUNCH_WAKE_CAPTURES.with(|captures| std::mem::take(&mut *captures.borrow_mut()));
    assert_eq!(captured.len(), 3);
    assert!(captured.iter().all(crate::WakeHandle::is_active));
    // Closing the parent must not close any child construction capture.
    outer.close();
    assert!(captured.iter().all(crate::WakeHandle::is_active));
    drop(app);
    assert!(captured.iter().all(|handle| !handle.is_active()));
}
```

This test uses disabled autosave; it does not discover/open user storage. Native App fields use existing configuration readers. Run these tests through the isolated environment below so configuration, identity and service discovery cannot resolve the operator’s state. Do not invoke real app factories, identity enrollment, or write user settings.

- [ ] Add a detached utility callback test using a command handler to avoid native-window/headless-process exit. This proves the actual `run_with_args` dispatch masks an ambient host:

```rust
fn capture_command_wake(_: &LaunchArgs) -> Result<bool, LaunchError> {
    capture_launch_wake();
    Ok(true)
}

#[test]
fn cli_utility_capture_stays_detached_under_a_host_callback() {
    LAUNCH_WAKE_CAPTURES.with(|captures| captures.borrow_mut().clear());
    let mut outer = crate::WakeScope::new();
    let _outer = outer.enter();
    let spec = LaunchSpec::new("dev.qos.wake-utility", dummy_root)
        .command_handler(capture_command_wake);
    run_with_args(&spec, &strings(&["app"])).unwrap();
    outer.close();
    let captures = LAUNCH_WAKE_CAPTURES.with(|captures| std::mem::take(&mut *captures.borrow_mut()));
    assert_eq!(captures.len(), 1);
    assert!(captures.iter().all(crate::WakeHandle::is_active));
}
```

Use the existing signature-mandated `clippy::unnecessary_wraps` allowance for the test command handler, matching adjacent `custom_command` tests. Record RED before the helper/detached dispatch implementation: missing `build_native_app`, and the old dispatch incorrectly closes the utility capture when outer closes.

#### 2I. Verify the complete cutover before committing

- [ ] Run the focused combined portable tests:

```bash
CARGO_TARGET_DIR=/Users/xaver/dev/qos/target/macos-shell python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo nextest run -p ui --lib --locked --profile ci -E 'test(wake::tests::) or test(local_state::tests::) or test(app::wake_integration_tests::) or test(launch::tests::)'
CARGO_TARGET_DIR=/Users/xaver/dev/qos/target/macos-shell python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo test -p ui --doc --locked
CARGO_TARGET_DIR=/Users/xaver/dev/qos/target/macos-shell cargo check -p ui --all-targets --locked
CARGO_TARGET_DIR=/Users/xaver/dev/qos/target/macos-shell cargo clippy -p ui --lib --locked -- -D warnings
```

Require every preexisting generation/revision exhaustion test still passes, including no invocation of a revision-exhausted callback. Require the new real-sink reentrancy test; source inspection alone is insufficient to establish that the generation lock releases before wake. Compile/doc failures caused by non-`Copy` WakeHandle must be fixed in this task.

- [ ] Audit every constructor and copy consumer with:

```bash
rg -n 'install_event_loop_proxy|proxy_slot|wake_app|EventLoopProxyGuard' ui/src/local_state.rs ui/src/app.rs
rg -n 'UserEvent::Wake' ui apps shell crates --glob '*.rs'
rg -n 'WakeHandle|let wake = self\.wake' ui apps shell crates --glob '*.rs'
rg -n 'App::(new|with_collaboration_manager)|ui::app::App|App[[:space:]]+as' ui apps shell crates --glob '*.rs'
rg -n 'live_handler\(' apps crates ui --glob '*.rs'
```

The first search must return no old-global routing symbol. `UserEvent::Wake` is payload-only. The last searches must account for all five custom handlers (browser, terminal, osk, PDF, Word Processor), both session applications, hosted shell, and all eight live examples. Remaining prebuilt App sites must match the explicit test/headless inventory above. No new aliases/wrappers may hide a production constructor. This audit is a review check, not a brittle source-text unit test.

- [ ] Run the affected live binary/example compile gates supported at the baseline on macOS, and require the **complete** affected set on Fedora at the same commit:

```bash
cargo check --locked --all-targets -p ui -p browser -p terminal -p osk -p pdf_viewer -p word-processor -p installer -p greeter -p settings -p shell
python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo test --locked -p ui --example ui-lab
```

Use each host's isolated target directory and existing supported features. A baseline macOS OS-dependency blocker is recorded with package/error and reproduced at the parent SHA; it is not grounds to alter platform cfgs or claim an unchecked target passed. Fedora must compile the Linux-only clipboard/session/OSK consumers. Do not add a permanent Mac package exclusion list to work around broader port blockers.

- [ ] Format the changed Rust files with their repository edition and check the diff for unrelated formatting. Commit **all** Task 2 code/tests/docs/constructor migrations together with `refactor(ui): route app wakes through captured instance lifetimes`. No intermediate commits containing partial live routing.
- [ ] Controller mirrors the exact SHA to Fedora and runs the focused tests, doctests, affected compile gate and example test there, recording test counts and target triples. If any gate exposes a missed constructor or callback entry, fix the cutover before proceeding. No live GUI launch, keystroke injection, or account/install action is part of this task's portable verification.

### Task 3: Verify the reusable host seam under multiple mounts and reopen

**Controller amendment from Task 2 review:** Also correct only the API documentation in `ui/src/local_state.rs` for `LatestTask::try_begin`, `LocalStateWriter::{set,try_set,update,try_update}`. State that the return value means publication was accepted, not that a wake was delivered/scheduled. Document closed-owner and exhausted-revision rejection and the task-generation rejection. Preserve implementation behavior. This is the outstanding Minor documentation/spec finding from Task 2 and is intentionally covered by this task's review; do not add implementation changes or repeat Task 2 runtime suites for a documentation edit.

**Files:** Create `ui/tests/wake_scope_host.rs`; modify `ui/tests/it.rs` to register the module. `ui/Cargo.toml` has `autotests = false` and the consolidated `it` test binary, so an unregistered file would execute no tests. No production component contract or shell catalogue is added.

**Interfaces:** Consumes production `ui::{WakeScope, WakeEvent, LocalState, LatestTask, LatestRequest}` and `WakeScope::enter_detached` from Tasks 1–2. This test host deliberately uses existing stable `u64` mount keys only inside the fixture; it does not propose a new public instance-id type. The sink sends `(mount_id, WakeEvent)`; dispatch checks lookup and then `scope.accepts` before synchronization. Never insert an entry when dispatching a wake.

- [ ] Register the test module in `ui/tests/it.rs` beside the other alphabetically ordered modules:

```rust
#[path = "wake_scope_host.rs"]
mod wake_scope_host;
```

- [ ] Add this complete conformance test file:

```rust
#![allow(
    clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing,
    clippy::string_slice, clippy::panic, clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::BTreeMap;
use std::sync::mpsc::{self, Receiver, Sender};
use ui::{LatestRequest, LatestTask, LocalState, WakeEvent, WakeScope};

type Message = (u64, WakeEvent);

struct Mount {
    scope: WakeScope,
    state: LocalState<u32>,
    syncs: usize,
}

fn mount(id: u64, tx: Sender<Message>) -> Mount {
    let mut scope = WakeScope::new();
    let state = {
        let _entry = scope.enter();
        LocalState::new(0)
    };
    scope.bind(move |event| { let _ = tx.send((id, event)); }).unwrap();
    Mount { scope, state, syncs: 0 }
}

fn dispatch(mounts: &mut BTreeMap<u64, Mount>, message: Message) -> bool {
    let (id, event) = message;
    let Some(mount) = mounts.get_mut(&id) else { return false; };
    if !mount.scope.accepts(&event) { return false; }
    let _entry = mount.scope.enter();
    mount.syncs = mount.syncs.saturating_add(1);
    true
}

fn host() -> (BTreeMap<u64, Mount>, Sender<Message>, Receiver<Message>) {
    let (tx, rx) = mpsc::channel();
    (BTreeMap::new(), tx, rx)
}

#[test]
fn two_mounts_on_one_host_sync_only_their_own_background_changes() {
    let (mut mounts, tx, rx) = host();
    mounts.insert(10, mount(10, tx.clone()));
    mounts.insert(20, mount(20, tx));
    let a = mounts.get(&10).unwrap().state.writer();
    let b = mounts.get(&20).unwrap().state.writer();
    assert!(std::thread::spawn(move || a.try_set(3)).join().unwrap());
    assert!(dispatch(&mut mounts, rx.try_recv().unwrap()));
    assert_eq!(mounts.get(&10).unwrap().syncs, 1);
    assert_eq!(mounts.get(&20).unwrap().syncs, 0);
    assert_eq!(mounts.get(&10).unwrap().state.get(), 3);
    assert_eq!(mounts.get(&20).unwrap().state.get(), 0);
    assert!(std::thread::spawn(move || b.try_set(4)).join().unwrap());
    assert!(dispatch(&mut mounts, rx.try_recv().unwrap()));
    assert_eq!(mounts.get(&10).unwrap().syncs, 1);
    assert_eq!(mounts.get(&20).unwrap().syncs, 1);
}

#[test]
fn two_hosts_keep_destinations_when_the_other_binds_and_drops() {
    let (mut a, tx_a, rx_a) = host();
    let (mut b, tx_b, rx_b) = host();
    a.insert(1, mount(1, tx_a));
    let writer_a = a.get(&1).unwrap().state.writer();
    b.insert(1, mount(1, tx_b));
    let writer_b = b.get(&1).unwrap().state.writer();
    writer_a.set(1);
    assert!(dispatch(&mut a, rx_a.try_recv().unwrap()));
    assert!(rx_b.try_recv().is_err());
    drop(a);
    assert!(!writer_a.try_set(2));
    writer_b.set(3);
    assert!(dispatch(&mut b, rx_b.try_recv().unwrap()));
    assert_eq!(b.get(&1).unwrap().state.get(), 3);
}

#[test]
fn queued_wake_cannot_recreate_missing_mount_or_reach_same_id_replacement() {
    let (mut mounts, tx, rx) = host();
    mounts.insert(7, mount(7, tx.clone()));
    let old_state = mounts.get(&7).unwrap().state.clone();
    old_state.writer().set(1);
    let queued = rx.try_recv().unwrap();
    let duplicate = queued.clone();
    let mut old = mounts.remove(&7).unwrap();
    old.scope.close();
    assert!(!dispatch(&mut mounts, duplicate));
    assert!(mounts.is_empty());
    mounts.insert(7, mount(7, tx));
    assert!(!dispatch(&mut mounts, queued));
    assert_eq!(mounts.get(&7).unwrap().syncs, 0);
    assert_eq!(mounts.get(&7).unwrap().state.get(), 0);
    assert!(!old_state.writer().try_set(2));
    drop(old);
    mounts.get(&7).unwrap().state.writer().set(9);
    assert!(dispatch(&mut mounts, rx.try_recv().unwrap()));
    assert_eq!(mounts.get(&7).unwrap().syncs, 1);
    assert_eq!(mounts.get(&7).unwrap().state.get(), 9);
}

#[test]
fn explicitly_detached_headless_work_stays_functional_after_host_creation() {
    let (state, task, mut request) = {
        let _detached = WakeScope::enter_detached();
        (LocalState::new(0), LatestTask::new(0), LatestRequest::new())
    };
    let (mut mounts, tx, rx) = host();
    mounts.insert(1, mount(1, tx));
    {
        let _host_entry = mounts.get(&1).unwrap().scope.enter();
        assert!(state.writer().try_set(1));
        assert!(task.begin(1).complete(2));
        assert!(request.begin().complete(3));
    }
    assert_eq!(state.get(), 1);
    assert_eq!(task.state().get(), 2);
    assert_eq!(request.try_take(), Some(3));
    assert!(rx.try_recv().is_err());
    assert_eq!(mounts.get(&1).unwrap().syncs, 0);
}
```

- [ ] Run `CARGO_TARGET_DIR=/Users/xaver/dev/qos/target/macos-shell python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo nextest run -p ui --test it --locked --profile ci -E 'test(wake_scope_host::)'`. Require discovery and execution of exactly four `wake_scope_host::` tests in binary `it`; zero matching tests is a failure. This is added conformance coverage over existing Task 2 production behavior; it should be GREEN immediately, not require a deliberately broken implementation. If it is not green, investigate the exact ownership/publication failure and repair Task 2 semantics before proceeding.
- [ ] Run `CARGO_TARGET_DIR=/Users/xaver/dev/qos/target/macos-shell cargo clippy -p ui --test it --locked -- -D warnings` and format the new test file. Use the same scoped test-only lint allowances already used by adjacent integration tests for assertions and small fixture arithmetic; do not suppress production lints.
- [ ] Commit both `ui/tests/it.rs` and `ui/tests/wake_scope_host.rs` as `test(ui): cover multi-host wake routing and mount replacement`. Controller verifies the same four tests and test-target clippy on Fedora at this SHA. Record that the fixture is framework conformance, not real app embedding qualification.

## Final review checklist and handoff evidence

- [ ] Exactly one production WakeHandle type exists; no global event-loop destination remains in local_state.
- [ ] All captured owners are established before factories; error/unwind closes original captures; transfer never substitutes owner; callback entry includes configuration, direct App methods, FreezeThaw, native lifecycle and root destruction.
- [ ] Real LocalState writes, task publications and request sends authorize against captured scope; revision/generation invariants remain; close never waits for an update; wake sinks run outside generation and route locks.
- [ ] Recipient filtering is used by the production App handler before window lookup; current pre-window events mark dirty; closed/foreign events do nothing; synthetic mount dispatch neither revives missing IDs nor wakes replacements.
- [ ] Every listed live constructor migrated in the Task 2 commit; all remaining prebuilt calls are intentional detached/headless/test usage; Copy-to-Clone consumers compile on their actual hosts.
- [ ] Native/Fedora evidence records SHA, lockfile, target triple, selected/discovered test counts, outcomes, and any independently reproduced baseline blockers. This plan's scoped gates do not claim the broader required workspace/provider/GPU matrix passed.
- [ ] Preserve headless Fedora and the remaining port work in the controller's next plan. Real Notes/Files component factories and per-tile scopes, full capability map, persistence/service contexts, collaboration, Mac presentation and native providers are outstanding. Do not claim “apps hosted on Mac” from these routing tests.

After any normal `git push`, wait for the task's automatic final CI notification. Investigate failures with `fgj actions`; keep relevant CI green before merging. This plan does not require a merge or upstream push. The controller owns authorized Fedora synchronization and verification. Task workers do not perform native GUI actions or claim the broader host port from these scoped checks.
