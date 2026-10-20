//! Presentation off the input thread.
//!
//! Blocking GPU submit (measured 5.6 ms p50 / 10.4 ms p99 on headless, against 0.1 ms to build
//! a scene) used to share a thread with input, stalling every keystroke. The seam is the
//! finished [`vello::Scene`], not the widget tree — `ui` widgets mutate the Automerge document
//! directly, so sharing it across threads would need a frame-long lock and reintroduce the stall.
//!
//! [`PresentPipe`] admits [`Presenter::queue_depth`] frames beyond the one presenting
//! ([`QUEUE_DEPTH`] unless the presenter says otherwise); when full, the logic thread refuses to
//! build a frame rather than discard a queued one, which would decouple queue length from the
//! display and grow latency without bound while looking busy. A presenter that waits for every
//! page flip admits none: a frame queued behind one that is still waiting for its vblank would
//! start rendering only at that vblank and reach the screen an interval later.
//!
//! A presenter can also return before its frame's page flip retires
//! ([`Presenter::overlaps_presentation`]): the frame is rendered and its flip queued, and the
//! next frame renders while that flip waits for its vblank. Each frame then has two
//! boundaries. Its render returning frees the pipe for the next frame; its flip retiring
//! publishes the frame ([`PresentPipe::presented`], [`PresentPipe::latest_retirement`], its
//! physical outcome), strictly in submission order. At most [`MAX_UNRETIRED`] frames are
//! between handoff and retirement beyond the queue depth ([`admits`]), and no frame is admitted
//! behind one rendering: the GPU is busy either way, and a queued frame would only age, so the
//! frame rendered next is always built from the newest state.
//!
//! Latest wins. A frame the loop builds while the pipe has no capacity, but a frame handed over
//! earlier still waits for the render thread, replaces that frame instead of queueing behind it
//! ([`PresentPipe::can_replace_queued`]): the waiting frame would only show older state one
//! render later. The replacement takes over the waiting frame's admission (so [`admits`] bounds
//! hold), repaints both frames' damage ([`FramePaint::superseding`]), and retires the frames it
//! replaced with itself, each serial exactly once and in submission order. The client buffers
//! only the replaced frame drew go back to their clients at once: it is never rendered.
//!
//! A presenter panic unwinds the render thread; the next [`PresentPipe::submit`] then sees a
//! closed channel and drops to no presenter, so a lost display doesn't take the session (and
//! clients' unsaved work) with it. The headless backend stays synchronous, since its `submit_frame` output is read back immediately by the test-control `snapshot` command.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use vello::Scene;

use super::frame::FramePaint;
use super::render::OutputId;
use tilo_lock::MutexRecover as _;

/// Frames accepted beyond the one currently being presented, unless the
/// presenter reports its own [`Presenter::queue_depth`].
pub const QUEUE_DEPTH: usize = 1;

/// Frames an overlapping presenter may owe a retirement for while the next one renders: the one
/// whose flip waits for its vblank. The presenter queues the next flip only once that one
/// retired, so with the frame rendering, two frames are between handoff and the screen.
pub const MAX_UNRETIRED: usize = 1;

/// Whether the loop may hand over another frame, with `inflight` frames handed over whose render
/// has not returned and `unretired` rendered frames still waiting for their flip.
///
/// `inflight` stays within `depth` frames beyond the one rendering. The flips awaited stay within
/// [`MAX_UNRETIRED`] beyond those: a presenter owing more than it should is refused frames until
/// they retire, rather than growing the distance between the newest frame and the screen.
pub fn admits(inflight: usize, unretired: usize, depth: usize) -> bool {
    let unretired_limit = depth.saturating_add(MAX_UNRETIRED);
    inflight <= depth && inflight.saturating_add(unretired) <= unretired_limit
}

/// The queue depth the pipe enforces: the presenter's own, raised to the loop's render-bound
/// floor unless the presenter overlaps presentation. An overlapping presenter renders the next
/// frame while the previous one waits for its vblank, so the floor's reason, building only after
/// a flip retired, does not arise; a frame admitted behind the one rendering would wait there a
/// whole render and reach the screen with input that old.
pub fn effective_queue_depth(presenter: usize, floor: usize, overlapping: bool) -> usize {
    if overlapping {
        presenter
    } else {
        presenter.max(floor)
    }
}

/// How long an idle render thread waits on an owed flip before it looks for a new frame again.
/// Bounds how late a frame arriving while a flip is overdue starts to render.
const OWED_RETIRE_SLICE: Duration = Duration::from_millis(2);

/// Normal refresh-rate frames stay silent. Anomalous lifecycle records flow
/// through the session runner's bounded, periodically-synced flight log, so a
/// hard reboot does not erase the last completed diagnostic boundary.
const SLOW_FRAME_LIFECYCLE: Duration = Duration::from_millis(100);

/// Physical presentation proven by a kernel event, never a renderer return.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PhysicalPresentation {
    pub timestamp: Duration,
    pub sequence: u64,
    pub refresh: Option<Duration>,
}

/// Physical outcome associated with one exact producer-frame serial.
pub type PhysicalOutcome = (u64, Option<PhysicalPresentation>);

/// When the display took the frame just presented, for the frame clock.
///
/// Pacing evidence, not presentation proof: `wp_presentation` feedback keeps
/// its stricter exact-serial [`PhysicalPresentation`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DisplayTiming {
    /// Every powered output waits for its page flip in the presenter, so
    /// retirements follow the vblank.
    pub vblank_paced: bool,
    /// The vblank the frame's flips retired on: the earliest among the
    /// outputs that pace the loop, so the next frame is due before the first
    /// of their next vblanks.
    pub vblank: Option<Instant>,
    /// When this frame's flip was queued, i.e. its rendering finished. `None`
    /// when the frame queued no flip (an unchanged frame, a skipped output).
    pub flip_queued_at: Option<Instant>,
}

/// The newest frame the render thread finished, as the logic thread reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Retirement {
    pub serial: u64,
    /// When the presenter returned.
    pub at: Instant,
    /// How long the frame waited behind earlier frames before its render began.
    pub queue_wait: Duration,
    pub display: DisplayTiming,
}

/// What a presenter reports when a frame retires: the frame it presented without one
/// ([`Presenter::owes_retirement`] false), or one whose flip retired later
/// ([`Presenter::retire_owed`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OwedRetirement {
    pub physical: Option<PhysicalPresentation>,
    pub display: DisplayTiming,
}

/// Opt-in sequential wall-clock phases for the exact frame just handled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FramePhaseTiming {
    pub paint: super::render::paint::BridgePhaseTiming,
    /// Time waiting for actual KMS page-flip retirement, separate from GPU completion.
    pub kms_retirement_us: u128,
}

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

    /// Return physical proof only when the exact submitted scene reached scanout.
    /// Nested/headless presenters deliberately return no proof.
    fn present_frame_observed(
        &mut self,
        frame: &FramePaint,
        output: OutputId,
    ) -> Option<PhysicalPresentation> {
        self.present_frame(frame, output);
        None
    }

    /// Content-free profiling for the exact preceding observed frame, when enabled.
    fn frame_phase_timing(&self) -> Option<FramePhaseTiming> {
        None
    }

    /// Frames this presenter accepts beyond the one presenting. Read on the
    /// render thread after every frame, so it may change with the output
    /// topology. A presenter that blocks until each flip retires returns 0.
    fn queue_depth(&self) -> usize {
        QUEUE_DEPTH
    }

    /// Whether this presenter's thread runs realtime: it blocks on the GPU
    /// and the display every few milliseconds, so a realtime policy only makes
    /// its deadlines. A presenter that rasterizes on the CPU on this thread
    /// runs for longer than the realtime budget and stays normal.
    fn realtime(&self) -> bool {
        true
    }

    /// Display timing of the exact preceding observed frame.
    fn display_timing(&self) -> DisplayTiming {
        DisplayTiming::default()
    }

    /// This presenter returns once a frame is rendered and its flip queued, before the flip
    /// retires, so the next frame renders while it waits for its vblank. Read after every frame.
    /// Its own [`Self::queue_depth`] then stands without the loop's render-bound floor
    /// ([`effective_queue_depth`]).
    fn overlaps_presentation(&self) -> bool {
        false
    }

    /// The frame just presented queued a flip that has not retired: its physical outcome and
    /// display timing come from a later [`Self::retire_owed`], and what
    /// [`Self::present_frame_observed`] and [`Self::display_timing`] returned for it is void.
    fn owes_retirement(&self) -> bool {
        false
    }

    /// When the oldest owed flip should retire, so an idle render thread sleeps until then.
    fn owed_retirement_due(&self) -> Option<Instant> {
        None
    }

    /// Owed frames whose flips retired since the last call, oldest first, after waiting at most
    /// `wait` for the oldest still pending. Includes flips retired inside a later present, which
    /// retires the previous flip before it queues its own.
    fn retire_owed(&mut self, _wait: Duration) -> Vec<OwedRetirement> {
        Vec::new()
    }

    /// Build the renderer before the first frame arrives. Called once on the render thread as it
    /// starts, so device setup and shader compilation overlap the compositor's own startup
    /// instead of delaying the first frame.
    fn prepare(&mut self) {}

    /// The output was resized; default no-op for presenters with a fixed surface.
    fn resize(&mut self, _size: (u32, u32)) {}

    /// Power the display on/off, routed through the same channel as frames so a blank can never overtake a frame already queued.
    fn set_dpms(&mut self, _on: bool) {}

    /// What the frame just presented cost, as `key=value` fields for its
    /// `FRAME-LIFECYCLE` line. A presenter that returns fields gets the line
    /// for every frame that changed pixels, not only for slow ones.
    fn frame_detail(&mut self) -> Option<String> {
        None
    }

    /// Idle work, not a frame: bring whatever this presenter retains for
    /// `frame`'s layers up to them while the desktop rests, without showing
    /// anything ([`PresentPipe::warm`]). Asks `keep_going` between steps
    /// and stops as soon as it says a frame waits. Presenters that retain
    /// nothing do nothing.
    fn warm(&mut self, frame: &FramePaint, keep_going: &dyn Fn() -> bool) {
        let _ = (frame, keep_going);
    }
}

