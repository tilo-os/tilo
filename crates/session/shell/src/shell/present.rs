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

/// Frames accepted beyond the one currently being presented, unless the
/// presenter reports its own [`Presenter::queue_depth`].
pub const QUEUE_DEPTH: usize = 1;

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

    /// Display timing of the exact preceding observed frame.
    fn display_timing(&self) -> DisplayTiming {
        DisplayTiming::default()
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
        observed: Option<super::capture_timing::PipeTiming>,
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
    /// Monotonic completion time and display timing of the newest frame the
    /// presenter returned from. Read under one lock so metrics never associate
    /// an old time with a newer frame.
    last_retirement: Arc<Mutex<Option<Retirement>>>,
    /// The presenter's own [`Presenter::queue_depth`], as of its last frame.
    queue_depth: Arc<AtomicUsize>,
    /// The logic thread's floor on the queue depth ([`Self::set_min_queue_depth`]).
    min_queue_depth: usize,
    physical_results: Arc<Mutex<VecDeque<PhysicalOutcome>>>,
    /// Actual completed presenter duration, published before the retirement wake.
    last_render_nanos: Arc<AtomicU64>,
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
    physical: Option<PhysicalPresentation>,
    display: DisplayTiming,
}

#[derive(Clone, Copy)]
struct FrameLifecycleRecord {
    serial: u64,
    revision: u64,
    timing: FrameLifecycle,
}

impl std::fmt::Display for FrameLifecycleRecord {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self {
            serial,
            revision,
            timing,
        } = *self;
        let FrameLifecycle {
            queue_wait,
            render,
            total,
            inflight_at_enqueue,
            input_age_at_enqueue,
            phases,
            physical,
            display,
        } = timing;
        write!(
            output,
            "FRAME-LIFECYCLE serial={serial} event=render-retired render_work={} target_width={} target_height={} render_attempts={} sparse_attempts={} full_attempts={} damage_admitted={} queue_wait_us={} render_us={} total_us={} inflight_at_enqueue={inflight_at_enqueue} input_age_at_enqueue_us={} revision={} phase_timing={} encode_submit_us={} gpu_completion_wait_us={} bump_settle_us={} backdrop_us={} vblank_paced={} flip_to_vblank_us={} kms_retirement_us={} cpu_copy_us={} readback_bytes={} output_bytes={} physical_presented_time_ns={} physical_sequence={}",
            phases.and_then(|value| value.paint.work).is_some(),
            phases
                .and_then(|value| value.paint.work)
                .map_or(0, |work| work.width),
            phases
                .and_then(|value| value.paint.work)
                .map_or(0, |work| work.height),
            phases
                .and_then(|value| value.paint.work)
                .map_or(0, |work| work.render_attempts),
            phases
                .and_then(|value| value.paint.work)
                .map_or(0, |work| work.sparse_attempts),
            phases
                .and_then(|value| value.paint.work)
                .map_or(0, |work| work.full_attempts),
            phases
                .and_then(|value| value.paint.work)
                .is_some_and(|work| work.damage_admitted),
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
            display.vblank_paced,
            display.flip_queued_at.zip(display.vblank).map_or_else(
                || "none".to_owned(),
                |(queued, at)| at.saturating_duration_since(queued).as_micros().to_string(),
            ),
            phases.map_or(0, |value| value.kms_retirement_us),
            phases.map_or(0, |value| value.paint.cpu_copy_swizzle_us),
            phases.map_or(0, |value| value.paint.readback_bytes),
            phases.map_or(0, |value| value.paint.cpu_output_bytes),
            physical.map_or_else(
                || "none".to_owned(),
                |value| value.timestamp.as_nanos().to_string()
            ),
            physical.map_or_else(|| "none".to_owned(), |value| value.sequence.to_string()),
        )
    }
}

