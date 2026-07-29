//! Presentation off the input thread.
//!
//! # The problem
//!
//! Everything used to happen on one thread: backend input polling,
//! widget dispatch, the CRDT pump, layout, scene building, GPU submit and
//! present. The submit is by far the largest phase — measured at 5.6 ms
//! p50 / 10.4 ms p99 on the headless backend, against 0.1 ms for building
//! the scene — and it is a *blocking* wait on the GPU (`device.poll`
//! in the winit backend, a full render + readback in the DRM one). While
//! it runs, no keystroke is read.
//!
//! # Where the seam has to go
//!
//! Not at the widget tree. `ui` widgets mutate the Automerge document
//! directly inside `handle_event`, and the draw pass reads that same
//! document, so the document cannot be shared across threads without a
//! lock held for the whole frame — which would reintroduce the stall it
//! was meant to remove.
//!
//! The seam goes one step later, at the finished frame. `paint()` returns
//! an owned [`vello::Scene`]: a self-contained encoding of the frame with
//! no borrow of the document, the widget tree, or the text renderer. That
//! is what crosses the thread boundary. The logic thread keeps sole
//! ownership of the CRDT and the widget tree exactly as before; the
//! render thread only ever sees finished scenes.
//!
//! ```text
//!   logic thread                       render thread
//!   ─────────────────────────────      ────────────────────────────
//!   input → widgets → AutoCommit
//!   layout
//!   paint() ──► Scene ─── channel ───► Presenter::present
//!   (next frame's input …)                GPU submit / page flip
//! ```
//!
//! # Depth, and the latency it buys or costs
//!
//! [`PresentPipe`] admits [`QUEUE_DEPTH`] frames beyond the one being
//! presented. At depth 1 that is two CPU-side frames in flight: the
//! render thread works on frame N while the logic thread builds N+1.
//!
//! When the pipe is full the logic thread **does not build a frame at
//! all** — [`PresentPipe::has_capacity`] returns false and the paint gate
//! declines. The alternative (build it anyway and discard the older
//! queued frame) would waste the build and, worse, decouple the queue
//! length from the display: a compositor that keeps accepting frames it
//! cannot present grows latency without bound while looking busy. Refusing
//! to build caps the input-to-photon path at two frame intervals, and the
//! refused tick still services input and protocol traffic — the loop stays
//! responsive, it just doesn't render a frame nobody could see.
//!
//! Deeper queues trade latency for throughput. Two frames is the shallowest
//! depth that lets CPU and GPU overlap at all, and a compositor is a
//! latency-first workload: a desktop that renders 200 fps at four frames
//! of lag feels worse than one that renders 144 fps at two.
//!
//! # If a presenter panics
//!
//! The render thread unwinds, its `finished` flag is set on the way out,
//! and the channel's receiver drops. The next [`PresentPipe::submit`]
//! sees a closed channel, returns `false`, and the loop logs and reverts
//! to `present_pipe = None` — presentation stops, but the compositor
//! keeps serving its clients and holding its protocol state. Losing the
//! display is bad; taking the session (and every client's unsaved work)
//! with it would be worse.
//!
//! Note that neither production presenter can panic on a poisoned lock,
//! because neither takes a lock at all: [`super::backend::winit::WinitPresenter`]
//! makes only wgpu calls, and `DrmPresenter` only touches scanouts it
//! solely owns. The `Arc<Mutex<RenderContext>>` in `ui::offscreen` — the
//! one shared GPU lock in the process — is reached only by the headless
//! backend, which deliberately has no presenter. The scene arriving here
//! is already built, so nothing on this thread reaches back into the
//! frame builder, the CRDT, or the foreign-surface store.
//!
//! # What stays synchronous
//!
//! The headless backend. Its `submit_frame` renders into an offscreen
//! texture whose bytes the test-control `snapshot` command reads back
//! immediately afterwards; making it asynchronous would make every
//! headless harness racy. It returns `None` from
//! [`super::backend::Backend::take_presenter`] and the loop keeps calling
//! `submit_frame` inline. This is deliberate: the headless path must not
//! acquire a threading model it doesn't need.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use vello::Scene;

use super::backend::OutputId;

/// Frames accepted beyond the one currently being presented.
pub const QUEUE_DEPTH: usize = 1;