enum PresentMsg {
    /// A frame waits in [`PendingFrames`]: take the oldest. One message per frame queued there;
    /// a frame that replaces a waiting one sends none.
    Frame,
    /// Idle work for [`Presenter::warm`]; never presented, never counted in
    /// flight. Its client frames stay pinned until it is done.
    Warm(Box<Warm>),
    Resize((u32, u32)),
    Dpms(bool),
    Stop,
}

/// A frame handed to the render thread and not yet taken by it.
struct QueuedFrame {
    // Boxed so moving it between the queue and the render thread copies a pointer, not a scene.
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
    /// The client frames `frame` draws, held until it was presented: their
    /// buffers do not go back to their clients while it can be rendered.
    pins: crate::shell::gpu_frames::FramePins,
    /// The frame's share of the pipe's capacity, given back however the frame ends.
    admission: Admission,
    /// Serials of the frames this one replaced while they waited, oldest first. They retire
    /// with it, just before it.
    superseded: Vec<u64>,
}

/// Frames handed over that the render thread has not taken yet, oldest first. Shared so the
/// logic thread can replace the newest while it still waits; the channel carries one
/// [`PresentMsg::Frame`] per entry, so frames keep their order against the other messages.
type PendingFrames = Arc<Mutex<VecDeque<QueuedFrame>>>;

fn lock_pending(pending: &PendingFrames) -> std::sync::MutexGuard<'_, VecDeque<QueuedFrame>> {
    pending.lock_recover()
}

/// A frame the pipe accepted ([`PresentPipe::submit_frame_with_input_age`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Submission {
    pub serial: u64,
    /// The waiting frame this one replaced (latest wins). It is never rendered and retires
    /// with this frame; whatever was owed to it on presentation is now this frame's.
    pub replaced: Option<u64>,
}

/// Idle work for the render thread ([`PresentPipe::warm`]).
struct Warm {
    frame: FramePaint,
    pins: crate::shell::gpu_frames::FramePins,
    /// [`PresentPipe::note_input`]'s count when the warm was asked for.
    input_epoch: u64,
}

/// Signalled by the render thread each time a frame retires. Boxed as a trait object so
/// `present` doesn't depend on calloop: the loop passes a closure that pings its event source, tests pass one that bumps a counter.
pub type RetireSignal = Box<dyn Fn() + Send>;

/// Handle to the render thread.
pub struct PresentPipe {
    tx: Sender<PresentMsg>,
    /// Frames handed over and not yet taken by the render thread.
    pending: PendingFrames,
    /// The newest message sent is a frame: a frame built now may replace it while it waits.
    /// Cleared by any other message, which must not be overtaken by a frame built after it.
    frame_is_newest: AtomicBool,
    /// Frames handed over but not yet presented; the logic thread reads it to decide whether to build the next frame.
    inflight: Arc<AtomicUsize>,
    /// Input the logic thread has read, counted ([`Self::note_input`]).
    input_epoch: Arc<AtomicU64>,
    /// Frames presented whose flip has not retired ([`Presenter::owes_retirement`]).
    unretired: Arc<AtomicUsize>,
    /// Serial of the newest frame the render thread has finished handling.
    /// With the FIFO channel this also equals the number of retired frames,
    /// but the identity is what callers need for a reliable acknowledgement.
    presented: Arc<AtomicU64>,
    /// Last serial successfully handed to the render thread.  This is kept
    /// separately from `inflight`: the latter is a bounded queue depth, not
    /// an identity that survives a completed presentation.
    submitted: Arc<AtomicU64>,
    /// Monotonic completion time and display timing of the newest frame the
    /// presenter returned from. Read under one lock so metrics never associate
    /// an old time with a newer frame.
    last_retirement: Arc<Mutex<Option<Retirement>>>,
    /// The presenter's own [`Presenter::queue_depth`], as of its last frame.
    queue_depth: Arc<AtomicUsize>,
    /// [`Presenter::overlaps_presentation`], as of its last frame.
    overlapping: Arc<AtomicBool>,
    /// The logic thread's floor on the queue depth ([`Self::set_min_queue_depth`]).
    min_queue_depth: usize,
    physical_results: Arc<Mutex<VecDeque<PhysicalOutcome>>>,
    /// Ticks on which the paint gate declined because the pipe was full. Surfaced in the paint stats — a large number means the GPU is the bottleneck, a different diagnosis from a slow scene build.
    backpressured: u64,
    /// Set by the render thread just before it returns; lets [`Self::shutdown`] tell "finished" from "wedged" without committing to a `join` that might never return.
    finished: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

#[derive(Clone, Copy)]
struct FrameLifecycle {
    queue_wait: Duration,
    render: Duration,
    total: Duration,
    inflight_at_enqueue: usize,
    input_age_at_enqueue: Option<Duration>,
    phases: Option<FramePhaseTiming>,
    display: DisplayTiming,
    /// From the presenter returning to the frame's flip retiring, for a frame whose
    /// retirement was owed ([`Presenter::owes_retirement`]); zero otherwise.
    retire_wait: Duration,
    /// The presenter overlapped this frame's render with the previous frame's flip.
    overlapped: bool,
}

#[allow(
    clippy::large_types_passed_by_value,
    reason = "consumed once at retirement; a reference would only add a lifetime to the retire path"
)]
fn log_frame_lifecycle(serial: u64, revision: u64, timing: FrameLifecycle) {
    let FrameLifecycle {
        queue_wait,
        render,
        total,
        inflight_at_enqueue,
        input_age_at_enqueue,
        phases,
        display,
        retire_wait,
        overlapped,
    } = timing;
    // Keep the first completed presentation observable even when
    // it is fast and the event loop then parks indefinitely.
    if phases.is_some()
        || serial == 1
        || queue_wait >= SLOW_FRAME_LIFECYCLE
        || render >= SLOW_FRAME_LIFECYCLE
        || total >= SLOW_FRAME_LIFECYCLE
    {
        eprintln!(
            "FRAME-LIFECYCLE serial={serial} event=render-retired queue_wait_us={} render_us={} total_us={} inflight_at_enqueue={inflight_at_enqueue} input_age_at_enqueue_us={} revision={} phase_timing={} encode_submit_us={} gpu_completion_wait_us={} bump_settle_us={} backdrop_us={} kms_retirement_us={} vblank_paced={} flip_to_vblank_us={} overlapped={overlapped} retire_wait_us={} monotonic_us={}",
            queue_wait.as_micros(),
            render.as_micros(),
            total.as_micros(),
            input_age_at_enqueue
                .map_or_else(|| "none".to_owned(), |age| age.as_micros().to_string()),
            revision,
            phases.is_some(),
            phases.map_or(0, |value| value.paint.submit_us),
            phases.map_or(0, |value| value.paint.completed_readback_map_us),
            phases.map_or(0, |value| value.paint.settle_us),
            phases.map_or(0, |value| value.paint.backdrop_us),
            phases.map_or(0, |value| value.kms_retirement_us),
            display.vblank_paced,
            display.flip_queued_at.zip(display.vblank).map_or_else(
                || "none".to_owned(),
                |(queued, vblank)| vblank
                    .saturating_duration_since(queued)
                    .as_micros()
                    .to_string()
            ),
            retire_wait.as_micros(),
            // CLOCK_MONOTONIC at retirement, so a boot or sign-in measurement can place the
            // session's first presented frame on the system's own clock.
            crate::compositor::metrics::monotonic_time_ns()
                .map_or_else(|| "none".to_owned(), |ns| (ns / 1_000).to_string()),
        );
    }
}

/// One admitted frame's share of the pipe's capacity (`inflight`), returned when this drops.
///
/// Releasing by drop makes "exactly once per admitted frame" structural: the render thread
/// releases it when the frame's render returned ([`RetireBoard::release`]), and every other
/// end of the frame gives it back as well. A presenter that panics unwinds the frame out of
/// the thread, a pipe whose thread is gone drops its queued frames, and a send that fails
/// returns the frame to the caller. A counter decremented only on the happy path would
/// instead stay above zero for good, the loop would never be admitted to submit again, and
/// it would never learn that its presenter is gone.
struct Admission {
    inflight: Arc<AtomicUsize>,
}

impl Admission {
    /// Admit one frame; returns its share and how many frames are in flight with it.
    fn admit(inflight: &Arc<AtomicUsize>) -> (Self, usize) {
        let now = inflight.fetch_add(1, Ordering::AcqRel).saturating_add(1);
        (
            Self {
                inflight: inflight.clone(),
            },
            now,
        )
    }
}

impl Drop for Admission {
    fn drop(&mut self) {
        // Release: whatever the render thread stored for the loop before giving the capacity
        // back is visible to a loop that finds it.
        self.inflight.fetch_sub(1, Ordering::Release);
    }
}