/// Nested host CPU boundaries, with the same exact frame serial as the aggregate.
/// Missing samples are explicit; these durations are not GPU execution time.
fn submission_cpu_record(serial: u64, timing: &FrameLifecycle) -> Option<String> {
    let paint = timing.phases?.paint;
    let observed = paint.submission_cpu;
    let status = match observed {
        Some(value) if value.complete() => "complete",
        Some(value) if value.invalid => "invalid",
        Some(_) => "incomplete",
        None => "missing",
    };
    let value = observed.unwrap_or_default();
    let measured = |count: u32, duration: u64| {
        if count == 0 {
            "none".to_owned()
        } else {
            duration.to_string()
        }
    };
    Some(format!(
        "FRAME-SUBMISSION serial={serial} status={status} scene_count={} command_count={} finish_count={} submit_count={} scene_record_us={} command_encode_us={} encoder_finish_us={} queue_submit_us={} sparse_marker_submit_us={}",
        value.scene_count,
        value.command_count,
        value.finish_count,
        value.submit_count,
        measured(value.scene_count, value.scene_us),
        measured(value.command_count, value.commands_us),
        measured(value.finish_count, value.finish_us),
        measured(value.submit_count, value.submit_us),
        paint
            .sparse_marker_submit_us
            .map_or_else(|| "none".to_owned(), |value| value.to_string()),
    ))
}

/// Device-clock intervals joined only by actual recording ownership and frame serial.
fn submission_gpu_record(serial: u64, revision: u64, timing: &FrameLifecycle) -> Option<String> {
    let gpu = timing.phases?.paint.submission_gpu;
    let status = match gpu {
        Some(value) if value.complete() => "complete",
        Some(value) if !value.encoder_supported => "unsupported",
        Some(value) if !value.encoder_enabled => "disabled",
        Some(_) => "incomplete",
        None => "missing",
    };
    let value = gpu.unwrap_or_default();
    let identity =
        |value: Option<u64>| value.map_or_else(|| "none".to_owned(), |value| value.to_string());
    let measured = |value: u64| {
        if gpu.is_some_and(|gpu| gpu.measured > 0) {
            value.to_string()
        } else {
            "none".to_owned()
        }
    };
    Some(format!(
        "FRAME-GPU-SUBMISSION serial={serial} revision={revision} status={status} process_id={} encoder_supported={} encoder_enabled={} external_queue_work={} renderer_generation={} first_recording={} last_recording={} recording_count={} measured_count={} stale_count={} recording_ns={} post_render_ns={} scope=encoder_before_query_resolve",
        std::process::id(),
        value.encoder_supported,
        value.encoder_enabled,
        value.external_queue_work,
        identity(value.renderer_generation),
        identity(value.first_recording),
        identity(value.last_recording),
        value.recordings,
        value.measured,
        value.stale,
        measured(value.recording_ns),
        measured(value.post_render_ns),
    ))
}

fn log_frame_lifecycle(serial: u64, revision: u64, timing: &FrameLifecycle) {
    // Keep the first completed presentation observable even when
    // it is fast and the event loop then parks indefinitely.
    if timing.phases.is_some()
        || serial == 1
        || timing.queue_wait >= SLOW_FRAME_LIFECYCLE
        || timing.render >= SLOW_FRAME_LIFECYCLE
        || timing.total >= SLOW_FRAME_LIFECYCLE
    {
        if let Some(record) = submission_cpu_record(serial, timing) {
            eprintln!("{record}");
        }
        if let Some(record) = submission_gpu_record(serial, revision, timing) {
            eprintln!("{record}");
        }
        eprintln!(
            "{}",
            FrameLifecycleRecord {
                serial,
                revision,
                timing: *timing
            }
        );
    }
}

#[cfg(test)]
mod phase_record_tests {
    use super::*;

