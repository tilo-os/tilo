//! Presentation off the input thread.
//!
//! Blocking GPU submit (measured 5.6 ms p50 / 10.4 ms p99 on headless, against 0.1 ms to build
//! a scene) used to share a thread with input, stalling every keystroke. The seam is the
//! finished [`vello::Scene`], not the widget tree — `ui` widgets mutate the Automerge document
//! directly, so sharing it across threads would need a frame-long lock and reintroduce the stall.
//!
//! [`PresentPipe`] admits [`QUEUE_DEPTH`] frames beyond the one presenting; when full, the logic
//! thread refuses to build a frame rather than discard a queued one, which would decouple queue
//! length from the display and grow latency without bound while looking busy.
//!
//! A presenter panic unwinds the render thread; the next [`PresentPipe::submit`] then sees a
//! closed channel and drops to no presenter, so a lost display doesn't take the session (and
//! clients' unsaved work) with it. The headless backend stays synchronous, since its `submit_frame` output is read back immediately by the test-control `snapshot` command.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use vello::Scene;

use super::frame::FramePaint;
use super::render::OutputId;

/// Frames accepted beyond the one currently being presented.
pub const QUEUE_DEPTH: usize = 1;

/// Normal refresh-rate frames stay silent. Anomalous lifecycle records flow
/// through the session runner's bounded, periodically-synced flight log, so a
/// hard reboot does not erase the last completed diagnostic boundary.
const SLOW_FRAME_LIFECYCLE: Duration = Duration::from_millis(100);

/// The half of a backend that turns a finished scene into photons.
///
/// Split off a `super::backend::Backend` by `super::backend::Backend::take_presenter` and
/// moved onto the render thread, so `Send` is the whole point of the trait — everything not `Send` (a winit `EventLoop`, a `GlesRenderer`, the libinput context) stays on the backend.
pub trait Presenter: Send {
    /// Render and present one frame.
    fn present(&mut self, scene: &Scene, output: OutputId);

    /// Versioned retained damage when the producer has an authoritative relation to its prior
    /// scene. Default presenters remain dense and therefore compatible with plain scenes.
    fn present_frame(&mut self, frame: &FramePaint, output: OutputId) {
        self.present(&frame.scene, output);
    }

    /// Build the renderer before the first frame arrives. Called once on the render thread as it
    /// starts, so device setup and shader compilation overlap the compositor's own startup
    /// instead of delaying the first frame.
    fn prepare(&mut self) {}

    /// The output was resized; default no-op for presenters with a fixed surface.
    fn resize(&mut self, _size: (u32, u32)) {}

    /// Power the display on/off, routed through the same channel as frames so a blank can never overtake a frame already queued.
    fn set_dpms(&mut self, _on: bool) {}
}

enum PresentMsg {
    // Boxed so the message is no bigger than its next-largest variant — a `Scene` inline here
    // would size every `Resize`/`Dpms`/`Stop` message (and the channel's internal buffer slot)
    // to match it.
    Frame {
        frame: Box<FramePaint>,
        output: OutputId,
        /// Assigned by the logic thread at submit time. It lets a caller wait for one
        /// *specific* frame rather than treating any frame that happened to retire afterwards
        /// as an acknowledgement. That distinction is security-critical for the pre-sleep lock
        /// frame: a desktop frame already queued before the lock must not release the logind
        /// delay inhibitor.
        serial: u64,
        enqueued_at: Instant,
        input_age_at_enqueue: Option<Duration>,
        inflight_at_enqueue: usize,
    },
    Resize((u32, u32)),
    Dpms(bool),
    Stop,
}

/// Signalled by the render thread each time a frame retires. Boxed as a trait object so
/// `present` doesn't depend on calloop: the loop passes a closure that pings its event source, tests pass one that bumps a counter.
pub type RetireSignal = Box<dyn Fn() + Send>;