/// The half of a backend that turns a finished scene into photons.
///
/// Split off a [`super::backend::Backend`] by
/// [`super::backend::Backend::take_presenter`] and moved onto the render
/// thread, so `Send` is the whole point of the trait. Everything that is
/// *not* `Send` — a winit `EventLoop`, a `GlesRenderer`, the libinput
/// context — stays behind on the backend.
pub trait Presenter: Send {
    /// Render and present one frame.
    fn present(&mut self, scene: &Scene, output: OutputId);

    /// The output was resized. Default no-op for presenters with a fixed
    /// surface.
    fn resize(&mut self, _size: (u32, u32)) {}

    /// Power the display on / off. Routed through the same channel as
    /// frames so a blank can never overtake a frame already queued.
    fn set_dpms(&mut self, _on: bool) {}
}

enum PresentMsg {
    Frame { scene: Scene, output: OutputId },
    Resize((u32, u32)),
    Dpms(bool),
    Stop,
}

/// Signalled by the render thread each time a frame retires.
///
/// Boxed as a trait object so `present` doesn't depend on calloop: the
/// loop passes a closure that pings its event source, the unit tests
/// pass one that bumps a counter.
pub type RetireSignal = Box<dyn Fn() + Send>;

/// Handle to the render thread.
pub struct PresentPipe {
    tx: Sender<PresentMsg>,
    /// Frames handed over but not yet presented. The logic thread reads
    /// it to decide whether to build the next frame.
    inflight: Arc<AtomicUsize>,
    presented: Arc<AtomicU64>,
    /// Ticks on which the paint gate declined because the pipe was full.
    /// Surfaced in the paint stats — a large number means the GPU is the
    /// bottleneck, which is a different diagnosis from a slow scene build.
    backpressured: u64,
    /// Set by the render thread just before it returns. Read by
    /// [`Self::shutdown`] so it can tell "finished" from "wedged" without
    /// committing to a `join` that might never return.
    finished: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

/// How long [`PresentPipe::shutdown`] waits for the render thread before
/// giving up on it.
///
/// Generous enough to cover a frame in flight on any real display (a
/// 24 Hz mode is 42 ms) plus a slow driver's teardown, short enough that
/// a wedged GPU cannot hold the session open.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

impl PresentPipe {
    /// Move `presenter` onto a dedicated thread.
    ///
    /// The thread is named `qos-present` so it is identifiable in
    /// `perf`/`top` output and in a backtrace.
    ///
    /// `on_retire` is called on the render thread each time a frame
    /// finishes presenting. The loop uses it to wake its event loop: when
    /// the pipe is full the paint gate declines and the loop parks, and
    /// without a wakeup it would sleep out the whole frame interval even
    /// though capacity came back microseconds later. On a GPU-bound frame
    /// that halves throughput — measured as a 33.3 ms paint period (every
    /// other deadline) on a nested run whose render thread was keeping up
    /// far better than that.
    pub fn spawn(mut presenter: Box<dyn Presenter>, on_retire: RetireSignal) -> Self {
        let (tx, rx): (Sender<PresentMsg>, Receiver<PresentMsg>) = channel();
        let inflight = Arc::new(AtomicUsize::new(0));
        let presented = Arc::new(AtomicU64::new(0));
        let thread_inflight = inflight.clone();
        let thread_presented = presented.clone();
        let finished = Arc::new(AtomicBool::new(false));
        let thread_finished = finished.clone();
        let handle = std::thread::Builder::new()
            .name("qos-present".to_string())
            .spawn(move || {
                // Flag completion however this thread leaves — normal
                // return or unwind — so `shutdown` never waits on a
                // thread that is already gone.
                let _done = FinishedOnDrop(thread_finished);
                for msg in rx {
                    match msg {
                        PresentMsg::Frame { scene, output } => {
                            presenter.present(&scene, output);
                            thread_presented.fetch_add(1, Ordering::Release);
                            // Decrement *after* the present so the logic
                            // thread's capacity check reflects work
                            // actually finished, not work dequeued.
                            thread_inflight.fetch_sub(1, Ordering::Release);
                            // Then wake the loop — strictly after the
                            // decrement, so a loop that wakes on this
                            // signal is guaranteed to see the capacity.
                            on_retire();
                        }
                        PresentMsg::Resize(size) => presenter.resize(size),
                        PresentMsg::Dpms(on) => presenter.set_dpms(on),
                        PresentMsg::Stop => break,
                    }
                }
            })
            .expect("spawn qos-present thread");
        Self {
            tx,
            inflight,
            presented,
            backpressured: 0,
            finished,
            handle: Some(handle),
        }
    }