    #[test]
    fn submission_gpu_record_keeps_serial_revision_and_missing_coverage() {
        use crate::shell::render::paint::GpuSubmissionTiming;
        let mut timing = FrameLifecycle {
            queue_wait: Duration::ZERO,
            render: Duration::ZERO,
            total: Duration::ZERO,
            inflight_at_enqueue: 0,
            input_age_at_enqueue: None,
            phases: Some(FramePhaseTiming::default()),
            physical: None,
            display: DisplayTiming::default(),
        };
        assert!(
            submission_gpu_record(41, 9, &timing)
                .unwrap()
                .contains("status=missing")
        );
        timing.phases.as_mut().unwrap().paint.submission_gpu = Some(GpuSubmissionTiming {
            renderer_generation: Some(7),
            encoder_supported: true,
            encoder_enabled: true,
            external_queue_work: false,
            first_recording: Some(3),
            last_recording: Some(4),
            recordings: 2,
            measured: 1,
            recording_ns: 30,
            post_render_ns: 15,
            stale: 0,
        });
        let partial = submission_gpu_record(41, 9, &timing).unwrap();
        assert!(partial.contains("serial=41 revision=9 status=incomplete"));
        assert!(partial.contains("recording_count=2 measured_count=1"));
        timing
            .phases
            .as_mut()
            .unwrap()
            .paint
            .submission_gpu
            .as_mut()
            .unwrap()
            .measured = 2;
        let complete = submission_gpu_record(42, 10, &timing).unwrap();
        assert!(complete.contains("serial=42 revision=10 status=complete"));
        assert!(complete.contains("recording_ns=30 post_render_ns=15"));
        let gpu = timing
            .phases
            .as_mut()
            .unwrap()
            .paint
            .submission_gpu
            .as_mut()
            .unwrap();
        gpu.measured = 0;
        gpu.encoder_supported = false;
        gpu.encoder_enabled = false;
        assert!(
            submission_gpu_record(43, 11, &timing)
                .unwrap()
                .contains("status=unsupported")
        );
        timing
            .phases
            .as_mut()
            .unwrap()
            .paint
            .submission_gpu
            .as_mut()
            .unwrap()
            .encoder_supported = true;
        assert!(
            submission_gpu_record(44, 12, &timing)
                .unwrap()
                .contains("status=disabled")
        );
    }

    #[test]
    fn submission_cpu_record_keeps_exact_serial_and_missing_boundaries() {
        let mut timing = FrameLifecycle {
            queue_wait: Duration::ZERO,
            render: Duration::ZERO,
            total: Duration::ZERO,
            inflight_at_enqueue: 0,
            input_age_at_enqueue: None,
            phases: None,
            physical: None,
            display: DisplayTiming::default(),
        };
        assert!(submission_cpu_record(41, &timing).is_none());
        timing.phases = Some(FramePhaseTiming::default());
        let missing = submission_cpu_record(41, &timing).unwrap();
        assert!(missing.starts_with("FRAME-SUBMISSION serial=41 status=missing "));
        assert!(missing.contains("scene_record_us=none command_encode_us=none"));
        timing.phases.as_mut().unwrap().paint.submission_cpu =
            Some(vello::submission_timing::SubmissionTiming {
                scene_count: 1,
                scene_us: 3,
                ..Default::default()
            });
        let incomplete = submission_cpu_record(41, &timing).unwrap();
        assert!(incomplete.contains("status=incomplete"));
        assert!(incomplete.contains("scene_record_us=3 command_encode_us=none"));
        timing.phases.as_mut().unwrap().paint.submission_cpu =
            Some(vello::submission_timing::SubmissionTiming {
                scene_count: 1,
                command_count: 1,
                finish_count: 1,
                submit_count: 1,
                ..Default::default()
            });
        let complete = submission_cpu_record(42, &timing).unwrap();
        assert!(complete.starts_with("FRAME-SUBMISSION serial=42 status=complete "));
        assert!(complete.contains("encoder_finish_us=0 queue_submit_us=0"));
        assert!(complete.ends_with("sparse_marker_submit_us=none"));
    }