/// Handle to the render thread.
pub struct PresentPipe {
    tx: Sender<PresentMsg>,
    /// Frames handed over but not yet presented; the logic thread reads it to decide whether to build the next frame.
    inflight: Arc<AtomicUsize>,
    /// Serial of the newest frame the render thread has finished handling.
    /// With the FIFO channel this also equals the number of retired frames,
    /// but the identity is what callers need for a reliable acknowledgement.
    presented: Arc<AtomicU64>,
    /// Last serial successfully handed to the render thread.  This is kept
    /// separately from `inflight`: the latter is a bounded queue depth, not
    /// an identity that survives a completed presentation.
    submitted: Arc<AtomicU64>,
    /// Monotonic completion time of the newest frame the presenter returned
    /// from. The timestamp and serial are read under one lock so metrics never
    /// associate an old time with a newer frame.
    last_retirement: Arc<Mutex<Option<(u64, Instant)>>>,
    /// Ticks on which the paint gate declined because the pipe was full. Surfaced in the paint stats — a large number means the GPU is the bottleneck, a different diagnosis from a slow scene build.
    backpressured: u64,
    /// Set by the render thread just before it returns; lets [`Self::shutdown`] tell "finished" from "wedged" without committing to a `join` that might never return.
    finished: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

/// How long [`PresentPipe::shutdown`] waits for the render thread before giving up on it.
///
/// Generous enough to cover a frame in flight on any real display (a 24 Hz mode is 42 ms) plus a slow driver's teardown, short enough that a wedged GPU cannot hold the session open.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

impl PresentPipe {
    /// Move `presenter` onto a dedicated thread named `tilo-present` (identifiable in `perf`/`top` output and backtraces).
    ///
    /// `on_retire` fires on the render thread each time a frame finishes presenting, so the loop can
    /// wake up — without it, a parked loop sleeps out the whole frame interval even though capacity
    /// came back microseconds later, measured as a 33.3 ms paint period (every other deadline) on a render thread keeping up far better than that.
    pub fn spawn(mut presenter: Box<dyn Presenter>, on_retire: RetireSignal) -> Self {
        let (tx, rx): (Sender<PresentMsg>, Receiver<PresentMsg>) = channel();
        let inflight = Arc::new(AtomicUsize::new(0));
        let presented_frames = Arc::new(AtomicU64::new(0));
        let submitted_frames = Arc::new(AtomicU64::new(0));
        let last_retirement = Arc::new(Mutex::new(None));
        let thread_inflight = inflight.clone();
        let thread_presented = presented_frames.clone();
        let thread_last_retirement = last_retirement.clone();
        let finished = Arc::new(AtomicBool::new(false));
        let thread_finished = finished.clone();
        let spawned = std::thread::Builder::new()
            .name("tilo-present".to_string())
            .spawn(move || {
                // Flag completion however this thread leaves (normal return or unwind), so `shutdown` never waits on a thread that's already gone.
                let _done = FinishedOnDrop(thread_finished);
                presenter.prepare();
                for msg in rx {
                    match msg {
                        PresentMsg::Frame {
                            frame,
                            output,
                            serial,
                            enqueued_at,
                            input_age_at_enqueue,
                            inflight_at_enqueue,
                        } => {
                            let render_started = Instant::now();
                            let queue_wait = render_started.saturating_duration_since(enqueued_at);
                            presenter.present_frame(&frame, output);
                            let render_done = Instant::now();
                            let render = render_done.saturating_duration_since(render_started);
                            let total = render_done.saturating_duration_since(enqueued_at);
                            // Keep the first completed presentation observable even when
                            // it is fast and the event loop then parks indefinitely.
                            if serial == 1
                                || queue_wait >= SLOW_FRAME_LIFECYCLE
                                || render >= SLOW_FRAME_LIFECYCLE
                                || total >= SLOW_FRAME_LIFECYCLE
                            {
                                eprintln!(
                                    "FRAME-LIFECYCLE serial={serial} event=render-retired queue_wait_us={} render_us={} total_us={} inflight_at_enqueue={inflight_at_enqueue} input_age_at_enqueue_us={}",
                                    queue_wait.as_micros(),
                                    render.as_micros(),
                                    total.as_micros(),
                                    input_age_at_enqueue.map_or_else(
                                        || "none".to_owned(),
                                        |age| age.as_micros().to_string()
                                    ),
                                );
                            }
                            *thread_last_retirement
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                Some((serial, render_done));
                            thread_presented.store(serial, Ordering::Release);
                            // Decrement *after* the present so the logic thread's capacity check reflects work actually finished, not work dequeued.
                            thread_inflight.fetch_sub(1, Ordering::Release);
                            // Then wake the loop — strictly after the decrement, so a loop that wakes on this signal is guaranteed to see the capacity.
                            on_retire();
                        }
                        PresentMsg::Resize(size) => presenter.resize(size),
                        PresentMsg::Dpms(on) => presenter.set_dpms(on),
                        PresentMsg::Stop => break,
                    }
                }
            });
        // If the OS refuses to hand over a thread, `rx` (moved into the closure above) is
        // dropped right here along with the rest of the unspawned closure — so every future
        // `submit`/`resize`/`set_dpms` sees a channel with no receiver and reports failure,
        // exactly the "lost display" path this pipe already has to handle, rather than a panic
        // that would take the whole session down with it.
        let handle = match spawned {
            Ok(handle) => Some(handle),
            Err(e) => {
                tracing::error!(
                    target: "shell::compositor::present",
                    error = %e,
                    "could not start the tilo-present render thread; presentation stays unavailable"
                );
                finished.store(true, Ordering::Release);
                None
            }
        };
        Self {
            tx,
            inflight,
            presented: presented_frames,
            submitted: submitted_frames,
            last_retirement,
            backpressured: 0,
            finished,
            handle,
        }
    }