/// The render thread's half of the state the logic thread reads after a frame.
struct RetireBoard {
    physical_results: Arc<Mutex<VecDeque<PhysicalOutcome>>>,
    last_retirement: Arc<Mutex<Option<Retirement>>>,
    presented: Arc<AtomicU64>,
    queue_depth: Arc<AtomicUsize>,
    overlapping: Arc<AtomicBool>,
    unretired: Arc<AtomicUsize>,
    /// Frames handed over and not yet rendered: idle work waits for none.
    inflight: Arc<AtomicUsize>,
    /// Input the logic thread has read: idle work stops once more arrives.
    input_epoch: Arc<AtomicU64>,
}

impl RetireBoard {
    /// Publish one frame that reached the display, or was handled without
    /// reaching it: its physical outcome, then the retirement the logic thread
    /// paces by and acknowledges frames with.
    fn retire(&self, retirement: Retirement, physical: Option<PhysicalPresentation>) {
        self.physical_results
            .lock_recover()
            .push_back((retirement.serial, physical));
        let serial = retirement.serial;
        *self.last_retirement.lock_recover() = Some(retirement);
        self.presented.store(serial, Ordering::Release);
    }

    /// Run [`Presenter::warm`] for `frame` while no frame is in flight: a
    /// frame handed over before or during it goes first. Its client frames
    /// go back once it is done.
    ///
    /// Input read since the warm was asked for stops it too, before the
    /// frame that input makes is even built: the GPU is then free for that
    /// frame after at most the one step under way.
    fn warm(
        &self,
        presenter: &mut dyn Presenter,
        Warm {
            frame,
            pins,
            input_epoch,
        }: Warm,
    ) {
        let idle = || {
            self.inflight.load(Ordering::Acquire) == 0
                && self.input_epoch.load(Ordering::Acquire) == input_epoch
        };
        if idle() {
            presenter.warm(&frame, &idle);
        }
        drop(pins);
    }

    /// One handed-over frame finished rendering: return its capacity. The depth
    /// and the flips still owed are stored first, so a loop that finds capacity
    /// already sees what that capacity was granted under.
    fn release(
        &self,
        queue_depth: usize,
        overlapping: bool,
        unretired: usize,
        admission: Admission,
    ) {
        self.queue_depth.store(queue_depth, Ordering::Release);
        self.overlapping.store(overlapping, Ordering::Release);
        self.unretired.store(unretired, Ordering::Release);
        // Give the capacity back *after* the present so the logic thread's capacity check reflects work actually finished, not work dequeued.
        drop(admission);
    }
}

/// Frames the presenter returned from, in submission order, until each is
/// published. A frame owing its retirement holds back every frame after it,
/// so retirements reach the logic thread in the order the frames were
/// submitted: `presented` never passes a frame still waiting for its flip.
struct RetireQueue<T> {
    frames: VecDeque<(T, Option<OwedRetirement>)>,
}

impl<T> Default for RetireQueue<T> {
    fn default() -> Self {
        Self {
            frames: VecDeque::new(),
        }
    }
}

impl<T> RetireQueue<T> {
    /// Frames whose flip has not retired yet.
    fn owed(&self) -> usize {
        self.frames
            .iter()
            .filter(|(_, outcome)| outcome.is_none())
            .count()
    }

    /// A frame the presenter returned from, with its outcome unless it is owed.
    fn push(&mut self, frame: T, outcome: Option<OwedRetirement>) {
        self.frames.push_back((frame, outcome));
    }

    /// Owed flips that retired, oldest first: each settles the oldest frame
    /// still owing one. Surplus outcomes (a presenter retiring a flip it never
    /// reported owing) have no frame to settle and are dropped.
    fn settle(&mut self, outcomes: impl IntoIterator<Item = OwedRetirement>) {
        let mut owed = self
            .frames
            .iter_mut()
            .filter_map(|(_, outcome)| outcome.is_none().then_some(outcome));
        for retired in outcomes {
            let Some(slot) = owed.next() else {
                break;
            };
            *slot = Some(retired);
        }
    }

    /// The settled frames ahead of the first still owed, in order.
    fn take_ready(&mut self) -> Vec<(T, OwedRetirement)> {
        let mut ready = Vec::new();
        while let Some((_, Some(_))) = self.frames.front() {
            if let Some((frame, Some(outcome))) = self.frames.pop_front() {
                ready.push((frame, outcome));
            }
        }
        ready
    }
}

/// A frame the presenter returned from, until its retirement is published.
#[allow(
    clippy::struct_field_names,
    reason = "`rendered_at` is the timestamp of the render, named for the event it marks"
)]
struct Rendered {
    serial: u64,
    revision: u64,
    enqueued_at: Instant,
    queue_wait: Duration,
    render: Duration,
    rendered_at: Instant,
    inflight_at_enqueue: usize,
    input_age_at_enqueue: Option<Duration>,
    phases: Option<FramePhaseTiming>,
    owed: bool,
    overlapped: bool,
    /// Frames this one replaced before they rendered; they retire with it.
    superseded: Vec<u64>,
}

/// The `tilo-present` thread: presents frames, and publishes each one's
/// retirement in submission order.
struct RenderThread {
    presenter: Box<dyn Presenter>,
    pending: PendingFrames,
    board: RetireBoard,
    on_retire: RetireSignal,
    queue: RetireQueue<Rendered>,
}

impl Drop for RenderThread {
    /// The thread is leaving, normally or by unwinding out of the presenter: the flips its
    /// frames still owed will never be collected, so they are no longer between handoff and
    /// the screen. Reporting them as owed would keep the pipe refusing frames for good, and
    /// the loop would never submit again to find the channel closed and drop the presenter.
    fn drop(&mut self) {
        // Frames still waiting will never be rendered: their capacity and client buffers go
        // back now, and a replacement can no longer land on them.
        lock_pending(&self.pending).clear();
        self.board.unretired.store(0, Ordering::Release);
        (self.on_retire)();
    }
}