    #[test]
    fn same_frame_phase_export_distinguishes_readback_zero_and_missing_samples() {
        let mut record = FrameLifecycleRecord {
            serial: 41,
            revision: 73,
            timing: FrameLifecycle {
                queue_wait: Duration::ZERO,
                render: Duration::ZERO,
                total: Duration::ZERO,
                inflight_at_enqueue: 1,
                input_age_at_enqueue: None,
                phases: None,
                physical: None,
                display: DisplayTiming::default(),
            },
        };
        let absent = record.to_string();
        assert!(absent.contains("serial=41 "));
        assert!(absent.contains("revision=73 phase_timing=false"));
        assert!(absent.ends_with("cpu_copy_us=0 readback_bytes=0 output_bytes=0 physical_presented_time_ns=none physical_sequence=none"));
        record.timing.phases = Some(FramePhaseTiming {
            paint: super::super::render::paint::BridgePhaseTiming {
                cpu_copy_swizzle_us: 17,
                readback_bytes: 4096,
                cpu_output_bytes: 2048,
                ..Default::default()
            },
            kms_retirement_us: 19,
        });
        let readback = record.to_string();
        assert!(readback.contains("revision=73 phase_timing=true"));
        assert!(readback.ends_with(
            "kms_retirement_us=19 cpu_copy_us=17 readback_bytes=4096 output_bytes=2048 physical_presented_time_ns=none physical_sequence=none"
        ));
        record.timing.phases.as_mut().unwrap().paint =
            super::super::render::paint::BridgePhaseTiming::default();
        let direct = record.to_string();
        assert!(direct.contains("phase_timing=true"));
        assert!(direct.contains("render_work=false target_width=0 target_height=0"));
        record.timing.phases.as_mut().unwrap().paint.work =
            Some(super::super::render::paint::RenderWork {
                width: 1920,
                height: 1080,
                render_attempts: 2,
                sparse_attempts: 1,
                full_attempts: 1,
                damage_admitted: false,
            });
        let recovered = record.to_string();
        assert!(recovered.contains("render_work=true target_width=1920 target_height=1080 render_attempts=2 sparse_attempts=1 full_attempts=1 damage_admitted=false"));
        record.timing.physical = Some(PhysicalPresentation {
            timestamp: Duration::new(123, 456_000),
            sequence: 789,
            refresh: None,
        });
        assert!(
            record
                .to_string()
                .ends_with("physical_presented_time_ns=123000456000 physical_sequence=789")
        );

        assert!(direct.ends_with("cpu_copy_us=0 readback_bytes=0 output_bytes=0 physical_presented_time_ns=none physical_sequence=none"));
        let queued = Instant::now();
        record.timing.display = DisplayTiming {
            vblank_paced: true,
            vblank: queued.checked_add(Duration::from_micros(250)),
            flip_queued_at: Some(queued),
        };
        let paced = record.to_string();
        assert!(paced.contains("vblank_paced=true flip_to_vblank_us=250"));
        assert!(paced.ends_with("physical_presented_time_ns=123000456000 physical_sequence=789"));
    }
}

/// The render thread's half of the state the logic thread reads after a frame.
struct RetireBoard {
    physical_results: Arc<Mutex<VecDeque<PhysicalOutcome>>>,
    last_retirement: Arc<Mutex<Option<Retirement>>>,
    presented: Arc<AtomicU64>,
    queue_depth: Arc<AtomicUsize>,
    inflight: Arc<AtomicUsize>,
}

impl RetireBoard {
    fn push_physical(&self, outcome: PhysicalOutcome) {
        self.physical_results
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(outcome);
    }