    /// Whether another frame may be built and handed over.
    pub fn has_capacity(&self) -> bool {
        self.inflight.load(Ordering::Acquire) <= QUEUE_DEPTH
    }

    /// Count a tick on which the paint gate declined for lack of capacity.
    pub fn note_backpressure(&mut self) {
        self.backpressured = self.backpressured.saturating_add(1);
        let count = self.backpressured;
        // First refusal plus powers of two reconstruct a sustained render
        // wedge without emitting at the display refresh rate.
        if count == 1 || count.is_power_of_two() {
            eprintln!(
                "FRAME-LIFECYCLE event=backpressure count={count} inflight={} submitted={} retired={}",
                self.inflight(),
                self.submitted(),
                self.presented(),
            );
        }
    }

    pub fn backpressured(&self) -> u64 {
        self.backpressured
    }

    /// Frames the render thread has finished presenting.
    pub fn presented(&self) -> u64 {
        self.presented.load(Ordering::Acquire)
    }

    /// Serial of the latest frame accepted by the render queue.  A caller
    /// that submits a security-sensitive scene can retain this value and
    /// wait until [`Self::presented`] reaches it; a prior queued scene cannot
    /// accidentally satisfy that wait.
    pub fn submitted(&self) -> u64 {
        self.submitted.load(Ordering::Acquire)
    }

    /// Newest presenter-return boundary with its monotonic timestamp.
    ///
    /// This is deliberately called a retirement rather than a physical
    /// display time: each backend defines when `present_frame` returns. The
    /// DRM direct path separately tightens that boundary to a page-flip event.
    pub fn latest_retirement(&self) -> Option<(u64, Instant)> {
        *self
            .last_retirement
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether the render thread has exited, normally or through a panic.
    /// A caller waiting for a specific frame must distinguish this from a
    /// merely slow presentation: no later retirement can satisfy its wait.
    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }

    /// Frames handed over but not yet presented.
    pub fn inflight(&self) -> usize {
        self.inflight.load(Ordering::Acquire)
    }

    /// Hand a dense scene to the render thread. Never blocks. Returns `false` if the render
    /// thread has gone away — fatal for presentation but not the compositor: the caller logs it and keeps serving clients.
    pub fn submit(&mut self, scene: Scene, output: OutputId) -> bool {
        self.submit_frame(FramePaint::full(scene), output).is_some()
    }

    /// Hand a finished scene with retained-damage authority to the render thread.
    pub fn submit_frame(&mut self, frame: FramePaint, output: OutputId) -> Option<u64> {
        self.submit_frame_with_input_age(frame, output, None)
    }