    /// Whether another frame may be built and handed over.
    pub fn has_capacity(&self) -> bool {
        self.inflight.load(Ordering::Acquire) <= QUEUE_DEPTH
    }

    /// Count a tick on which the paint gate declined for lack of capacity.
    pub fn note_backpressure(&mut self) {
        self.backpressured += 1;
    }

    pub fn backpressured(&self) -> u64 {
        self.backpressured
    }

    /// Frames the render thread has finished presenting.
    pub fn presented(&self) -> u64 {
        self.presented.load(Ordering::Acquire)
    }

    /// Frames handed over but not yet presented.
    pub fn inflight(&self) -> usize {
        self.inflight.load(Ordering::Acquire)
    }

    /// Hand a finished scene to the render thread. Never blocks.
    ///
    /// Returns `false` if the render thread has gone away, which is fatal
    /// for presentation but not for the compositor: the caller logs it
    /// and keeps serving clients rather than taking the session down.
    pub fn submit(&mut self, scene: Scene, output: OutputId) -> bool {
        self.inflight.fetch_add(1, Ordering::AcqRel);
        if self.tx.send(PresentMsg::Frame { scene, output }).is_err() {
            self.inflight.fetch_sub(1, Ordering::AcqRel);
            return false;
        }
        true
    }

    pub fn resize(&self, size: (u32, u32)) {
        let _ = self.tx.send(PresentMsg::Resize(size));
    }

    pub fn set_dpms(&self, on: bool) {
        let _ = self.tx.send(PresentMsg::Dpms(on));
    }

    /// Stop the render thread and wait for the frame in flight to finish.
    /// Called on shutdown so a half-presented frame doesn't outlive the
    /// device it was rendering into.
    /// Stop the render thread and wait — for at most [`SHUTDOWN_GRACE`] —
    /// for the frame in flight to finish.
    ///
    /// The wait is bounded because the thing being waited on is a GPU. A
    /// bare `join` here is a deadlock waiting for a driver to hang: the
    /// render thread is blocked inside `present`, `join` blocks the
    /// caller, and neither ever returns. That is not hypothetical — it is
    /// exactly how a deliberate-breakage run of the queue bound wedged,
    /// hanging for ten minutes instead of reporting a failure, because
    /// `Drop` ran `join` while the test still held the presenter blocked.
    ///
    /// If the thread doesn't finish in time we detach it and say so. A
    /// leaked thread on the way out of a process is a far better outcome
    /// than a compositor that will not exit.
    pub fn shutdown(&mut self) {
        self.shutdown_with_grace(SHUTDOWN_GRACE);
    }