    /// Publish one finished frame, ending with the in-flight decrement that
    /// returns its capacity.
    fn publish(&self, retirement: Retirement, queue_depth: usize) {
        let serial = retirement.serial;
        *self
            .last_retirement
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(retirement);
        self.presented.store(serial, Ordering::Release);
        // Before the decrement: a loop that finds capacity already sees the
        // depth that capacity was granted under.
        self.queue_depth.store(queue_depth, Ordering::Release);
        // Decrement *after* the present so the logic thread's capacity check reflects work actually finished, not work dequeued.
        self.inflight.fetch_sub(1, Ordering::Release);
    }
}

/// Log one presented frame: its lifecycle record and the presenter's own detail.
fn report_frame(
    presenter: &mut dyn Presenter,
    serial: u64,
    revision: u64,
    timing: &FrameLifecycle,
) {
    log_frame_lifecycle(serial, revision, timing);
    if let Some(detail) = presenter.frame_detail() {
        eprintln!("CPU-FRAME serial={serial} revision={revision} {detail}");
    }
}

/// How long [`PresentPipe::shutdown`] waits for the render thread before giving up on it.
///
/// Generous enough to cover a frame in flight on any real display (a 24 Hz mode is 42 ms) plus a slow driver's teardown, short enough that a wedged GPU cannot hold the session open.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

fn shared_render_counter() -> (Arc<AtomicU64>, Arc<AtomicU64>) {
    let counter = Arc::new(AtomicU64::new(0));
    (counter.clone(), counter)
}

fn note_render(latest: &AtomicU64, render: Duration) {
    latest.store(
        u64::try_from(render.as_nanos()).unwrap_or(u64::MAX),
        Ordering::Release,
    );
}

fn presentation_thread_handle(
    spawned: std::io::Result<std::thread::JoinHandle<()>>,
    finished: &AtomicBool,
) -> Option<std::thread::JoinHandle<()>> {
    match spawned {
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
    }
}

impl PresentPipe {
    /// Move `presenter` onto a dedicated thread named `tilo-present` (identifiable in `perf`/`top` output and backtraces).
    ///
    /// `on_retire` fires on the render thread each time a frame finishes presenting, so the loop can
    /// wake up — without it, a parked loop sleeps out the whole frame interval even though capacity
    /// came back microseconds later, measured as a 33.3 ms paint period (every other deadline) on a render thread keeping up far better than that.
    pub fn spawn(mut presenter: Box<dyn Presenter>, on_retire: RetireSignal) -> Self {
        let (tx, rx): (Sender<PresentMsg>, Receiver<PresentMsg>) = channel();
        let inflight = Arc::new(AtomicUsize::new(0));
        let (last_render_nanos, thread_last_render_nanos) = shared_render_counter();
        let queue_depth = Arc::new(AtomicUsize::new(QUEUE_DEPTH));
        let presented_frames = Arc::new(AtomicU64::new(0));
        let submitted_frames = Arc::new(AtomicU64::new(0));
        let last_retirement = Arc::new(Mutex::new(None));
        let physical_results = Arc::new(Mutex::new(VecDeque::new()));
        let board = RetireBoard {
            physical_results: physical_results.clone(),
            last_retirement: last_retirement.clone(),
            presented: presented_frames.clone(),
            queue_depth: queue_depth.clone(),
            inflight: inflight.clone(),
        };
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
                            mut observed,
                            input_age_at_enqueue,
                            inflight_at_enqueue,
                        } => {
                            let render_started = Instant::now();
                            super::capture_timing::PipeTiming::render_started(&mut observed);
                            let queue_wait = render_started.saturating_duration_since(enqueued_at);
                            let physical = presenter.present_frame_observed(&frame, output);
                            board.push_physical((serial, physical));
                            let phases = presenter.frame_phase_timing();
                            let display = presenter.display_timing();
                            let render_done = Instant::now();
                            super::capture_timing::PipeTiming::retire(observed, serial);
                            let render = render_done.saturating_duration_since(render_started);
                            let total = render_done.saturating_duration_since(enqueued_at);
                            note_render(&thread_last_render_nanos, render);
                            report_frame(
                                &mut *presenter,
                                serial,
                                frame.revision,
                                &FrameLifecycle {
                                    queue_wait,
                                    render,
                                    total,
                                    inflight_at_enqueue,
                                    input_age_at_enqueue,
                                    phases,
                                    physical,
                                    display,
                                },
                            );
                            board.publish(
                                Retirement {
                                    serial,
                                    at: render_done,
                                    queue_wait,
                                    display,
                                },
                                presenter.queue_depth(),
                            );
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
        let handle = presentation_thread_handle(spawned, &finished);
        Self {
            tx,
            inflight,
            presented: presented_frames,
            submitted: submitted_frames,
            last_retirement,
            queue_depth,
            min_queue_depth: 0,
            physical_results,
            last_render_nanos,
            backpressured: 0,
            finished,
            handle,
        }
    }