    /// Submit while preserving the age of the oldest input represented by
    /// this frame. This is diagnostic metadata only; presentation semantics
    /// and FIFO ordering are unchanged.
    pub fn submit_frame_with_input_age(
        &mut self,
        frame: FramePaint,
        output: OutputId,
        input_age_at_enqueue: Option<Duration>,
    ) -> Option<u64> {
        // Serial zero is the unambiguous "no frame" value used by callers
        // before their first successful submit.  A closed pipe may consume a
        // number, but its caller immediately takes the lost-presenter path,
        // so no acknowledgement can observe that gap.
        let serial = self
            .submitted
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        let inflight_at_enqueue = self
            .inflight
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        let enqueued_at = Instant::now();
        if self
            .tx
            .send(PresentMsg::Frame {
                frame: Box::new(frame),
                output,
                serial,
                enqueued_at,
                input_age_at_enqueue,
                inflight_at_enqueue,
            })
            .is_err()
        {
            self.inflight.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(serial)
    }

    pub fn resize(&self, size: (u32, u32)) {
        let _ = self.tx.send(PresentMsg::Resize(size));
    }

    pub fn set_dpms(&self, on: bool) {
        let _ = self.tx.send(PresentMsg::Dpms(on));
    }

    /// Stop the render thread and wait — for at most [`SHUTDOWN_GRACE`] — for the frame in flight to finish.
    ///
    /// Bounded because the thing being waited on is a GPU: a bare `join` would deadlock if a driver
    /// hangs, since the render thread stays blocked inside `present` forever. If the thread doesn't finish in time, detach it instead — a leaked thread on exit beats a compositor that won't exit.
    pub fn shutdown(&mut self) {
        self.shutdown_with_grace(SHUTDOWN_GRACE);
    }

    /// [`Self::shutdown`] with an explicit grace period, so a test that deliberately wedges a
    /// presenter can use a short grace instead of [`SHUTDOWN_GRACE`] — otherwise a regression onto the timeout path is indistinguishable, from outside, from a clean run.
    pub fn shutdown_with_grace(&mut self, grace: Duration) {
        let Some(handle) = self.handle.take() else {
            return;
        };
        let _ = self.tx.send(PresentMsg::Stop);
        // An unrepresentable deadline degrades to "already due" — the loop below then gives up
        // on its very first check rather than panicking on the addition.
        let deadline = Instant::now()
            .checked_add(grace)
            .unwrap_or_else(Instant::now);
        while !self.finished.load(Ordering::Acquire) {
            if Instant::now() >= deadline {
                eprintln!(
                    "tilo compositor: render thread did not stop within {grace:?}; \
                     detaching it rather than blocking shutdown"
                );
                return;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        // The thread has flagged completion, so this join returns promptly; `Err` means it panicked, already reported by its own handler.
        let _ = handle.join();
    }
}

/// Sets the shared "thread finished" flag on drop, so it's set whether the render thread returns normally or unwinds.
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

    /// A presenter that blocks until released, so a test can hold a frame "on the GPU" and observe the pipe's back-pressure.
    struct Blocking {
        gate: Arc<Mutex<()>>,
        seen: Arc<AtomicU64>,
        resizes: Arc<Mutex<Vec<(u32, u32)>>>,
        dpms: Arc<Mutex<Vec<bool>>>,
    }

    struct PanickingPresenter;

    impl Presenter for PanickingPresenter {
        fn present(&mut self, _scene: &Scene, _output: OutputId) {
            panic!("intentional presenter failure");
        }
    }

    impl Presenter for Blocking {
        // Every lock here recovers from poisoning rather than panicking — a failing test that poisons the gate shouldn't also print an unrelated `PoisonError` backtrace over the real assertion failure.
        fn present(&mut self, _scene: &Scene, _output: OutputId) {
            let _held = self
                .gate
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.seen.fetch_add(1, Ordering::Release);
        }
        fn resize(&mut self, size: (u32, u32)) {
            self.resizes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(size);
        }
        fn set_dpms(&mut self, on: bool) {
            self.dpms
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(on);
        }
    }

    /// `PresentPipe::spawn` with a retire signal that does nothing — what most of these tests want, since they assert on the counters rather than the wakeup.
    fn spawn_with_retire(p: Box<dyn Presenter>) -> PresentPipe {
        PresentPipe::spawn(p, Box::new(|| {}))
    }

    // Declaration order in the gate-holding tests below: each spawns the pipe *before* taking the
    // gate, so locals drop in reverse order and the render thread is released before `shutdown`
    // waits on it — the other order still works but pays `SHUTDOWN_GRACE` on every teardown instead of exercising the clean path.

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn frames_present_asynchronously_on_the_render_thread() {
        fn a_scene_can_cross_a_thread_boundary() {
            // The whole design rests on this: if `Scene` were not `Send` the seam would have to move — pin it as a compile-time fact rather than an assumption in a comment.
            fn assert_send<T: Send>() {}
            assert_send::<Scene>();
            assert_send::<Box<dyn Presenter>>();
        }

        fn frames_are_presented_on_the_render_thread_not_the_caller() {
            let seen = Arc::new(AtomicU64::new(0));
            let mut pipe = spawn_with_retire(Box::new(Blocking {
                gate: Arc::new(Mutex::new(())),
                seen: seen.clone(),
                resizes: Arc::new(Mutex::new(Vec::new())),
                dpms: Arc::new(Mutex::new(Vec::new())),
            }));
            assert!(pipe.submit(Scene::new(), OutputId(0)));
            tilo_test_env::wait_until("the frame to be presented", || pipe.presented() == 1);
            assert_eq!(seen.load(Ordering::Acquire), 1);
            pipe.shutdown();
        }

        fn submit_does_not_block_on_a_slow_present() {
            // The point of the split: hold the presenter inside `present` and assert the submitting thread returns anyway — if `submit` blocked, this call wouldn't return until the gate released.
            let gate = Arc::new(Mutex::new(()));
            // See the declaration-order note above.
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
            tilo_test_env::wait_until("the released frame", || pipe.presented() == 1);
            pipe.shutdown();
        }

        fn input_age_metadata_does_not_change_async_submission() {
            let seen = Arc::new(AtomicU64::new(0));
            let mut pipe = spawn_with_retire(Box::new(Blocking {
                gate: Arc::new(Mutex::new(())),
                seen: seen.clone(),
                resizes: Arc::new(Mutex::new(Vec::new())),
                dpms: Arc::new(Mutex::new(Vec::new())),
            }));
            assert!(
                pipe.submit_frame_with_input_age(
                    FramePaint::full(Scene::new()),
                    OutputId(0),
                    Some(Duration::from_secs(2)),
                )
                .is_some()
            );
            tilo_test_env::wait_until("the annotated frame to retire", || pipe.presented() == 1);
            assert_eq!(seen.load(Ordering::Acquire), 1);
            pipe.shutdown();
        }

        fn submitted_serial_distinguishes_the_requested_frame_from_an_older_retirement() {
            // The sleep path must wait for the lock frame it just handed over,
            // not merely observe that *a* frame retired.  Model a desktop frame
            // that is already complete, then hold the lock frame in the queue:
            // its serial remains distinguishable until that exact frame runs.
            let gate = Arc::new(Mutex::new(()));
            let mut pipe = spawn_with_retire(Box::new(Blocking {
                gate: gate.clone(),
                seen: Arc::new(AtomicU64::new(0)),
                resizes: Arc::new(Mutex::new(Vec::new())),
                dpms: Arc::new(Mutex::new(Vec::new())),
            }));

            assert!(pipe.submit(Scene::new(), OutputId(0)));
            tilo_test_env::wait_until("the older desktop frame", || pipe.presented() == 1);

            let held = gate.lock().expect("hold the lock frame in presentation");
            assert!(pipe.submit(Scene::new(), OutputId(0)));
            let lock_frame = pipe.submitted();
            assert_eq!(lock_frame, 2);
            assert_eq!(
                pipe.presented(),
                1,
                "retiring the older desktop frame must not acknowledge lock frame {lock_frame}"
            );
            drop(held);
            tilo_test_env::wait_until("the specific lock frame to retire", || {
                pipe.presented() >= lock_frame
            });
            pipe.shutdown();
        }

        fn presenter_failure_is_observable_while_a_frame_is_unretired() {
            // A security-sensitive caller waits for a particular serial.  It
            // must be able to tell "the GPU is slow" from "the render worker is
            // gone"; otherwise a sleep-delay lock could wait forever with an
            // in-flight count that will never drain.
            let mut pipe = spawn_with_retire(Box::new(PanickingPresenter));
            assert!(pipe.submit(Scene::new(), OutputId(0)));
            let serial = pipe.submitted();
            tilo_test_env::wait_until("the presenter panic to terminate its thread", || {
                pipe.is_finished()
            });
            assert!(pipe.is_finished());
            assert!(
                pipe.presented() < serial,
                "a failed presenter must not claim it retired frame {serial}"
            );
            pipe.shutdown();
        }

        a_scene_can_cross_a_thread_boundary();
        frames_are_presented_on_the_render_thread_not_the_caller();
        submit_does_not_block_on_a_slow_present();
        input_age_metadata_does_not_change_async_submission();
        submitted_serial_distinguishes_the_requested_frame_from_an_older_retirement();
        presenter_failure_is_observable_while_a_frame_is_unretired();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn present_pipe_bounds_queue_depth_and_orders_control_behind_frames() {
        fn the_pipe_refuses_more_than_queue_depth_frames_beyond_the_one_in_flight() {
            // Back-pressure is what bounds latency. With the presenter stuck, capacity must run out — otherwise the logic thread would build frames forever and input-to-photon latency would grow unbounded.
            let gate = Arc::new(Mutex::new(()));
            // See the declaration-order note above.
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
            tilo_test_env::wait_until("the queued frames to drain", || {
                usize::try_from(pipe.presented()).is_ok_and(|presented| presented == accepted)
            });
            assert!(
                pipe.has_capacity(),
                "capacity must come back once the queue drains"
            );
            pipe.shutdown();
        }

        fn resize_and_dpms_are_ordered_behind_queued_frames() {
            // A blank must never overtake a frame already handed over: the display would light up showing
            // a frame the compositor thought it had suppressed. One channel gives that for free — this pins it so a future "fast path" for control messages can't silently break it.
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
            tilo_test_env::wait_until("the dpms message to be handled after the frame", || {
                dpms.lock().expect("dpms").len() == 1
            });
            assert_eq!(seen.load(Ordering::Acquire), 1, "the frame ran first");
            assert_eq!(*resizes.lock().expect("resizes"), vec![(800, 600)]);
            assert_eq!(*dpms.lock().expect("dpms"), vec![false]);
            pipe.shutdown();
        }

        fn the_loop_is_woken_after_the_capacity_is_restored_not_before() {
            // The wakeup exists so a back-pressured loop doesn't park out the whole frame interval — only
            // useful if, by the time the loop wakes, `has_capacity` is already true. A signal sent before
            // the in-flight count drops would wake the loop into the same "no room" state, and the frame would still wait a full interval.
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
            // Fill the pipe while the presenter is stuck. Bounded rather than `while has_capacity()`: a regression making capacity unconditional would turn an unbounded fill into a hang that reports nothing.
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
            tilo_test_env::wait_until("one wakeup per retired frame", || {
                usize::try_from(wakeups.load(Ordering::Acquire))
                    .is_ok_and(|wakeups| wakeups == accepted)
            });
            assert!(
                pipe.has_capacity(),
                "the decrement happens-before the signal, so a loop woken by it \
                 must find room; signalling first would wake it into the same \
                 full pipe it just parked from"
            );
            pipe.shutdown();
        }

        the_pipe_refuses_more_than_queue_depth_frames_beyond_the_one_in_flight();
        resize_and_dpms_are_ordered_behind_queued_frames();
        the_loop_is_woken_after_the_capacity_is_restored_not_before();
    }

    #[test]
    fn present_shutdown_returns_and_later_submits_fail_cleanly() {
        fn shutdown_returns_even_when_the_presenter_never_finishes() {
            // Guards against a compositor that won't exit when its GPU driver wedges: hold the presenter forever and assert `shutdown` still returns, within a bound comfortably above SHUTDOWN_GRACE.
            let gate = Arc::new(Mutex::new(()));
            let mut pipe = spawn_with_retire(Box::new(Blocking {
                gate: gate.clone(),
                seen: Arc::new(AtomicU64::new(0)),
                resizes: Arc::new(Mutex::new(Vec::new())),
                dpms: Arc::new(Mutex::new(Vec::new())),
            }));
            let held = gate.lock().expect("wedge the presenter");
            assert!(pipe.submit(Scene::new(), OutputId(0)));
            // The channel is FIFO, so the render thread must take this frame — and block on the gate
            // inside `present` — before it can see the `Stop` `shutdown` is about to send, so it's
            // guaranteed wedged. A short grace means this test can't be confused with a regression: it
            // finishes in a tenth of a second, while a fallback onto the real `SHUTDOWN_GRACE` stands out as a two-second outlier.
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

        fn submit_after_shutdown_reports_failure_instead_of_panicking() {
            // Presentation dying must not take the session with it: clients stay served, the operator gets a log line.
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

        shutdown_returns_even_when_the_presenter_never_finishes();
        submit_after_shutdown_reports_failure_instead_of_panicking();
    }
}