impl RenderThread {
    fn run(mut self, rx: &Receiver<PresentMsg>) {
        self.presenter.prepare();
        loop {
            let msg = if self.queue.owed() > 0 {
                // A flip is owed: sleep until it should have retired unless a frame
                // arrives first, which then renders beside it.
                let wait = self
                    .presenter
                    .owed_retirement_due()
                    .map_or(OWED_RETIRE_SLICE, |due| {
                        due.saturating_duration_since(Instant::now())
                    });
                match rx.recv_timeout(wait) {
                    Ok(msg) => msg,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        self.settle(OWED_RETIRE_SLICE);
                        continue;
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
            } else {
                match rx.recv() {
                    Ok(msg) => msg,
                    Err(_) => break,
                }
            };
            match msg {
                PresentMsg::Frame => {
                    let Some(QueuedFrame {
                        frame,
                        output,
                        serial,
                        enqueued_at,
                        input_age_at_enqueue,
                        inflight_at_enqueue,
                        pins,
                        admission,
                        superseded,
                    }) = lock_pending(&self.pending).pop_front()
                    else {
                        continue;
                    };
                    let render_started = Instant::now();
                    let queue_wait = render_started.saturating_duration_since(enqueued_at);
                    let physical = self.presenter.present_frame_observed(&frame, output);
                    // Rendered: its client buffers may go back.
                    drop(pins);
                    if let Some(detail) = self.presenter.frame_detail() {
                        eprintln!(
                            "CPU-FRAME serial={serial} revision={} {detail}",
                            frame.revision
                        );
                    }
                    let owed = self.presenter.owes_retirement();
                    let overlapped = self.presenter.overlaps_presentation();
                    let rendered = Rendered {
                        serial,
                        revision: frame.revision,
                        enqueued_at,
                        queue_wait,
                        render: render_started.elapsed(),
                        rendered_at: Instant::now(),
                        inflight_at_enqueue,
                        input_age_at_enqueue,
                        phases: self.presenter.frame_phase_timing(),
                        owed,
                        overlapped,
                        superseded,
                    };
                    let outcome = (!owed).then(|| OwedRetirement {
                        physical,
                        display: self.presenter.display_timing(),
                    });
                    // The frame joins the queue before the presenter is asked what retired: a
                    // flip that retired inside this present (the vblank fell between queueing
                    // it and returning) is this frame's own, and an outcome with no frame
                    // waiting for it would be dropped, leaving the frame owed for good.
                    self.queue.push(rendered, outcome);
                    let retired = self.presenter.retire_owed(Duration::ZERO);
                    self.queue.settle(retired);
                    self.publish_ready();
                    self.board.release(
                        self.presenter.queue_depth(),
                        overlapped,
                        self.queue.owed(),
                        admission,
                    );
                    // Then wake the loop — strictly after the decrement, so a loop that wakes on this signal is guaranteed to see the capacity.
                    (self.on_retire)();
                }
                PresentMsg::Warm(warm) => self.board.warm(&mut *self.presenter, *warm),
                PresentMsg::Resize(size) => {
                    self.presenter.resize(size);
                    self.settle(Duration::ZERO);
                }
                PresentMsg::Dpms(on) => {
                    self.presenter.set_dpms(on);
                    self.settle(Duration::ZERO);
                }
                PresentMsg::Stop => break,
            }
        }
    }

    /// Collect owed flips that retired, waiting at most `wait`, and publish
    /// what they release.
    fn settle(&mut self, wait: Duration) {
        let retired = self.presenter.retire_owed(wait);
        if retired.is_empty() {
            return;
        }
        self.queue.settle(retired);
        self.publish_ready();
        self.board
            .unretired
            .store(self.queue.owed(), Ordering::Release);
        (self.on_retire)();
    }

    fn publish_ready(&mut self) {
        for (frame, outcome) in self.queue.take_ready() {
            let now = Instant::now();
            let at = if frame.owed { now } else { frame.rendered_at };
            log_frame_lifecycle(
                frame.serial,
                frame.revision,
                FrameLifecycle {
                    queue_wait: frame.queue_wait,
                    render: frame.render,
                    total: at.saturating_duration_since(frame.enqueued_at),
                    inflight_at_enqueue: frame.inflight_at_enqueue,
                    input_age_at_enqueue: frame.input_age_at_enqueue,
                    phases: frame.phases,
                    display: outcome.display,
                    retire_wait: if frame.owed {
                        now.saturating_duration_since(frame.rendered_at)
                    } else {
                        Duration::ZERO
                    },
                    overlapped: frame.overlapped,
                },
            );
            // The frames it replaced are shown by it: each retires once, before it.
            for serial in frame.superseded {
                self.board.retire(
                    Retirement {
                        serial,
                        at,
                        queue_wait: frame.queue_wait,
                        display: outcome.display,
                    },
                    outcome.physical,
                );
            }
            self.board.retire(
                Retirement {
                    serial: frame.serial,
                    at,
                    queue_wait: frame.queue_wait,
                    display: outcome.display,
                },
                outcome.physical,
            );
        }
    }
}

/// The name of every thread a GPU driver starts while the render thread builds its renderer.
///
/// Linux names a new thread after the thread that started it, and drivers do not always rename
/// theirs. Mesa's CPU rasteriser (lavapipe) names its rasteriser threads `llvmpipe-N` but leaves
/// its compute-shader pool, which runs every Vello stage, unnamed. Built on the render thread,
/// that pool was called `tilo-present` too, so the CPU a software-rendered VM spent rasterising
/// one frame read as the present thread's (#4085: `tilo-present×14 1.51%` with the present
/// thread itself at 0.07%).
pub const DRIVER_THREAD_NAME: &std::ffi::CStr = c"tilo-gpu-driver";

/// Run `build` (device and renderer setup) with the calling thread named
/// [`DRIVER_THREAD_NAME`], so the threads the driver starts inside it carry that name rather
/// than the caller's. The caller's own name is restored afterwards, also when `build` unwinds.
pub fn with_driver_thread_name<R>(build: impl FnOnce() -> R) -> R {
    #[cfg(target_os = "linux")]
    {
        /// The caller's name as `PR_GET_NAME` wrote it: at most 15 bytes and a NUL.
        struct Restore([u8; 16]);
        impl Drop for Restore {
            fn drop(&mut self) {
                // SAFETY: PR_SET_NAME reads a NUL-terminated string of at most 16 bytes, and
                // the buffer is the 16 bytes PR_GET_NAME filled, NUL included.
                unsafe { libc::prctl(libc::PR_SET_NAME, self.0.as_ptr(), 0, 0, 0) };
            }
        }
        let mut own = [0u8; 16];
        // SAFETY: PR_GET_NAME writes at most 16 bytes, NUL included, into a 16-byte buffer.
        if unsafe { libc::prctl(libc::PR_GET_NAME, own.as_mut_ptr(), 0, 0, 0) } == 0 {
            let _restore = Restore(own);
            // SAFETY: a NUL-terminated string of 15 bytes and its NUL, the most PR_SET_NAME keeps.
            unsafe { libc::prctl(libc::PR_SET_NAME, DRIVER_THREAD_NAME.as_ptr(), 0, 0, 0) };
            return build();
        }
    }
    build()
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
    pub fn spawn(presenter: Box<dyn Presenter>, on_retire: RetireSignal) -> Self {
        let (tx, rx): (Sender<PresentMsg>, Receiver<PresentMsg>) = channel();
        let inflight = Arc::new(AtomicUsize::new(0));
        let input_epoch = Arc::new(AtomicU64::new(0));
        let unretired = Arc::new(AtomicUsize::new(0));
        let queue_depth = Arc::new(AtomicUsize::new(QUEUE_DEPTH));
        let overlapping = Arc::new(AtomicBool::new(false));
        let presented_frames = Arc::new(AtomicU64::new(0));
        let submitted_frames = Arc::new(AtomicU64::new(0));
        let last_retirement = Arc::new(Mutex::new(None));
        let physical_results = Arc::new(Mutex::new(VecDeque::new()));
        let board = RetireBoard {
            physical_results: physical_results.clone(),
            last_retirement: last_retirement.clone(),
            presented: presented_frames.clone(),
            queue_depth: queue_depth.clone(),
            overlapping: overlapping.clone(),
            unretired: unretired.clone(),
            inflight: inflight.clone(),
            input_epoch: input_epoch.clone(),
        };
        let pending: PendingFrames = Arc::new(Mutex::new(VecDeque::new()));
        let thread_pending = pending.clone();
        let finished = Arc::new(AtomicBool::new(false));
        let thread_finished = finished.clone();
        let spawned = std::thread::Builder::new()
            .name("tilo-present".to_string())
            .spawn(move || {
                // Flag completion however this thread leaves (normal return or unwind), so `shutdown` never waits on a thread that's already gone.
                let _done = FinishedOnDrop(thread_finished);
                // Presentation has a deadline every refresh; input and the app
                // under the person's hands share the machine with anything.
                #[cfg(target_os = "linux")]
                if presenter.realtime() {
                    ::launcher::priority::request_realtime(
                        ::launcher::priority::PRESENT_RT_PRIORITY,
                    );
                }
                RenderThread {
                    presenter,
                    pending: thread_pending,
                    board,
                    on_retire,
                    queue: RetireQueue::default(),
                }
                .run(&rx);
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
            pending,
            frame_is_newest: AtomicBool::new(false),
            inflight,
            input_epoch,
            unretired,
            presented: presented_frames,
            submitted: submitted_frames,
            last_retirement,
            queue_depth,
            overlapping,
            min_queue_depth: 0,
            physical_results,
            backpressured: 0,
            finished,
            handle,
        }
    }

    /// Drain exact-frame physical outcomes on the Wayland dispatch thread.
    pub fn take_physical_results(&self) -> Vec<PhysicalOutcome> {
        self.physical_results.lock_recover().drain(..).collect()
    }

    /// Whether another frame may be built and handed over ([`admits`]).
    pub fn has_capacity(&self) -> bool {
        admits(
            self.inflight.load(Ordering::Acquire),
            self.unretired(),
            self.queue_depth(),
        )
    }

    /// The pipe has no capacity, but the newest frame handed over still waits for the render
    /// thread, so a frame built now would replace it rather than queue behind it (latest wins).
    pub fn can_replace_queued(&self) -> bool {
        self.frame_is_newest.load(Ordering::Acquire) && !lock_pending(&self.pending).is_empty()
    }

    /// Frames admitted beyond the one presenting: the presenter's own depth,
    /// raised to the logic thread's floor unless it overlaps presentation
    /// ([`effective_queue_depth`]).
    pub fn queue_depth(&self) -> usize {
        effective_queue_depth(
            self.queue_depth.load(Ordering::Acquire),
            self.min_queue_depth,
            self.overlapping.load(Ordering::Acquire),
        )
    }

    /// Rendered frames whose flip has not retired yet.
    pub fn unretired(&self) -> usize {
        self.unretired.load(Ordering::Acquire)
    }

    /// Admit at least `depth` frames beyond the one presenting, whatever the
    /// presenter reports, unless it overlaps presentation. The loop raises it
    /// when a frame no longer fits one refresh interval, so building overlaps
    /// the previous frame's present.
    pub fn set_min_queue_depth(&mut self, depth: usize) {
        self.min_queue_depth = depth;
    }

    /// Count a tick on which the paint gate declined for lack of capacity.
    pub fn note_backpressure(&mut self) {
        self.backpressured = self.backpressured.saturating_add(1);
        let count = self.backpressured;
        // First refusal plus powers of two reconstruct a sustained render
        // wedge without emitting at the display refresh rate.
        if count == 1 || count.is_power_of_two() {
            eprintln!(
                "FRAME-LIFECYCLE event=backpressure count={count} inflight={} unretired={} submitted={} retired={}",
                self.inflight(),
                self.unretired(),
                self.submitted(),
                self.presented(),
            );
        }
    }

    pub fn backpressured(&self) -> u64 {
        self.backpressured
    }

    /// Serial of the newest frame retired: shown, or handled without reaching the screen. A
    /// frame an overlapping presenter rendered counts only once its flip retired.
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
    /// DRM direct path separately tightens that boundary to a page-flip event,
    /// and reports that event's vblank in [`Retirement::display`]; when it
    /// overlaps rendering with the flip, the retirement is published once the
    /// flip retired ([`Presenter::retire_owed`]), not when the render returned.
    pub fn latest_retirement(&self) -> Option<Retirement> {
        *self.last_retirement.lock_recover()
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
            .map(|submission| submission.serial)
    }

    /// Submit while preserving the age of the oldest input represented by
    /// this frame (diagnostic metadata only).
    ///
    /// Without capacity, a frame replaces the newest frame handed over if that one still waits
    /// for the render thread and nothing was sent after it ([`Self::can_replace_queued`]);
    /// otherwise frames queue in order.
    pub fn submit_frame_with_input_age(
        &mut self,
        frame: FramePaint,
        output: OutputId,
        input_age_at_enqueue: Option<Duration>,
    ) -> Option<Submission> {
        if !self.has_capacity() && self.frame_is_newest.load(Ordering::Acquire) {
            match self.replace_queued(frame, output, input_age_at_enqueue) {
                Ok(submission) => return submission,
                Err(frame) => return self.enqueue(*frame, output, input_age_at_enqueue),
            }
        }
        self.enqueue(frame, output, input_age_at_enqueue)
    }

    /// Put `frame` in place of the newest frame still waiting for the render thread, keeping
    /// its admission. `Err` hands the frame back when none waits any more.
    fn replace_queued(
        &self,
        frame: FramePaint,
        output: OutputId,
        input_age_at_enqueue: Option<Duration>,
    ) -> Result<Option<Submission>, Box<FramePaint>> {
        let mut pending = lock_pending(&self.pending);
        let Some(queued) = pending.back_mut() else {
            return Err(Box::new(frame));
        };
        let serial = self
            .submitted
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        let frame = frame.superseding(&queued.frame);
        let pins = frame.client_frame_pins();
        let replaced = std::mem::replace(&mut queued.serial, serial);
        queued.superseded.push(replaced);
        *queued.frame = frame;
        queued.output = output;
        queued.enqueued_at = Instant::now();
        queued.input_age_at_enqueue = input_age_at_enqueue;
        queued.inflight_at_enqueue = self.inflight.load(Ordering::Acquire);
        // The replaced frame's client buffers go back unless this frame draws them too.
        queued.pins = pins;
        drop(pending);
        if self.finished.load(Ordering::Acquire) {
            // The render thread left after the frame was replaced: nothing will render it.
            lock_pending(&self.pending).clear();
            return Ok(None);
        }
        Ok(Some(Submission {
            serial,
            replaced: Some(replaced),
        }))
    }

    /// Queue `frame` behind the frames handed over before it.
    fn enqueue(
        &self,
        frame: FramePaint,
        output: OutputId,
        input_age_at_enqueue: Option<Duration>,
    ) -> Option<Submission> {
        // Serial zero is the unambiguous "no frame" value used by callers
        // before their first successful submit.  A closed pipe may consume a
        // number, but its caller immediately takes the lost-presenter path,
        // so no acknowledgement can observe that gap.
        let serial = self
            .submitted
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        let (admission, inflight_at_enqueue) = Admission::admit(&self.inflight);
        let enqueued_at = Instant::now();
        let pins = frame.client_frame_pins();
        lock_pending(&self.pending).push_back(QueuedFrame {
            frame: Box::new(frame),
            output,
            serial,
            enqueued_at,
            input_age_at_enqueue,
            inflight_at_enqueue,
            pins,
            admission,
            superseded: Vec::new(),
        });
        if self.tx.send(PresentMsg::Frame).is_err() {
            // No render thread: dropping the frame returns its admission.
            lock_pending(&self.pending).clear();
            return None;
        }
        self.frame_is_newest.store(true, Ordering::Release);
        Some(Submission {
            serial,
            replaced: None,
        })
    }

    /// Hand the render thread idle work: `frame`'s layers, drawn while the
    /// desktop rests, for the presenter to retain ([`Presenter::warm`]).
    /// It runs after the frames already handed over and yields to any frame
    /// handed over after it; it is not a frame, so it takes no capacity and
    /// gets no serial. Returns `false` if the render thread has gone away.
    pub fn warm(&self, frame: FramePaint) -> bool {
        let pins = frame.client_frame_pins();
        self.frame_is_newest.store(false, Ordering::Release);
        let input_epoch = self.input_epoch.load(Ordering::Acquire);
        self.tx
            .send(PresentMsg::Warm(Box::new(Warm {
                frame,
                pins,
                input_epoch,
            })))
            .is_ok()
    }

    /// The logic thread read input: idle work on the render thread
    /// ([`Self::warm`]) yields at its next step, so the frame that input
    /// makes finds the GPU free.
    pub fn note_input(&self) {
        self.input_epoch.fetch_add(1, Ordering::AcqRel);
    }

    pub fn resize(&self, size: (u32, u32)) {
        self.frame_is_newest.store(false, Ordering::Release);
        let _ = self.tx.send(PresentMsg::Resize(size));
    }

    pub fn set_dpms(&self, on: bool) {
        self.frame_is_newest.store(false, Ordering::Release);
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
        self.frame_is_newest.store(false, Ordering::Release);
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

    /// A thread a driver starts while the renderer is built reads [`DRIVER_THREAD_NAME`], not
    /// the render thread's name, and the render thread gets its own name back, also when the
    /// build unwinds.
    #[cfg(target_os = "linux")]
    #[test]
    fn driver_thread_name_reaches_threads_started_inside_and_is_undone_after() {
        fn comm() -> String {
            std::fs::read_to_string("/proc/thread-self/comm")
                .expect("read this thread's name")
                .trim_end()
                .to_owned()
        }
        fn started_thread_comm() -> String {
            std::thread::spawn(comm)
                .join()
                .expect("join an unnamed thread")
        }
        std::thread::Builder::new()
            .name("tilo-present".to_owned())
            .spawn(|| {
                assert_eq!(started_thread_comm(), "tilo-present");
                let inside = with_driver_thread_name(started_thread_comm);
                assert_eq!(inside, DRIVER_THREAD_NAME.to_str().expect("UTF-8 name"));
                assert_eq!(comm(), "tilo-present");
                assert_eq!(started_thread_comm(), "tilo-present");
                let unwound = std::panic::catch_unwind(|| {
                    with_driver_thread_name(|| panic!("intentional driver setup failure"))
                });
                assert!(unwound.is_err());
                assert_eq!(comm(), "tilo-present");
            })
            .expect("spawn a render thread")
            .join()
            .expect("render thread assertions");
    }

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
            let _held = self.gate.lock_recover();
            self.seen.fetch_add(1, Ordering::Release);
        }
        fn resize(&mut self, size: (u32, u32)) {
            self.resizes.lock_recover().push(size);
        }
        fn set_dpms(&mut self, on: bool) {
            self.dpms.lock_recover().push(on);
        }
    }

    /// Idle warming yields to input the loop has read, before the frame
    /// that input makes is built: the warm asks `keep_going` between steps,
    /// and input noted after the warm was asked for answers no.
    #[test]
    fn warming_yields_to_input_before_its_frame_is_built() {
        struct Warming {
            started: std::sync::mpsc::Sender<()>,
            resume: Arc<Mutex<std::sync::mpsc::Receiver<()>>>,
            verdict: std::sync::mpsc::Sender<bool>,
        }
        impl Presenter for Warming {
            fn present(&mut self, _scene: &Scene, _output: OutputId) {}
            fn warm(&mut self, _frame: &FramePaint, keep_going: &dyn Fn() -> bool) {
                let before = keep_going();
                let _ = self.started.send(());
                let _ = self.resume.lock_recover().recv();
                let _ = self.verdict.send(before && !keep_going());
            }
        }
        let (started, started_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume) = std::sync::mpsc::channel();
        let (verdict, verdict_rx) = std::sync::mpsc::channel();
        let mut pipe = spawn_with_retire(Box::new(Warming {
            started,
            resume: Arc::new(Mutex::new(resume)),
            verdict,
        }));
        assert!(pipe.warm(FramePaint::full(Scene::new())));
        started_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the warm started");
        pipe.note_input();
        resume_tx.send(()).expect("resume the warm");
        assert!(
            verdict_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("the warm reported"),
            "the warm went on while idle and stopped at the step after input arrived"
        );
        pipe.shutdown();
    }

    /// `PresentPipe::spawn` with a retire signal that does nothing — what most of these tests want, since they assert on the counters rather than the wakeup.
    fn spawn_with_retire(p: Box<dyn Presenter>) -> PresentPipe {
        PresentPipe::spawn(p, Box::new(|| {}))
    }

    /// A client buffer drawn by a queued frame stays with the compositor until
    /// that frame was presented, even after the store let go of its frame: the
    /// presenter would otherwise draw a buffer its client is writing again.
    #[test]
    fn a_queued_frame_pins_the_client_frames_it_draws() {
        let mut context = ui::gpu::RenderContext::new();
        let Some(id) = pollster::block_on(context.device(None)) else {
            eprintln!("no wgpu adapter; skipping");
            return;
        };
        let handle = &context.devices[id];
        let frame = crate::shell::gpu_frames::frame_for_test(
            &handle.device,
            &handle.queue,
            u64::MAX,
            (4, 4),
            [1, 2, 3, 255],
            31_337,
        );
        let (on_release, given_back) = std::sync::mpsc::channel::<()>();
        frame.hold(crate::shell::gpu_frames::BufferLease::new(move || {
            let _ = on_release.send(());
        }));
        let mut scene = Scene::new();
        scene.fill(
            vello::peniko::Fill::NonZero,
            vello::kurbo::Affine::IDENTITY,
            &vello::peniko::ImageBrush::new(frame.image((0, 0, 4, 4))),
            None,
            &vello::kurbo::Rect::new(0.0, 0.0, 4.0, 4.0),
        );
        let gate = Arc::new(Mutex::new(()));
        let rendering = gate.lock_recover();
        let mut pipe = spawn_with_retire(Box::new(Blocking {
            gate: gate.clone(),
            seen: Arc::new(AtomicU64::new(0)),
            resizes: Arc::new(Mutex::new(Vec::new())),
            dpms: Arc::new(Mutex::new(Vec::new())),
        }));
        assert!(pipe.submit(scene, OutputId(0)));
        // The store replaced the frame while the queued one is still rendering.
        frame.note_superseded();
        drop(frame);
        assert!(
            given_back.recv_timeout(Duration::from_millis(200)).is_err(),
            "the buffer stays held while the queued frame can still draw it"
        );
        drop(rendering);
        tilo_test_env::wait_until("the frame to be presented", || pipe.presented() == 1);
        assert!(
            given_back.recv_timeout(Duration::from_secs(2)).is_ok(),
            "presenting the frame gives the buffer back"
        );
        pipe.shutdown();
    }

    #[test]
    fn physical_outcomes_keep_exact_frame_serials_separate_from_retirement() {
        struct Observed {
            proof: Option<PhysicalPresentation>,
        }
        impl Presenter for Observed {
            fn present(&mut self, _: &Scene, _: OutputId) {}
            fn present_frame_observed(
                &mut self,
                _: &FramePaint,
                _: OutputId,
            ) -> Option<PhysicalPresentation> {
                self.proof.take()
            }
        }
        let proof = PhysicalPresentation {
            timestamp: Duration::new(123, 456_000),
            sequence: 789,
            refresh: None,
        };
        let mut pipe = spawn_with_retire(Box::new(Observed { proof: Some(proof) }));
        let first = pipe
            .submit_frame(FramePaint::full(Scene::new()), OutputId(0))
            .unwrap();
        let second = pipe
            .submit_frame(FramePaint::full(Scene::new()), OutputId(0))
            .unwrap();
        tilo_test_env::wait_until("both submitted frames to retire", || {
            pipe.presented() == second
        });
        assert_eq!(
            pipe.take_physical_results(),
            vec![(first, Some(proof)), (second, None)]
        );
        assert!(pipe.latest_retirement().is_some());
        assert!(
            pipe.take_physical_results().is_empty(),
            "physical events drain once"
        );
        pipe.shutdown();
        let mut plain = spawn_with_retire(Box::new(Blocking {
            gate: Arc::new(Mutex::new(())),
            seen: Arc::new(AtomicU64::new(0)),
            resizes: Arc::new(Mutex::new(Vec::new())),
            dpms: Arc::new(Mutex::new(Vec::new())),
        }));
        assert!(plain.submit(Scene::new(), OutputId(0)));
        tilo_test_env::wait_until("plain presenter to return", || plain.presented() == 1);
        assert_eq!(
            plain.take_physical_results(),
            vec![(1, None)],
            "returning from a presenter cannot prove physical scanout"
        );
        plain.shutdown();
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

    /// What a presenter was handed: revision, predecessor and damage of each frame it rendered.
    type RenderLog = Arc<Mutex<Vec<(u64, Option<u64>, Option<Vec<vello::FineDamageRect>>)>>>;

    /// Blocks inside each frame until released and records what it renders.
    struct Recording {
        gate: Arc<Mutex<()>>,
        rendered: RenderLog,
    }
    impl Presenter for Recording {
        fn present(&mut self, _scene: &Scene, _output: OutputId) {}
        fn present_frame(&mut self, frame: &FramePaint, _output: OutputId) {
            let _held = self.gate.lock_recover();
            self.rendered.lock_recover().push((
                frame.revision,
                frame.previous_revision,
                frame.damage_regions.clone(),
            ));
        }
    }

    fn line(y: u32) -> vello::FineDamageRect {
        vello::FineDamageRect {
            x: 0,
            y,
            width: 100,
            height: 20,
        }
    }

    fn damaged(revision: u64, previous: u64, regions: Vec<vello::FineDamageRect>) -> FramePaint {
        FramePaint {
            revision,
            previous_revision: Some(previous),
            damage_regions: Some(regions),
            ..FramePaint::full(Scene::new())
        }
    }

    /// Latest wins: a frame built while the pipe is full replaces the frame still waiting
    /// behind the one rendering instead of queueing behind it. The waiting frame is never
    /// rendered; the replacement repaints both frames' damage from the frame the presenter
    /// holds, takes over the waiting frame's admission, and every serial retires exactly
    /// once, in submission order.
    #[test]
    fn a_frame_built_while_the_pipe_is_full_replaces_the_frame_still_waiting() {
        let gate = Arc::new(Mutex::new(()));
        let rendered: RenderLog = Arc::new(Mutex::new(Vec::new()));
        let mut pipe = spawn_with_retire(Box::new(Recording {
            gate: gate.clone(),
            rendered: rendered.clone(),
        }));
        let held = gate.lock().expect("hold the presenter inside its frame");
        let rendering = pipe
            .submit_frame(damaged(1, 0, vec![line(0)]), OutputId(0))
            .expect("first frame");
        let waiting = pipe
            .submit_frame(damaged(2, 1, vec![line(100)]), OutputId(0))
            .expect("queued frame");
        assert!(!pipe.has_capacity(), "one frame renders and one waits");
        assert!(
            pipe.can_replace_queued(),
            "the waiting frame has not started, so a newer one may take its place"
        );
        let newest = pipe
            .submit_frame_with_input_age(damaged(3, 2, vec![line(200)]), OutputId(0), None)
            .expect("replacing frame");
        assert_eq!(newest.replaced, Some(waiting));
        assert_eq!(
            pipe.inflight(),
            2,
            "the replacement holds the waiting frame's admission, not one of its own"
        );
        assert!(!pipe.has_capacity());
        drop(held);
        tilo_test_env::wait_until("the replacing frame to retire", || {
            pipe.presented() == newest.serial
        });
        assert_eq!(
            pipe.take_physical_results(),
            vec![(rendering, None), (waiting, None), (newest.serial, None)],
            "each serial retires once, in submission order"
        );
        let rendered = rendered.lock().expect("rendered").clone();
        assert_eq!(
            rendered,
            vec![
                (1, Some(0), Some(vec![line(0)])),
                (3, Some(1), Some(vec![line(100), line(200)])),
            ],
            "the replaced frame is never rendered; the replacement repaints its damage too, \
             relative to the frame the presenter holds"
        );
        assert!(pipe.has_capacity());
        assert!(!pipe.can_replace_queued(), "nothing waits any more");
        pipe.shutdown();
    }

    /// A message sent after a waiting frame (a resize, a blank) must not be overtaken by a
    /// frame built after it, so that frame queues instead of replacing.
    #[test]
    fn a_frame_never_replaces_one_a_control_message_followed() {
        let gate = Arc::new(Mutex::new(()));
        let rendered: RenderLog = Arc::new(Mutex::new(Vec::new()));
        let mut pipe = spawn_with_retire(Box::new(Recording {
            gate: gate.clone(),
            rendered: rendered.clone(),
        }));
        let held = gate.lock().expect("hold the presenter inside its frame");
        assert!(pipe.submit(Scene::new(), OutputId(0)));
        assert!(pipe.submit(Scene::new(), OutputId(0)));
        pipe.resize((640, 480));
        assert!(!pipe.can_replace_queued());
        let third = pipe
            .submit_frame_with_input_age(FramePaint::full(Scene::new()), OutputId(0), None)
            .expect("queued behind the resize");
        assert_eq!(third.replaced, None);
        drop(held);
        tilo_test_env::wait_until("every frame", || pipe.presented() == third.serial);
        assert_eq!(rendered.lock().expect("rendered").len(), 3);
        pipe.shutdown();
    }

    /// A replacement that does not follow the waiting frame's revision cannot prove what
    /// changed since the frame the presenter holds, so it repaints everything.
    #[test]
    fn a_replacement_not_chained_to_the_waiting_frame_repaints_everything() {
        let older = damaged(2, 1, vec![line(0)]);
        let chained = damaged(3, 2, vec![line(40)]).superseding(&older);
        assert_eq!(chained.previous_revision, Some(1));
        assert_eq!(chained.damage_regions, Some(vec![line(0), line(40)]));
        let unrelated = damaged(9, 7, vec![line(40)]).superseding(&older);
        assert_eq!(unrelated.damage_regions, None);
        assert_eq!(unrelated.previous_revision, None);
        let dense_older = FramePaint {
            revision: 2,
            previous_revision: Some(1),
            ..FramePaint::full(Scene::new())
        };
        assert_eq!(
            damaged(3, 2, vec![line(40)])
                .superseding(&dense_older)
                .damage_regions,
            None,
            "a dense waiting frame keeps the replacement dense"
        );
    }

    #[test]
    fn zero_depth_presenter_admits_one_frame() {
        /// A presenter that waits for every flip: no frame queues behind one
        /// presenting. Reports a vblank so the timing's route is pinned too.
        struct VblankPaced {
            gate: Arc<Mutex<()>>,
            vblank: Instant,
        }
        impl Presenter for VblankPaced {
            fn present(&mut self, _scene: &Scene, _output: OutputId) {
                let _held = self.gate.lock_recover();
            }
            fn queue_depth(&self) -> usize {
                0
            }
            fn display_timing(&self) -> DisplayTiming {
                DisplayTiming {
                    vblank_paced: true,
                    vblank: Some(self.vblank),
                    flip_queued_at: None,
                }
            }
        }
        let gate = Arc::new(Mutex::new(()));
        let vblank = Instant::now();
        let mut pipe = spawn_with_retire(Box::new(VblankPaced {
            gate: gate.clone(),
            vblank,
        }));
        // The depth is learned from the presenter as its first frame retires.
        assert!(pipe.submit(Scene::new(), OutputId(0)));
        tilo_test_env::wait_until("the first frame", || pipe.presented() == 1);
        assert_eq!(pipe.queue_depth(), 0);
        let retired = pipe.latest_retirement().expect("a retirement");
        assert_eq!(retired.serial, 1);
        assert_eq!(
            retired.display,
            DisplayTiming {
                vblank_paced: true,
                vblank: Some(vblank),
                flip_queued_at: None,
            },
            "the presenter's vblank must reach the logic thread with its serial"
        );

        let held = gate.lock().expect("hold the presenter inside `present`");
        assert!(pipe.has_capacity());
        assert!(pipe.submit(Scene::new(), OutputId(0)));
        assert!(
            !pipe.has_capacity(),
            "a vblank-paced presenter must not admit a frame behind the one presenting"
        );
        // A frame that no longer fits an interval raises the floor: building
        // overlaps the present again, bounded at one queued frame.
        pipe.set_min_queue_depth(1);
        assert!(pipe.has_capacity());
        assert!(pipe.submit(Scene::new(), OutputId(0)));
        assert!(!pipe.has_capacity());
        pipe.set_min_queue_depth(0);
        drop(held);
        tilo_test_env::wait_until("the held frames", || pipe.presented() == 3);
        assert!(pipe.has_capacity());
        pipe.shutdown();
    }

    #[test]
    fn admission_bounds_frames_in_flight_and_never_queues_behind_a_render() {
        // Phase `an_overlapping_presenter_admits_a_frame_beside_the_owed_flip`.
        assert!(admits(0, 0, 0), "an idle pipe takes a frame");
        assert!(
            admits(0, MAX_UNRETIRED, 0),
            "the next frame renders while the previous one waits for its vblank"
        );
        // Phase `latest_wins_no_frame_waits_behind_one_rendering`.
        assert!(
            !admits(1, 0, 0),
            "a frame queued behind one rendering would age a whole render"
        );
        assert_eq!(
            effective_queue_depth(0, 1, true),
            0,
            "the render-bound floor does not apply while rendering overlaps the flip"
        );
        assert_eq!(
            effective_queue_depth(0, 1, false),
            1,
            "a presenter waiting for its flip keeps the floor"
        );
        // Phase `in_flight_frames_stay_bounded`.
        assert!(
            !admits(0, MAX_UNRETIRED.saturating_add(1), 0),
            "a presenter owing more flips than it should is refused frames"
        );
        assert!(
            admits(1, 1, 1),
            "an explicit queue depth still admits its frame"
        );
        assert!(!admits(1, 2, 1));
        assert!(!admits(2, 0, 1));
    }

    #[test]
    fn retirements_publish_in_submission_order() {
        let shown = OwedRetirement {
            physical: Some(PhysicalPresentation {
                timestamp: Duration::from_millis(50),
                sequence: 3,
                refresh: None,
            }),
            display: DisplayTiming::default(),
        };
        let mut queue = RetireQueue::default();
        queue.push(1_u64, None);
        // An unchanged frame queued no flip: it is shown by the one still owed.
        queue.push(2, Some(OwedRetirement::default()));
        assert_eq!(queue.owed(), 1);
        assert!(
            queue.take_ready().is_empty(),
            "a frame never retires ahead of an earlier one waiting for its flip"
        );
        queue.push(3, None);
        queue.settle([shown]);
        assert_eq!(
            queue.take_ready(),
            vec![(1, shown), (2, OwedRetirement::default())]
        );
        assert_eq!(queue.owed(), 1, "frame 3 still waits for its own flip");
        // A presenter reporting a flip nobody owes cannot settle a future frame.
        queue.settle([shown, shown]);
        assert_eq!(queue.take_ready(), vec![(3, shown)]);
        queue.push(4, None);
        assert!(queue.take_ready().is_empty());
    }

    /// A presenter whose frames return once rendered (held on `gpu`) and retire when
    /// the test releases their flip on `flips`.
    struct Overlapping {
        gpu: Arc<Mutex<()>>,
        rendered: Arc<AtomicU64>,
        /// Whether each next frame queues a flip; true once this runs out.
        queues_flip: VecDeque<bool>,
        flips: Receiver<OwedRetirement>,
        owes: bool,
    }

    impl Presenter for Overlapping {
        fn present(&mut self, _scene: &Scene, _output: OutputId) {
            let _held = self.gpu.lock_recover();
            self.owes = self.queues_flip.pop_front().unwrap_or(true);
            self.rendered.fetch_add(1, Ordering::Release);
        }
        fn queue_depth(&self) -> usize {
            0
        }
        fn overlaps_presentation(&self) -> bool {
            true
        }
        fn owes_retirement(&self) -> bool {
            self.owes
        }
        fn retire_owed(&mut self, wait: Duration) -> Vec<OwedRetirement> {
            let mut retired: Vec<_> = self.flips.recv_timeout(wait).into_iter().collect();
            retired.extend(self.flips.try_iter());
            retired
        }
    }

    fn flip_at(sequence: u64, vblank: Instant) -> OwedRetirement {
        OwedRetirement {
            physical: Some(PhysicalPresentation {
                timestamp: Duration::from_millis(sequence),
                sequence,
                refresh: None,
            }),
            display: DisplayTiming {
                vblank_paced: true,
                vblank: Some(vblank),
                flip_queued_at: Some(vblank),
            },
        }
    }

    #[test]
    fn rendering_overlaps_the_previous_flip_when_the_gpu_is_slower_than_refresh() {
        let gpu = Arc::new(Mutex::new(()));
        let rendered = Arc::new(AtomicU64::new(0));
        let (flip, flips) = channel();
        let mut pipe = spawn_with_retire(Box::new(Overlapping {
            gpu: gpu.clone(),
            rendered: rendered.clone(),
            queues_flip: VecDeque::new(),
            flips,
            owes: false,
        }));
        pipe.set_min_queue_depth(1);
        assert!(pipe.submit(Scene::new(), OutputId(0)));
        tilo_test_env::wait_until("frame 1 to render", || {
            rendered.load(Ordering::Acquire) == 1 && pipe.inflight() == 0
        });
        assert_eq!(
            pipe.presented(),
            0,
            "frame 1 is not shown before its flip retires"
        );
        assert_eq!(pipe.unretired(), 1);
        assert!(
            pipe.has_capacity(),
            "the next frame need not wait for frame 1's vblank"
        );

        // Frame 2 renders for longer than a refresh while frame 1 waits for its vblank.
        let rendering = gpu.lock().expect("hold frame 2 on the GPU");
        assert!(pipe.submit(Scene::new(), OutputId(0)));
        assert!(
            !pipe.has_capacity(),
            "no frame waits behind one rendering, whatever the render-bound floor says"
        );
        drop(rendering);
        tilo_test_env::wait_until("frame 2 to render beside frame 1's pending flip", || {
            rendered.load(Ordering::Acquire) == 2 && pipe.inflight() == 0
        });
        assert_eq!(
            pipe.presented(),
            0,
            "frame 2 rendered without waiting for frame 1's flip"
        );
        assert_eq!(pipe.unretired(), 2);
        assert!(
            !pipe.has_capacity(),
            "two frames between handoff and the screen: no third until one retires"
        );
        let vblank = Instant::now();
        flip.send(flip_at(1, vblank))
            .expect("release frame 1's flip");
        tilo_test_env::wait_until("frame 1 to retire", || pipe.presented() == 1);
        let retired = pipe.latest_retirement().expect("frame 1's retirement");
        assert_eq!(retired.serial, 1);
        assert_eq!(
            retired.display.vblank,
            Some(vblank),
            "the retirement carries the vblank the flip retired on"
        );
        assert!(pipe.has_capacity());

        // Nothing else arrives: the idle render thread still retires frame 2's flip.
        let second = Instant::now();
        flip.send(flip_at(2, second))
            .expect("release frame 2's flip");
        tilo_test_env::wait_until("frame 2 to retire on an idle thread", || {
            pipe.presented() == 2
        });
        assert_eq!(pipe.unretired(), 0);
        assert_eq!(
            pipe.take_physical_results(),
            vec![
                (1, flip_at(1, vblank).physical),
                (2, flip_at(2, second).physical)
            ],
            "physical outcomes keep their exact serials, in order"
        );
        pipe.shutdown();
    }

    #[test]
    fn owed_flips_bound_the_frames_in_flight_and_hold_later_retirements() {
        let rendered = Arc::new(AtomicU64::new(0));
        let (flip, flips) = channel();
        let mut pipe = spawn_with_retire(Box::new(Overlapping {
            gpu: Arc::new(Mutex::new(())),
            rendered: rendered.clone(),
            // Frame 2 is unchanged: it queues no flip and is shown by frame 1's.
            queues_flip: VecDeque::from([true, false, true]),
            flips,
            owes: false,
        }));
        assert!(pipe.submit(Scene::new(), OutputId(0)));
        tilo_test_env::wait_until("frame 1 to render", || pipe.inflight() == 0);
        assert!(pipe.submit(Scene::new(), OutputId(0)));
        tilo_test_env::wait_until("frame 2 to render", || {
            rendered.load(Ordering::Acquire) == 2 && pipe.inflight() == 0
        });
        assert_eq!(
            pipe.presented(),
            0,
            "frame 2 retires with the flip that shows it, not ahead of frame 1"
        );
        assert!(pipe.submit(Scene::new(), OutputId(0)));
        tilo_test_env::wait_until("frame 3 to render", || {
            rendered.load(Ordering::Acquire) == 3 && pipe.inflight() == 0
        });
        assert_eq!(pipe.unretired(), 2);
        assert!(
            !pipe.has_capacity(),
            "two flips owed: no more frames until one retires"
        );
        let vblank = Instant::now();
        flip.send(flip_at(1, vblank))
            .expect("release frame 1's flip");
        tilo_test_env::wait_until("frames 1 and 2 to retire", || pipe.presented() == 2);
        assert!(pipe.has_capacity(), "capacity returns as the flip retires");
        assert_eq!(
            pipe.take_physical_results(),
            vec![(1, flip_at(1, vblank).physical), (2, None)]
        );
        flip.send(flip_at(3, vblank))
            .expect("release frame 3's flip");
        tilo_test_env::wait_until("frame 3 to retire", || pipe.presented() == 3);
        pipe.shutdown();
    }

    /// How a scripted frame ends for [`Scripted`].
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Flip {
        /// Nothing changed: no flip is queued, the previous one shows the frame.
        None,
        /// The flip retires some vblanks after the present returned.
        Later,
        /// The vblank falls between queueing the flip and the present returning, so the
        /// presenter collects it in the same call, together with the one before it.
        WithinPresent,
    }

    /// A presenter that follows the real one's order: a frame first retires the flip the
    /// previous frame left owed, then queues its own, and flips retire oldest first.
    struct Scripted {
        plan: VecDeque<Flip>,
        rendered: Arc<AtomicU64>,
        flips: Receiver<OwedRetirement>,
        /// The previous frame's flip is still owed.
        owing: bool,
        owes: bool,
        serial: u64,
        ready: VecDeque<OwedRetirement>,
    }

    impl Presenter for Scripted {
        fn present(&mut self, _scene: &Scene, _output: OutputId) {
            if self.owing {
                // KMS takes one flip per CRTC: the previous one retires before this queues.
                self.ready.push_back(
                    self.flips
                        .recv_timeout(tilo_test_env::hang_bound())
                        .expect("the test releases the previous flip"),
                );
                self.owing = false;
            }
            self.serial = self.serial.saturating_add(1);
            let step = self.plan.pop_front().unwrap_or(Flip::Later);
            self.owes = step != Flip::None;
            match step {
                Flip::None => {}
                Flip::Later => self.owing = true,
                Flip::WithinPresent => self.ready.push_back(flip_at(self.serial, Instant::now())),
            }
            self.rendered.fetch_add(1, Ordering::Release);
        }
        fn queue_depth(&self) -> usize {
            0
        }
        fn overlaps_presentation(&self) -> bool {
            true
        }
        fn owes_retirement(&self) -> bool {
            self.owes
        }
        fn retire_owed(&mut self, wait: Duration) -> Vec<OwedRetirement> {
            let mut retired: Vec<_> = self.ready.drain(..).collect();
            if !self.owing {
                return retired;
            }
            if let Ok(flip) = self.flips.recv_timeout(wait) {
                retired.push(flip);
                self.owing = false;
            }
            retired
        }
    }

    /// Run `plan` through a pipe, releasing each `Later` flip as the next frame is handed
    /// over (and the last one at the end), and return the pipe once every frame retired.
    fn run_scripted(plan: &[Flip]) -> PresentPipe {
        let rendered = Arc::new(AtomicU64::new(0));
        let (flip, flips) = channel();
        let mut pipe = spawn_with_retire(Box::new(Scripted {
            plan: plan.iter().copied().collect(),
            rendered: rendered.clone(),
            flips,
            owing: false,
            owes: false,
            serial: 0,
            ready: VecDeque::new(),
        }));
        let mut unreleased: Option<u64> = None;
        for (index, step) in plan.iter().enumerate() {
            let serial = u64::try_from(index).expect("small plan").saturating_add(1);
            tilo_test_env::wait_until("the pipe to admit the next frame", || pipe.has_capacity());
            assert!(pipe.submit(Scene::new(), OutputId(0)));
            if let Some(owed) = unreleased.take() {
                flip.send(flip_at(owed, Instant::now()))
                    .expect("release the previous flip");
            }
            if *step == Flip::Later {
                unreleased = Some(serial);
            }
            tilo_test_env::wait_until("the frame to render", || {
                rendered.load(Ordering::Acquire) == serial && pipe.inflight() == 0
            });
        }
        if let Some(owed) = unreleased {
            flip.send(flip_at(owed, Instant::now()))
                .expect("release the last flip");
        }
        let total = u64::try_from(plan.len()).expect("small plan");
        tilo_test_env::wait_until("every frame to retire", || pipe.presented() == total);
        pipe
    }

    /// Every admitted frame gives its capacity back once and retires once, whatever order
    /// the flips of the frames before it retired in. A flip retiring inside the present that
    /// queued it used to have no frame to settle: that frame stayed owed for good, `presented`
    /// stopped and each later frame was admitted only as the one after it retired.
    #[test]
    fn a_flip_retiring_inside_its_own_present_still_retires_its_frame() {
        let mut pipe = run_scripted(&[Flip::WithinPresent]);
        assert_eq!(pipe.unretired(), 0);
        assert_eq!(pipe.inflight(), 0);
        assert!(pipe.has_capacity());
        assert_eq!(
            pipe.take_physical_results().len(),
            1,
            "the frame is published once"
        );
        pipe.shutdown();
    }

    #[test]
    fn capacity_and_retirement_are_exact_for_every_order_flips_retire_in() {
        // Every pair and a long mixed run: each step's flip retires later, inside its own
        // present (together with the previous frame's), or does not exist.
        let steps = [Flip::None, Flip::Later, Flip::WithinPresent];
        let mut plans: Vec<Vec<Flip>> = Vec::new();
        for first in steps {
            for second in steps {
                for third in steps {
                    plans.push(vec![first, second, third]);
                }
            }
        }
        let mut seed = 0x2545_f491_u32;
        plans.push(
            (0..96)
                .map(|_| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    steps[usize::try_from(seed >> 16).expect("fits") % steps.len()]
                })
                .collect(),
        );
        for plan in plans {
            let mut pipe = run_scripted(&plan);
            assert_eq!(pipe.inflight(), 0, "{plan:?}");
            assert_eq!(pipe.unretired(), 0, "{plan:?}");
            assert!(pipe.has_capacity(), "{plan:?}");
            assert_eq!(pipe.presented(), pipe.submitted(), "{plan:?}");
            let results = pipe.take_physical_results();
            let serials: Vec<u64> = results.iter().map(|(serial, _)| *serial).collect();
            let expected: Vec<u64> = (1..=u64::try_from(plan.len()).expect("small")).collect();
            assert_eq!(
                serials, expected,
                "each frame retires once, in order: {plan:?}"
            );
            for ((serial, physical), step) in results.iter().zip(&plan) {
                let sequence = physical.map(|proof| proof.sequence);
                match step {
                    Flip::None => assert_eq!(sequence, None, "{plan:?}"),
                    Flip::Later | Flip::WithinPresent => assert_eq!(
                        sequence,
                        Some(*serial),
                        "a frame retires with its own flip: {plan:?}"
                    ),
                }
            }
            pipe.shutdown();
        }
    }

    /// A presenter that panics takes the render thread, and the frame it was rendering, with
    /// it. That frame's capacity must come back with it: left counted in flight, the loop is
    /// refused every frame, never submits again, and never learns its presenter is gone.
    #[test]
    fn a_dead_render_thread_gives_back_the_capacity_of_the_frame_it_was_rendering() {
        let mut pipe = spawn_with_retire(Box::new(PanickingPresenter));
        assert!(pipe.submit(Scene::new(), OutputId(0)));
        tilo_test_env::wait_until("the presenter panic to terminate its thread", || {
            pipe.is_finished()
        });
        assert_eq!(pipe.inflight(), 0);
        assert!(
            pipe.has_capacity(),
            "the loop must be let submit, to find the closed channel and drop the presenter"
        );
        assert!(!pipe.submit(Scene::new(), OutputId(0)));
        assert_eq!(pipe.inflight(), 0, "a refused frame holds no capacity");
        pipe.shutdown();
    }

    #[test]
    fn a_dead_render_thread_owes_no_flips() {
        // The second frame panics once the first, which owes a flip, has been rendered.
        struct DiesAfterOne(Scripted);
        impl Presenter for DiesAfterOne {
            fn present(&mut self, scene: &Scene, output: OutputId) {
                assert!(self.0.serial < 1, "intentional presenter failure");
                self.0.present(scene, output);
            }
            fn queue_depth(&self) -> usize {
                0
            }
            fn overlaps_presentation(&self) -> bool {
                true
            }
            fn owes_retirement(&self) -> bool {
                self.0.owes
            }
        }
        let rendered = Arc::new(AtomicU64::new(0));
        let (_flip, flips) = channel();
        let mut pipe = spawn_with_retire(Box::new(DiesAfterOne(Scripted {
            plan: VecDeque::from([Flip::Later]),
            rendered: rendered.clone(),
            flips,
            owing: false,
            owes: false,
            serial: 0,
            ready: VecDeque::new(),
        })));
        assert!(pipe.submit(Scene::new(), OutputId(0)));
        tilo_test_env::wait_until("frame 1 to render", || {
            rendered.load(Ordering::Acquire) == 1 && pipe.inflight() == 0
        });
        assert_eq!(pipe.unretired(), 1);
        assert!(pipe.submit(Scene::new(), OutputId(0)));
        tilo_test_env::wait_until("the render thread to die", || pipe.is_finished());
        assert_eq!(pipe.inflight(), 0);
        assert_eq!(
            pipe.unretired(),
            0,
            "its owed flip can no longer be collected"
        );
        assert!(pipe.has_capacity());
        pipe.shutdown();
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