    /// Drain exact-frame physical outcomes on the Wayland dispatch thread.
    pub fn take_physical_results(&self) -> Vec<PhysicalOutcome> {
        self.physical_results
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain(..)
            .collect()
    }

    /// Whether another frame may be built and handed over.
    pub fn has_capacity(&self) -> bool {
        self.inflight.load(Ordering::Acquire) <= self.queue_depth()
    }

    /// Frames admitted beyond the one presenting: the presenter's own depth,
    /// raised to the logic thread's floor.
    pub fn queue_depth(&self) -> usize {
        self.queue_depth
            .load(Ordering::Acquire)
            .max(self.min_queue_depth)
    }

    /// Admit at least `depth` frames beyond the one presenting, whatever the
    /// presenter reports. The loop raises it when a frame no longer fits one
    /// refresh interval, so building overlaps the previous frame's present.
    pub fn set_min_queue_depth(&mut self, depth: usize) {
        self.min_queue_depth = depth;
    }

    /// Keep overlap for presenters meeting the current cadence. After a slow
    /// completion, wait for the active frame to retire before building from
    /// current model/input state, rather than queueing another already-old scene.
    /// Submitted frames and control messages retain their original FIFO order.
    pub fn has_capacity_for_interval(&self, interval: Duration) -> bool {
        if self.is_finished() {
            return self.has_capacity();
        }
        if interval.is_zero() {
            return self.has_capacity();
        }
        let completed_render = Duration::from_nanos(self.last_render_nanos.load(Ordering::Acquire));
        if completed_render > interval {
            self.inflight.load(Ordering::Acquire) == 0
        } else {
            self.has_capacity()
        }
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
    /// DRM direct path separately tightens that boundary to a page-flip event,
    /// and reports that event's vblank in [`Retirement::display`].
    pub fn latest_retirement(&self) -> Option<Retirement> {
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
        let observed = super::capture_timing::PipeTiming::start();
        if self
            .tx
            .send(PresentMsg::Frame {
                frame: Box::new(frame),
                output,
                serial,
                enqueued_at,
                observed,
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

    #[test]
    fn slow_presenter_admission_waits_for_retirement_without_dropping_frames() {
        struct Controlled {
            started: Sender<u64>,
            release: Receiver<()>,
        }
        impl Presenter for Controlled {
            fn present(&mut self, _: &Scene, _: OutputId) {}
            fn present_frame(&mut self, frame: &FramePaint, _: OutputId) {
                if frame.revision == 1 {
                    std::thread::sleep(Duration::from_millis(20));
                } else {
                    let _ = self.started.send(frame.revision);
                    let _ = self.release.recv_timeout(Duration::from_secs(3));
                }
            }
        }
        let (started_tx, started_rx) = channel();
        let (release_tx, release_rx) = channel();
        let (wake_tx, wake_rx) = channel();
        let mut pipe = PresentPipe::spawn(
            Box::new(Controlled {
                started: started_tx,
                release: release_rx,
            }),
            Box::new(move || {
                let _ = wake_tx.send(());
            }),
        );
        let mut warm = FramePaint::full(Scene::new());
        warm.revision = 1;
        let first = pipe.submit_frame(warm, OutputId(0)).unwrap();
        let warm_retired = wake_rx.recv_timeout(Duration::from_secs(3)).is_ok();
        let mut latest = FramePaint::full(Scene::new());
        latest.revision = 2;
        let second = pipe.submit_frame(latest, OutputId(0)).unwrap();
        let started = started_rx.recv_timeout(Duration::from_secs(3));
        let slow_capacity = pipe.has_capacity_for_interval(Duration::from_millis(1));
        let normal_capacity = pipe.has_capacity_for_interval(Duration::from_secs(60));
        let withheld_serial = pipe.submitted();
        // Withheld admission leaves model/input work on the producer; the next
        // accepted scene is built from that latest state, never a dropped FIFO item.
        let mut newest = FramePaint::full(Scene::new());
        newest.revision = 3;
        let _ = release_tx.send(());
        let second_retired = wake_rx.recv_timeout(Duration::from_secs(3)).is_ok();
        let resumed = pipe.has_capacity_for_interval(Duration::from_millis(1));
        let third = pipe.submit_frame(newest, OutputId(0)).unwrap();
        let newest_started = started_rx.recv_timeout(Duration::from_secs(3));
        let _ = release_tx.send(());
        let third_retired = wake_rx.recv_timeout(Duration::from_secs(3)).is_ok();
        let results = pipe.take_physical_results();
        pipe.shutdown();
        assert!(warm_retired && second_retired && third_retired);
        assert_eq!(started.unwrap(), 2);
        assert!(
            !slow_capacity,
            "a completed slow presenter must not queue stale scenes behind active render"
        );
        assert!(
            normal_capacity,
            "normal cadence retains the existing overlap"
        );
        assert_eq!(
            withheld_serial, second,
            "withholding must not submit or discard a serial"
        );
        assert!(
            resumed,
            "retirement wakes the producer after capacity returns"
        );
        assert_eq!(newest_started.unwrap(), 3);
        assert_eq!(results, vec![(first, None), (second, None), (third, None)]);
    }

    #[test]
    fn slow_presenter_failure_still_exposes_the_closed_submit_path() {
        struct FailAfterSlow;
        impl Presenter for FailAfterSlow {
            fn present(&mut self, _: &Scene, _: OutputId) {}
            fn present_frame(&mut self, frame: &FramePaint, _: OutputId) {
                if frame.revision == 1 {
                    std::thread::sleep(Duration::from_millis(20));
                } else {
                    panic!("test presenter fails after an actual slow completion");
                }
            }
        }
        let mut pipe = spawn_with_retire(Box::new(FailAfterSlow));
        let mut warm = FramePaint::full(Scene::new());
        warm.revision = 1;
        let first = pipe.submit_frame(warm, OutputId(0)).unwrap();
        tilo_test_env::wait_until("slow warm frame retires", || pipe.presented() == first);
        let mut failed = FramePaint::full(Scene::new());
        failed.revision = 2;
        let _ = pipe.submit_frame(failed, OutputId(0)).unwrap();
        tilo_test_env::wait_until("actual presenter unwind closes the pipe", || {
            pipe.is_finished()
        });
        let capacity = pipe.has_capacity_for_interval(Duration::from_millis(1));
        let submission = pipe.submit_frame(FramePaint::full(Scene::new()), OutputId(0));
        let retired = pipe.presented();
        let outcomes = pipe.take_physical_results();
        pipe.shutdown();
        assert!(
            capacity,
            "slow admission must still expose the ordinary closed-channel fallback"
        );
        assert!(
            submission.is_none(),
            "a failed presenter cannot accept another frame"
        );
        assert_eq!(retired, first, "failed frames do not retire");
        assert_eq!(outcomes, vec![(first, None)]);
    }

    struct AdmissionBenchmarkPresenter {
        sent: Arc<Mutex<std::collections::BTreeMap<u64, Instant>>>,
        queued: Arc<Mutex<Vec<Duration>>>,
    }

    impl Presenter for AdmissionBenchmarkPresenter {
        fn present(&mut self, _: &Scene, _: OutputId) {}
        fn present_frame(&mut self, frame: &FramePaint, _: OutputId) {
            let elapsed = self
                .sent
                .lock()
                .unwrap()
                .get(&frame.revision)
                .unwrap()
                .elapsed();
            self.queued.lock().unwrap().push(elapsed);
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn measure_pipe_admission(
        capacity: fn(&PresentPipe) -> bool,
    ) -> (Vec<Duration>, usize, Duration) {
        let sent = Arc::new(Mutex::new(std::collections::BTreeMap::new()));
        let queued = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = channel();
        let mut pipe = PresentPipe::spawn(
            Box::new(AdmissionBenchmarkPresenter {
                sent: sent.clone(),
                queued: queued.clone(),
            }),
            Box::new(move || {
                let _ = tx.send(());
            }),
        );
        let mut maximum_inflight = 0;
        let started = Instant::now();
        for revision in 1..=7 {
            while !capacity(&pipe) {
                rx.recv_timeout(Duration::from_secs(3))
                    .expect("real retirement wake");
            }
            let mut frame = FramePaint::full(Scene::new());
            frame.revision = revision;
            sent.lock().unwrap().insert(revision, Instant::now());
            assert!(pipe.submit_frame(frame, OutputId(0)).is_some());
            maximum_inflight = maximum_inflight.max(pipe.inflight());
            if revision == 1 {
                rx.recv_timeout(Duration::from_secs(3))
                    .expect("warm presenter retirement");
            }
        }
        while pipe.inflight() != 0 {
            rx.recv_timeout(Duration::from_secs(3))
                .expect("all admitted frames retire");
        }
        let elapsed = started.elapsed();
        pipe.shutdown();
        let samples = queued.lock().unwrap().clone();
        (
            samples.into_iter().skip(1).collect(),
            maximum_inflight,
            elapsed,
        )
    }

    #[test]
    fn adaptive_admission_measures_slow_pipe_queue_without_changing_work() {
        let (mut overlap, overlap_inflight, overlap_elapsed) =
            measure_pipe_admission(PresentPipe::has_capacity);
        let (mut adaptive, adaptive_inflight, adaptive_elapsed) =
            measure_pipe_admission(|pipe| pipe.has_capacity_for_interval(Duration::from_millis(1)));
        overlap.sort();
        adaptive.sort();
        eprintln!(
            "PIPE-ADMISSION-BENCH scope=controlled-20ms-presenter samples={} overlap_queue_median_us={} adaptive_queue_median_us={} overlap_inflight={overlap_inflight} adaptive_inflight={adaptive_inflight} overlap_elapsed_us={} adaptive_elapsed_us={}",
            overlap.len(),
            overlap.get(3).unwrap().as_micros(),
            adaptive.get(3).unwrap().as_micros(),
            overlap_elapsed.as_micros(),
            adaptive_elapsed.as_micros(),
        );
        assert_eq!(overlap.len(), 6);
        assert_eq!(adaptive.len(), 6, "every accepted frame still rendered");
        assert_eq!(overlap_inflight, 2);
        assert_eq!(
            adaptive_inflight, 1,
            "slow cadence admits only actual idle capacity"
        );
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
    fn zero_depth_presenter_admits_one_frame() {
        /// A presenter that waits for every flip: no frame queues behind one
        /// presenting. Reports a vblank so the timing's route is pinned too.
        struct VblankPaced {
            gate: Arc<Mutex<()>>,
            vblank: Instant,
        }
        impl Presenter for VblankPaced {
            fn present(&mut self, _scene: &Scene, _output: OutputId) {
                let _held = self
                    .gate
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
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