    /// [`Self::shutdown`] with an explicit grace period.
    ///
    /// Exists so the test that deliberately wedges a presenter can use a
    /// short grace. With both on the same constant, "the `present` test
    /// module took exactly `SHUTDOWN_GRACE`" is ambiguous — the tests run
    /// in parallel, so one test on the timeout path and five on it look
    /// identical from the outside. Parameterising it means the clean path
    /// is the only thing that can produce a fast module, and any test
    /// that regresses onto the timeout path shows up as a two-second
    /// outlier against neighbours that finish instantly.
    pub fn shutdown_with_grace(&mut self, grace: Duration) {
        let Some(handle) = self.handle.take() else {
            return;
        };
        let _ = self.tx.send(PresentMsg::Stop);
        let deadline = Instant::now() + grace;
        while !self.finished.load(Ordering::Acquire) {
            if Instant::now() >= deadline {
                eprintln!(
                    "nirios compositor: render thread did not stop within {grace:?}; \
                     detaching it rather than blocking shutdown"
                );
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        // The thread has flagged completion, so this join returns
        // promptly. `Err` means it panicked, which its own handler
        // already reported.
        let _ = handle.join();
    }
}

/// Sets the shared "thread finished" flag on drop, so it is set whether
/// the render thread returns normally or unwinds.
struct FinishedOnDrop(Arc<AtomicBool>);

impl Drop for FinishedOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

impl Drop for PresentPipe {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::Duration;

    /// A presenter that blocks until released, so a test can hold a frame
    /// "on the GPU" and observe the pipe's back-pressure.
    struct Blocking {
        gate: Arc<Mutex<()>>,
        seen: Arc<AtomicU64>,
        resizes: Arc<Mutex<Vec<(u32, u32)>>>,
        dpms: Arc<Mutex<Vec<bool>>>,
    }

    impl Presenter for Blocking {
        // Every lock here recovers from poisoning rather than panicking.
        // A test that fails while holding the gate poisons it, and a
        // presenter that then panicked would print a second, unrelated
        // `PoisonError` backtrace underneath the real assertion failure —
        // a red herring in exactly the output someone is reading to
        // understand why the test failed. The state behind these locks is
        // a counter and two Vecs; there is no invariant for a poisoned
        // lock to be protecting.
        fn present(&mut self, _scene: &Scene, _output: OutputId) {
            let _held = self.gate.lock().unwrap_or_else(|e| e.into_inner());
            self.seen.fetch_add(1, Ordering::Release);
        }
        fn resize(&mut self, size: (u32, u32)) {
            self.resizes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(size);
        }
        fn set_dpms(&mut self, on: bool) {
            self.dpms
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(on);
        }
    }

    /// `PresentPipe::spawn` with a retire signal that does nothing —
    /// what most of these tests want, since they assert on the counters
    /// rather than the wakeup.
    fn spawn_with_retire(p: Box<dyn Presenter>) -> PresentPipe {
        PresentPipe::spawn(p, Box::new(|| {}))
    }

    // Note on declaration order in the gate-holding tests below: each
    // spawns the pipe *before* taking the gate, so the guard drops first
    // and the render thread is released before `shutdown` waits on it.
    // That ordering is not what makes them correct —
    // `PresentPipe::shutdown` is bounded, so the other order returns after
    // the grace period with a detach warning rather than hanging. The
    // ordering is here so these tests exercise the clean shutdown path
    // instead of paying `SHUTDOWN_GRACE` on every teardown, which would be
    // slow and would hide a regression in the fast path.

    fn wait_until(mut cond: impl FnMut() -> bool, what: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if cond() {
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("timed out waiting for {what}");
    }

    #[test]
    fn a_scene_can_cross_a_thread_boundary() {
        // The whole design rests on this: if `Scene` were not `Send` the
        // seam would have to move, so pin it as a compile-time fact
        // rather than an assumption in a comment.
        fn assert_send<T: Send>() {}
        assert_send::<Scene>();
        assert_send::<Box<dyn Presenter>>();
    }

    #[test]
    fn frames_are_presented_on_the_render_thread_not_the_caller() {
        let seen = Arc::new(AtomicU64::new(0));
        let mut pipe = spawn_with_retire(Box::new(Blocking {
            gate: Arc::new(Mutex::new(())),
            seen: seen.clone(),
            resizes: Arc::new(Mutex::new(Vec::new())),
            dpms: Arc::new(Mutex::new(Vec::new())),
        }));
        assert!(pipe.submit(Scene::new(), OutputId(0)));
        wait_until(|| pipe.presented() == 1, "the frame to be presented");
        assert_eq!(seen.load(Ordering::Acquire), 1);
        pipe.shutdown();
    }

    #[test]
    fn submit_does_not_block_on_a_slow_present() {
        // The point of the split. Hold the presenter inside `present`
        // and assert the submitting thread returns anyway — if `submit`
        // blocked, this call would not return until the gate is released.
        let gate = Arc::new(Mutex::new(()));
        // Spawn *before* taking the gate: locals drop in reverse
        // declaration order, so `held` must be declared after `pipe` to be
        // released before `shutdown` waits on the render thread. The other
        // order deadlocks on any assertion failure.
        let mut pipe = spawn_with_retire(Box::new(Blocking {
            gate: gate.clone(),
            seen: Arc::new(AtomicU64::new(0)),
            resizes: Arc::new(Mutex::new(Vec::new())),
            dpms: Arc::new(Mutex::new(Vec::new())),
        }));
        let held = gate.lock().expect("hold the presenter inside `present`");
        let t0 = std::time::Instant::now();
        assert!(pipe.submit(Scene::new(), OutputId(0)));
        let elapsed = t0.elapsed();
        assert!(
            elapsed < Duration::from_millis(100),
            "submit must hand off, not wait for the present; took {elapsed:?}"
        );
        assert_eq!(pipe.presented(), 0, "the present is still blocked");
        drop(held);
        wait_until(|| pipe.presented() == 1, "the released frame");
        pipe.shutdown();
    }

    #[test]
    fn the_pipe_refuses_more_than_queue_depth_frames_beyond_the_one_in_flight() {
        // Back-pressure is what bounds latency. With the presenter stuck,
        // capacity must run out — otherwise the logic thread would build
        // frames forever and the input-to-photon path would grow without
        // bound.
        let gate = Arc::new(Mutex::new(()));
        // Spawn *before* taking the gate: locals drop in reverse
        // declaration order, so `held` must be declared after `pipe` to be
        // released before `shutdown` waits on the render thread. The other
        // order deadlocks on any assertion failure.
        let mut pipe = spawn_with_retire(Box::new(Blocking {
            gate: gate.clone(),
            seen: Arc::new(AtomicU64::new(0)),
            resizes: Arc::new(Mutex::new(Vec::new())),
            dpms: Arc::new(Mutex::new(Vec::new())),
        }));
        let held = gate.lock().expect("hold the presenter inside `present`");
        let mut accepted = 0;
        for _ in 0..10 {
            if !pipe.has_capacity() {
                pipe.note_backpressure();
                continue;
            }
            assert!(pipe.submit(Scene::new(), OutputId(0)));
            accepted += 1;
        }
        assert_eq!(
            accepted,
            QUEUE_DEPTH + 1,
            "the pipe must admit exactly one frame beyond the queue depth \
             while the presenter is stuck; admitted {accepted}"
        );
        assert!(
            pipe.backpressured() > 0,
            "the refused ticks must be counted so the stats can attribute them"
        );
        drop(held);
        wait_until(
            || pipe.presented() as usize == accepted,
            "the queued frames to drain",
        );
        assert!(
            pipe.has_capacity(),
            "capacity must come back once the queue drains"
        );
        pipe.shutdown();
    }

    #[test]
    fn resize_and_dpms_are_ordered_behind_queued_frames() {
        // A blank must never overtake a frame already handed over: the
        // display would light up showing the frame the compositor thought
        // it had suppressed. One channel gives that for free — this pins
        // it so a future "fast path" for control messages can't silently
        // break it.
        let resizes = Arc::new(Mutex::new(Vec::new()));
        let dpms = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::new(AtomicU64::new(0));
        let gate = Arc::new(Mutex::new(()));
        // See the note above about declaration order and `shutdown`.
        let mut pipe = spawn_with_retire(Box::new(Blocking {
            gate: gate.clone(),
            seen: seen.clone(),
            resizes: resizes.clone(),
            dpms: dpms.clone(),
        }));
        let held = gate.lock().expect("hold the presenter inside `present`");
        pipe.submit(Scene::new(), OutputId(0));
        pipe.resize((800, 600));
        pipe.set_dpms(false);
        // Nothing may have run yet: the frame is blocking the thread.
        assert!(resizes.lock().expect("resizes").is_empty());
        assert!(dpms.lock().expect("dpms").is_empty());
        drop(held);
        wait_until(
            || dpms.lock().expect("dpms").len() == 1,
            "the dpms message to be handled after the frame",
        );
        assert_eq!(seen.load(Ordering::Acquire), 1, "the frame ran first");
        assert_eq!(*resizes.lock().expect("resizes"), vec![(800, 600)]);
        assert_eq!(*dpms.lock().expect("dpms"), vec![false]);
        pipe.shutdown();
    }

    #[test]
    fn the_loop_is_woken_after_the_capacity_is_restored_not_before() {
        // The wakeup exists so a back-pressured loop doesn't park out the
        // whole frame interval. It is only useful if, by the time the
        // loop wakes, `has_capacity` is already true — a signal sent
        // before the in-flight count drops would wake the loop into
        // exactly the same "no room" state it just parked from, and the
        // frame would still wait a full interval.
        let wakeups = Arc::new(AtomicU64::new(0));
        let counter = wakeups.clone();
        let gate = Arc::new(Mutex::new(()));
        // See the note above about declaration order and `shutdown`.
        let mut pipe = PresentPipe::spawn(
            Box::new(Blocking {
                gate: gate.clone(),
                seen: Arc::new(AtomicU64::new(0)),
                resizes: Arc::new(Mutex::new(Vec::new())),
                dpms: Arc::new(Mutex::new(Vec::new())),
            }),
            Box::new(move || {
                counter.fetch_add(1, Ordering::Release);
            }),
        );
        let held = gate.lock().expect("hold the presenter inside `present`");
        // Fill the pipe while the presenter is stuck. Bounded rather than
        // `while has_capacity()`: a regression that made capacity
        // unconditional would turn an unbounded fill into a hang, and a
        // hung test reports nothing.
        let mut accepted = 0;
        for _ in 0..8 {
            if !pipe.has_capacity() {
                break;
            }
            assert!(pipe.submit(Scene::new(), OutputId(0)));
            accepted += 1;
        }
        assert_eq!(
            accepted,
            QUEUE_DEPTH + 1,
            "the stuck presenter must stop admitting frames after the queue fills"
        );
        assert_eq!(
            wakeups.load(Ordering::Acquire),
            0,
            "nothing has retired, so the loop must not have been woken — a \
             spurious wakeup would spin it against a still-full pipe"
        );
        drop(held);
        wait_until(
            || wakeups.load(Ordering::Acquire) as usize == accepted,
            "one wakeup per retired frame",
        );
        assert!(
            pipe.has_capacity(),
            "the decrement happens-before the signal, so a loop woken by it \
             must find room; signalling first would wake it into the same \
             full pipe it just parked from"
        );
        pipe.shutdown();
    }

    #[test]
    fn shutdown_returns_even_when_the_presenter_never_finishes() {
        // The failure this guards against is not hypothetical: a
        // deliberate-breakage run of the queue bound hung for ten minutes
        // instead of reporting a failed assertion, because `Drop` ran a
        // bare `join` on a render thread blocked inside `present`. The
        // same shape is a compositor that will not exit when its GPU
        // driver wedges.
        //
        // Hold the presenter forever and assert `shutdown` still returns,
        // within a bound comfortably above SHUTDOWN_GRACE.
        let gate = Arc::new(Mutex::new(()));
        let mut pipe = spawn_with_retire(Box::new(Blocking {
            gate: gate.clone(),
            seen: Arc::new(AtomicU64::new(0)),
            resizes: Arc::new(Mutex::new(Vec::new())),
            dpms: Arc::new(Mutex::new(Vec::new())),
        }));
        let held = gate.lock().expect("wedge the presenter");
        assert!(pipe.submit(Scene::new(), OutputId(0)));
        // The channel is FIFO, so the render thread must take this frame
        // — and block on the gate inside `present` — before it can ever
        // see the `Stop` that `shutdown` is about to send. It is therefore
        // guaranteed to be wedged, not merely likely to be.
        // A short grace, so this test cannot be confused with a
        // regression: it finishes in a tenth of a second, while anything
        // that fell back onto the real `SHUTDOWN_GRACE` path stands out as
        // a two-second outlier.
        let grace = Duration::from_millis(100);
        let t0 = Instant::now();
        pipe.shutdown_with_grace(grace);
        let took = t0.elapsed();
        assert!(
            took >= grace,
            "shutdown must actually wait out its grace before giving up; \
             returning in {took:?} means it never waited for the frame at all, \
             which would abandon a frame that was about to finish"
        );
        assert!(
            took < Duration::from_millis(1_500),
            "shutdown must give up on a wedged render thread, not block on it; \
             took {took:?} against a {grace:?} grace"
        );
        // Releasing afterwards must not panic or double-free anything.
        drop(held);
    }

    #[test]
    fn submit_after_shutdown_reports_failure_instead_of_panicking() {
        // Presentation dying must not take the session with it: clients
        // stay served, the operator gets a log line.
        let mut pipe = spawn_with_retire(Box::new(Blocking {
            gate: Arc::new(Mutex::new(())),
            seen: Arc::new(AtomicU64::new(0)),
            resizes: Arc::new(Mutex::new(Vec::new())),
            dpms: Arc::new(Mutex::new(Vec::new())),
        }));
        pipe.shutdown();
        assert!(
            !pipe.submit(Scene::new(), OutputId(0)),
            "a submit into a stopped pipe must report failure, not panic"
        );
        assert_eq!(
            pipe.inflight(),
            0,
            "a failed submit must not leave a phantom frame in flight, which \
             would wedge the capacity check forever"
        );
    }
}
