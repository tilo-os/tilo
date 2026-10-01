//! UI render worker thread.
//!
//! Moves scene encode, swapchain acquire, blit, submit, present, and poll
//! off the main winit thread onto a dedicated `ui-render` worker. Main keeps
//! the widget tree + scene encoding; the worker owns the wgpu
//! `RenderContext`, `RenderSurface<'static>`, and `vello::Renderer`.
//!
//! Three channels: `render_tx`/`rx` (capacity 1 — blocks main until the
//! worker drains the previous frame) moves an encoded `Scene` + dims +
//! base colour; `pool_tx`/`buffers` (worker → main, capacity 2,
//! pre-seeded) recycles each `Scene` and damage vector as one atomic pair;
//! `outcome_tx`/`frame_complete` (capacity 8) reports per-frame outcomes so
//! main can re-request a redraw on Outdated/Suboptimal/Occluded/Timeout.
//!
//! The worker resizes the surface itself — main never touches it after
//! handing it off at spawn — so wgpu state never crosses threads.
//!
//! On `exiting`, the App sends `RenderRequest::Shutdown` and `join()`s the
//! worker, which drops its `RenderSurface<'static>` before the `Window` is
//! dropped — required or wgpu panics on a surface outliving its window.
//! On `suspended` the worker drops only the surface and hands its context
//! and renderer back ([`RenderThreadHandle::shutdown_keeping_renderer`]), so
//! the next surface does not open a device and compile pipelines again.
#![allow(
    clippy::disallowed_methods,
    reason = "native host runner: the GPU render worker is a dedicated thread"
)]

use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::JoinHandle;

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
fn install_first_frame_sigill_probe() {
    extern "C" fn report_fault(
        _signal: libc::c_int,
        _info: *mut libc::siginfo_t,
        ctx: *mut libc::c_void,
    ) {
        // SAFETY: Linux passes a valid aarch64 ucontext to a SA_SIGINFO handler.
        let pc = unsafe { (*(ctx.cast::<libc::ucontext_t>())).uc_mcontext.pc };
        let mut digits = [b'0'; 16];
        let alphabet = b"0123456789abcdef";
        for (index, digit) in digits.iter_mut().enumerate() {
            *digit = alphabet[((pc >> (4 * (15 - index))) & 0xf) as usize];
        }
        // SAFETY: write/open/read/close/_exit are async-signal-safe system calls.
        unsafe {
            libc::write(
                libc::STDERR_FILENO,
                b"TILO_SIGILL_PC 0x".as_ptr().cast(),
                b"TILO_SIGILL_PC 0x".len(),
            );
            libc::write(libc::STDERR_FILENO, digits.as_ptr().cast(), digits.len());
            libc::write(libc::STDERR_FILENO, b"\n".as_ptr().cast(), 1);
            let fd = libc::open(c"/proc/self/maps".as_ptr(), libc::O_RDONLY);
            if fd >= 0 {
                libc::write(
                    libc::STDERR_FILENO,
                    b"TILO_SIGILL_MAPS_BEGIN\n".as_ptr().cast(),
                    b"TILO_SIGILL_MAPS_BEGIN\n".len(),
                );
                let mut buffer = [0_u8; 4096];
                loop {
                    let count = libc::read(fd, buffer.as_mut_ptr().cast(), buffer.len());
                    if count <= 0 {
                        break;
                    }
                    libc::write(libc::STDERR_FILENO, buffer.as_ptr().cast(), count as usize);
                }
                libc::close(fd);
                libc::write(
                    libc::STDERR_FILENO,
                    b"TILO_SIGILL_MAPS_END\n".as_ptr().cast(),
                    b"TILO_SIGILL_MAPS_END\n".len(),
                );
            }
            libc::_exit(128 + libc::SIGILL);
        }
    }

    // SAFETY: the action is initialized before use and the handler has the
    // required SA_SIGINFO signature. This diagnostic is enabled only when the
    // host requested startup metrics.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = report_fault as *const () as usize;
        action.sa_flags = libc::SA_SIGINFO;
        libc::sigemptyset(&mut action.sa_mask);
        if libc::sigaction(libc::SIGILL, &action, std::ptr::null_mut()) == 0 {
            tilo_startup_metrics::mark("render-worker-sigill-probe-installed");
        }
    }
}

#[path = "render_timing.rs"]
pub(crate) mod timing;

use crate::damage::{Region, coalesce_regions};
use crate::gpu::RenderContext;
use crate::offscreen::{
    damage_geometric_capacity_enabled, damage_texture_extent,
    damage_texture_geometric_needs_replacement, damage_texture_geometric_width_extent,
    damage_texture_height_extent, damage_texture_needs_replacement,
};
use vello::kurbo::Rect;
use vello::peniko::Color;
use vello::util::RenderSurface;
use vello::{FineDamageRect, RenderParams, Renderer, Scene};

/// A hook run on the render thread *after* vello encodes the frame into its
/// intermediate texture and *before* that texture is blit to the surface.
/// It may composite extra GPU content into `target` — used by the browser
/// to draw its zero-copy `WebView` texture (which vello cannot sample) over
/// the transparent region the chrome leaves for it.
pub trait FramePostProcess: Send {
    fn post_render(
        &mut self,
        device: &vello::wgpu::Device,
        queue: &vello::wgpu::Queue,
        target: &vello::wgpu::TextureView,
        width: u32,
        height: u32,
    );
}

static POST_PROCESSOR: std::sync::Mutex<Option<Box<dyn FramePostProcess>>> =
    std::sync::Mutex::new(None);
static POST_PROCESSOR_ACTIVE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Called by an app before its window comes up. A no-op for apps that
/// don't set one, so the shared render path is unchanged for everyone else.
pub fn set_frame_post_processor(p: Box<dyn FramePostProcess>) {
    // A poisoned lock means some other caller already panicked while
    // holding it; there's nothing safe left to do but skip installing the
    // processor rather than taking this caller down too.
    if let Ok(mut guard) = POST_PROCESSOR.lock() {
        *guard = Some(p);
        POST_PROCESSOR_ACTIVE.store(true, std::sync::atomic::Ordering::Release);
    }
}

/// Compile-time `Send` check: a vello/wgpu regression fails to compile rather
/// than at thread spawn.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<Renderer>();
    assert_send::<Scene>();
    assert_send::<RenderSurface<'static>>();
    assert_send::<RenderContext>();
};

#[allow(
    clippy::large_enum_variant,
    reason = "keeping Scene inline removes a heap allocation on every frame; the bounded channel reserves only one slot"
)]
/// Runs on the render thread once a frame is presented.
pub type PresentHook = Box<dyn FnOnce(crate::time::SystemTime) + Send>;

pub enum RenderRequest {
    Frame {
        /// Originating CPU profile sample; auxiliary/unprofiled frames have none.
        cpu_frame_id: Option<u64>,
        // Inline deliberately. `Scene` is a collection of Vec/Arc handles:
        // moving it through the bounded channel does not copy their backing
        // data, while boxing it allocated and freed one heap object per frame.
        scene: Scene,
        width: u32,
        height: u32,
        base_color: Color,
        /// Physical-pixel rectangles. Empty means the retained target is
        /// already current and only needs presenting.
        damage_regions: Vec<Rect>,
        /// Resize, relayout, structure, or untracked overlay invalidation.
        full_redraw: bool,
        /// Called with the time this frame, or the first later one, is
        /// presented.
        on_present: Option<PresentHook>,
    },
    /// Drain the channel and exit the worker loop. Sent from
    /// `AppHandler::exiting`; the App then `join()`s the handle.
    Shutdown { keep_renderer: bool },
}

/// A device and a renderer whose pipelines are compiled for it, with no
/// surface yet.
#[cfg_attr(
    not(feature = "native"),
    allow(
        dead_code,
        reason = "only the native event-loop runner takes a renderer back to reuse"
    )
)]
pub(crate) struct PreparedRenderer {
    pub(crate) context: RenderContext,
    pub(crate) device_id: usize,
    pub(crate) renderer: Renderer,
}

impl PreparedRenderer {
    #[cfg(feature = "native")]
    pub(crate) fn device_lost(&self) -> bool {
        self.context
            .devices
            .get(self.device_id)
            .is_none_or(|device| device.loss_monitor().reason().is_some())
    }
}

#[path = "render_retry.rs"]
pub(crate) mod retry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameOutcome {
    Presented,
    /// Surface was outdated or suboptimal; worker reconfigured it and
    /// skipped the frame. Main should redraw.
    Reconfigured,
    /// Acquisition explicitly reported occlusion; retry with bounded backoff.
    Occluded,
    /// Acquisition timed out or encoding failed; retry at the normal cadence.
    Skipped,
    /// No retry will help; the worker stops after emitting this and main
    /// treats it as "close the window". Ordinary end of session, not a
    /// fault — the compositor exiting destroys the surface mid-frame, and
    /// wgpu's uncaptured-validation-error handler is `panic!`, which under
    /// `panic = "abort"` killed the process. This variant avoids that.
    SurfaceGone,
}

#[cfg_attr(
    not(feature = "native"),
    allow(
        dead_code,
        reason = "some fields are read only by the native event-loop runner"
    )
)]
pub struct RenderThreadHandle {
    pub render_tx: SyncSender<RenderRequest>,
    pub(crate) buffers: BufferPool<(Scene, Vec<Rect>)>,
    pub frame_complete: Receiver<FrameOutcome>,
    pub(crate) retry: retry::RenderRetryState,
    join: Option<JoinHandle<Option<PreparedRenderer>>>,
}

/// One atomic reservation preserves both buffers across an aborted redraw.
#[cfg_attr(
    not(feature = "native"),
    allow(
        dead_code,
        reason = "some fields are read only by the native event-loop runner"
    )
)]
pub(crate) struct BufferPool<T> {
    receiver: Receiver<T>,
    prepared: Option<T>,
    waiting: Arc<AtomicBool>,
}

impl<T> BufferPool<T> {
    fn new(receiver: Receiver<T>, waiting: Arc<AtomicBool>) -> Self {
        Self {
            receiver,
            prepared: None,
            waiting,
        }
    }

    #[cfg(feature = "native")]
    pub(crate) fn redraw_deferred(&self) -> bool {
        self.waiting.load(Ordering::SeqCst)
    }

    #[cfg(feature = "native")]
    pub(crate) fn prepare(&mut self) -> Result<bool, mpsc::TryRecvError> {
        if self.prepared.is_some() {
            return Ok(true);
        }
        // Arm before checking: a completion between this check and event-loop
        // sleep must wake the window. At worst a race requests one extra redraw.
        self.waiting.store(true, Ordering::SeqCst);
        match self.receiver.try_recv() {
            Ok(buffers) => {
                self.prepared = Some(buffers);
                self.waiting.store(false, Ordering::SeqCst);
                Ok(true)
            }
            Err(mpsc::TryRecvError::Empty) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Put a reserved buffer back when this redraw will not present.
    /// Dropping it would shrink the pool until the worker had nothing to return.
    #[cfg(feature = "native")]
    pub(crate) fn unprepare(&mut self, buffers: T) {
        self.prepared = Some(buffers);
    }

    #[cfg(feature = "native")]
    pub(crate) fn recv(&mut self) -> Result<T, mpsc::RecvError> {
        if let Some(buffers) = self.prepared.take() {
            Ok(buffers)
        } else {
            self.receiver.recv()
        }
    }

    #[cfg(feature = "native")]
    pub(crate) fn try_recv(&mut self) -> Result<T, mpsc::TryRecvError> {
        if self.prepare()? {
            self.prepared.take().ok_or(mpsc::TryRecvError::Empty)
        } else {
            Err(mpsc::TryRecvError::Empty)
        }
    }
}

/// Do not consume a forced probe or dirty work until both pooled buffers exist.
#[cfg(feature = "native")]
pub(crate) fn prepare_frame<T>(
    retry: &mut retry::RenderRetryState,
    buffers: &mut BufferPool<T>,
    now: crate::time::Instant,
) -> Result<bool, mpsc::TryRecvError> {
    if !retry.allows(now) || !buffers.prepare()? {
        return Ok(false);
    }
    retry.admitted();
    Ok(true)
}

fn notify_buffer_waiter(waiting: &AtomicBool, wake: &dyn Fn()) {
    if waiting.swap(false, Ordering::SeqCst) {
        wake();
    }
}

fn notify_render_completion(waiting: &AtomicBool, wake: &dyn Fn(), outcome: FrameOutcome) {
    let deferred = waiting.swap(false, Ordering::SeqCst);
    if deferred || outcome != FrameOutcome::Presented {
        wake();
    }
}

struct DamageTarget {
    width: u32,
    height: u32,
    texture: vello::wgpu::Texture,
    view: vello::wgpu::TextureView,
}

#[derive(Default)]
struct DamageCache {
    targets: Vec<DamageTarget>,
    last_region_count: Option<usize>,
    geometric_capacity: bool,
    coarse_pool_high_water_pixels: u64,
}

impl RenderThreadHandle {
    /// Idempotent: a second call is a no-op (the `JoinHandle` is taken on
    /// the first). The worker drops its `RenderSurface` before this
    /// returns — it must die before the `Window` it was created against,
    /// or wgpu may panic on Vulkan backends.
    pub fn shutdown_and_join(&mut self) {
        let _ = self.stop(false);
    }

    /// [`Self::shutdown_and_join`], taking back the worker's context and
    /// renderer if it was still running.
    #[cfg(feature = "native")]
    pub(crate) fn shutdown_keeping_renderer(&mut self) -> Option<PreparedRenderer> {
        self.stop(true)
    }

    fn stop(&mut self, keep_renderer: bool) -> Option<PreparedRenderer> {
        // Best-effort: the worker may already have crashed and dropped the
        // receiver, in which case the send errors and join yields immediately.
        let _ = self
            .render_tx
            .send(RenderRequest::Shutdown { keep_renderer });
        self.join.take()?.join().ok().flatten()
    }
}

/// Channel capacities are documented at the module level.
///
/// # Errors
///
/// Returns an error if the OS refuses to spawn the `ui-render` thread (e.g.
/// the process is out of resources for new threads).
#[allow(
    clippy::too_many_lines,
    reason = "keep the single GPU owner's resize/render/trace/recycle/shutdown sequence and resource drop order visible together"
)]
pub fn spawn_render_thread(
    render_cx: RenderContext,
    mut surface: RenderSurface<'static>,
    mut renderer: Renderer,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> Result<RenderThreadHandle, String> {
    let (req_tx, rx) = mpsc::sync_channel::<RenderRequest>(1);
    let (pool_tx, pool_rx) = mpsc::sync_channel(2);
    let (outcome_tx, frame_complete) = mpsc::sync_channel::<FrameOutcome>(8);
    let waiting = Arc::new(AtomicBool::new(false));
    let buffers = BufferPool::new(pool_rx, waiting.clone());
    let _ = pool_tx.send((Scene::new(), Vec::with_capacity(8)));
    let _ = pool_tx.send((Scene::new(), Vec::with_capacity(8)));
    let join = std::thread::Builder::new()
        .name("ui-render".into())
        .spawn(move || {
            tilo_startup_metrics::mark("render-worker-start");
            tilo_rt::mark_ui_thread();
            // `TILO_UI_PRESENT=1` logs per-frame encode/acquire/present timings
            // — a slow compositor/present path shows up as a large acquire or
            // present, invisible to offscreen benches. Off by default.
            let present_dbg = crate::env::var("TILO_UI_PRESENT").is_ok();
            let present_origin = present_dbg.then(crate::time::now);
            let mut frame_ix: u64 = 0;
            let presentation = crate::presentation_feedback::Observer::from_env();
            let native_timing = presentation.as_ref().filter(|_| timing::enabled());
            if let Some(observer) = native_timing {
                observer.log_native_config(&surface);
            }
            let mut first_present_recorded = false;
            let mut present_hooks: Vec<PresentHook> = Vec::new();
            let geometric_capacity = damage_geometric_capacity_enabled();
            let mut damage_cache = DamageCache {
                geometric_capacity,
                ..DamageCache::default()
            };
            let mut target_valid = false;
            let mut handoff = native_timing.map(|_| timing::handoff::Recorder::default());
            let mut keep_renderer = false;
            loop {
                let (message, receive_bounds) =
                    timing::around(native_timing.is_some(), || rx.recv());
                let before_receive = receive_bounds.and_then(|bounds| bounds.before);
                let received = receive_bounds.and_then(|bounds| bounds.after);
                let Ok(msg) = message else { break };
                if frame_ix == 0 {
                    tilo_startup_metrics::mark("render-worker-first-frame-received");
                }
                let (
                    mut scene,
                    width,
                    height,
                    base_color,
                    mut damage_regions,
                    full_redraw,
                    cpu_frame_id,
                ) = match msg {
                    RenderRequest::Frame {
                        scene,
                        width,
                        height,
                        base_color,
                        damage_regions,
                        full_redraw,
                        cpu_frame_id,
                        on_present,
                    } => {
                        present_hooks.extend(on_present);
                        (
                            scene,
                            width,
                            height,
                            base_color,
                            damage_regions,
                            full_redraw,
                            cpu_frame_id,
                        )
                    }
                    RenderRequest::Shutdown {
                        keep_renderer: keep,
                    } => {
                        keep_renderer = keep;
                        break;
                    }
                };
                let worker_timing = present_origin.map(|origin| (origin, crate::time::now()));
                if let Some(observer) = native_timing {
                    timing::handoff::received(
                        &mut handoff,
                        frame_ix,
                        cpu_frame_id,
                        before_receive,
                        received,
                        |row| observer.log_native_handoff(row),
                    );
                }

                // Resize on the worker so main never touches wgpu state after
                // spawn — simpler than an explicit Resize variant, and keeps
                // every wgpu call on the thread that owns the surface.
                if surface.config.width != width || surface.config.height != height {
                    if let Err(err) =
                        resize_surface_checked(&render_cx, &mut surface, width, height)
                    {
                        // Re-attempting the same reconfigure on a destroyed
                        // surface fails identically forever.
                        tracing::info!(%err, "drawing surface is gone; stopping the render thread");
                        log_render_outcome(
                            frame_ix,
                            cpu_frame_id,
                            (width, height),
                            FrameOutcome::SurfaceGone,
                            worker_timing,
                            None,
                        );
                        if let Some(observer) = native_timing {
                            observer.log_native_worker(
                                frame_ix,
                                cpu_frame_id,
                                (received, None, None),
                                FrameOutcome::SurfaceGone,
                            );
                        }
                        let _ = outcome_tx.send(FrameOutcome::SurfaceGone);
                        notify_render_completion(
                            &waiting,
                            wake.as_ref(),
                            FrameOutcome::SurfaceGone,
                        );
                        break;
                    }
                    target_valid = false;
                    damage_cache = DamageCache {
                        geometric_capacity,
                        ..DamageCache::default()
                    };
                    renderer.trim_pooled_buffers();
                    if let Some(observer) = native_timing {
                        observer.log_native_config(&surface);
                    }
                }

                let render_start = present_dbg.then(crate::time::now);
                let render_host_start = native_timing.and_then(|_| timing::capture());
                let outcome = render_one_frame(
                    &render_cx,
                    &surface,
                    &mut renderer,
                    &mut damage_cache,
                    FrameContent {
                        scene: &scene,
                        width,
                        height,
                        base_color,
                        damage_regions: &damage_regions,
                        full_redraw: full_redraw || !target_valid,
                    },
                    FramePresentation {
                        frame_ix,
                        cpu_frame_id,
                        present_dbg,
                        observer: presentation.as_ref(),
                    },
                );
                let render_host_end = native_timing.and_then(|_| timing::capture());
                timing::handoff::set(
                    &mut handoff,
                    timing::handoff::Stage::RenderEnd,
                    render_host_end,
                );
                let render_duration = render_start.map(|start| start.elapsed());
                if let Some(observer) = native_timing {
                    observer.log_native_worker(
                        frame_ix,
                        cpu_frame_id,
                        (received, render_host_start, render_host_end),
                        outcome,
                    );
                }
                timing::handoff::record(&mut handoff, timing::handoff::Stage::AfterNativeWorkerLog);
                log_render_outcome(
                    frame_ix,
                    cpu_frame_id,
                    (width, height),
                    outcome,
                    worker_timing,
                    render_duration,
                );
                timing::handoff::record(&mut handoff, timing::handoff::Stage::AfterOutcomeLog);
                // A per-session frame counter; bounded by wall-clock session
                // length at typical refresh rates, nowhere near `u64::MAX`.
                frame_ix = frame_ix.saturating_add(1);
                target_valid = outcome == FrameOutcome::Presented;
                if target_valid && !present_hooks.is_empty() {
                    let presented = crate::time::wall();
                    for hook in std::mem::take(&mut present_hooks) {
                        hook(presented);
                    }
                }
                if target_valid && !first_present_recorded {
                    first_present_recorded = true;
                    tilo_startup_metrics::mark("first-frame-presented");
                    tilo_startup_metrics::dump();
                }

                // `Scene::reset` clears the encoding without freeing the
                // backing vecs, so main avoids a reallocation on reuse.
                scene.reset();
                damage_regions.clear();
                timing::handoff::record(&mut handoff, timing::handoff::Stage::AfterSceneRecycle);
                // Non-blocking: at most one Scene is outstanding on main (the
                // one being built), so the pool (capacity 2) never fills. If
                // main has shut down and the receiver is gone, dropping is fine.
                let _ = pool_tx.send((scene, damage_regions));
                timing::handoff::record(&mut handoff, timing::handoff::Stage::AfterPoolSend);
                let _ = outcome_tx.send(outcome);
                timing::handoff::record(&mut handoff, timing::handoff::Stage::AfterOutcomeSend);
                notify_render_completion(&waiting, wake.as_ref(), outcome);
                timing::handoff::record(
                    &mut handoff,
                    timing::handoff::Stage::AfterCompletionNotify,
                );
                if outcome == FrameOutcome::SurfaceGone {
                    break;
                }
            }
            // Publish disconnection before waking a deferred redraw, including
            // the early resize-failure path that cannot recycle its scene.
            drop(pool_tx);
            notify_buffer_waiter(&waiting, wake.as_ref());
            // The surface goes first: it releases its swapchain on the
            // instance `render_cx` owns.
            let device_id = surface.dev_id;
            drop(damage_cache);
            drop(surface);
            if keep_renderer {
                Some(PreparedRenderer {
                    context: render_cx,
                    device_id,
                    renderer,
                })
            } else {
                drop(renderer);
                drop(render_cx);
                None
            }
        })
        .map_err(|e| format!("could not spawn the ui-render thread: {e}"))?;
    Ok(RenderThreadHandle {
        render_tx: req_tx,
        buffers,
        frame_complete,
        retry: retry::RenderRetryState::default(),
        join: Some(join),
    })
}

/// Opt-in worker timing for every consumed frame, including early exits that
/// never reach the existing `present-frame` diagnostic. Times are relative to
/// this worker's origin, not the main-thread profiler or another process.
/// `total_us` excludes resize and is null when resize failed before rendering.
/// These are CPU wall times, not GPU completion or display presentation times;
/// worker bounds exclude channel waiting, scene recycling, and this log write.
fn log_render_outcome(
    frame_ix: u64,
    cpu_frame_id: Option<u64>,
    dimensions: (u32, u32),
    outcome: FrameOutcome,
    worker_timing: Option<(crate::time::Instant, crate::time::Instant)>,
    render_duration: Option<crate::time::Duration>,
) {
    let Some((origin, worker_start)) = worker_timing else {
        return;
    };
    let worker_end = crate::time::now();
    let row = render_outcome_row(
        frame_ix,
        cpu_frame_id,
        dimensions,
        outcome,
        worker_start.duration_since(origin).as_micros(),
        worker_end.duration_since(origin).as_micros(),
        render_duration.map(|duration| duration.as_micros()),
    );
    eprintln!("tilo-render-frame {row}");
}

fn render_outcome_row(
    frame_ix: u64,
    cpu_frame_id: Option<u64>,
    dimensions: (u32, u32),
    outcome: FrameOutcome,
    start_us: u128,
    end_us: u128,
    total_us: Option<u128>,
) -> serde_json::Value {
    serde_json::json!({
        "frame_ix": frame_ix,
        "cpu_frame_id": cpu_frame_id,
        "start_us": start_us,
        "end_us": end_us,
        "total_us": total_us,
        "outcome": match outcome {
            FrameOutcome::Presented => "Presented",
            FrameOutcome::Reconfigured => "Reconfigured",
            FrameOutcome::Skipped => "Skipped",
            FrameOutcome::Occluded => "Occluded",
            FrameOutcome::SurfaceGone => "SurfaceGone",
        },
        "width": dimensions.0,
        "height": dimensions.1,
    })
}

#[cfg(test)]
mod correlation_tests {
    #[test]
    fn worker_outcomes_keep_cpu_identity_independent_of_worker_counter() {
        for outcome in [
            super::FrameOutcome::Presented,
            super::FrameOutcome::Skipped,
            super::FrameOutcome::Reconfigured,
            super::FrameOutcome::SurfaceGone,
        ] {
            let row = super::render_outcome_row(19, Some(7), (800, 600), outcome, 10, 20, None);
            assert_eq!(row["frame_ix"], 19);
            assert_eq!(row["cpu_frame_id"], 7);
        }
        let row = super::render_outcome_row(
            19,
            None,
            (800, 600),
            super::FrameOutcome::Skipped,
            10,
            20,
            None,
        );
        assert!(
            row["cpu_frame_id"].is_null(),
            "auxiliary frames have no CPU profile sample"
        );
    }
}

/// `Surface::configure` returns no `Result` — failures go to the device's
/// error sink and panic with no matching error scope. Pushing the scope
/// here, around this one call, keeps the catch narrow: every other wgpu
/// misuse still panics.
///
/// The failure reads `Surface does not support the adapter's queue family`,
/// which is misleading: wgpu-core reports `UnsupportedQueueFamily`
/// whenever `get_capabilities` errors, and the Vulkan backend returns that
/// for `ERROR_SURFACE_LOST_KHR` too — the actual case here.
fn configure_surface_checked(
    render_cx: &RenderContext,
    surface: &RenderSurface<'static>,
) -> Result<(), String> {
    let Some(device_handle) = render_cx.devices.get(surface.dev_id) else {
        return Err("render device index out of range".to_string());
    };
    let scope = device_handle
        .device
        .push_error_scope(vello::wgpu::ErrorFilter::Validation);
    render_cx.configure_surface(surface);
    match pollster::block_on(scope.pop()) {
        Some(err) => Err(err.to_string()),
        None => Ok(()),
    }
}

/// [`configure_surface_checked`] for the resize path, which reconfigures after
/// swapping the intermediate targets.
fn resize_surface_checked(
    render_cx: &RenderContext,
    surface: &mut RenderSurface<'static>,
    width: u32,
    height: u32,
) -> Result<(), String> {
    let Some(device_handle) = render_cx.devices.get(surface.dev_id) else {
        return Err("render device index out of range".to_string());
    };
    let scope = device_handle
        .device
        .push_error_scope(vello::wgpu::ErrorFilter::Validation);
    render_cx.resize_surface(surface, width, height);
    match pollster::block_on(scope.pop()) {
        Some(err) => Err(err.to_string()),
        None => Ok(()),
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::expect_used,
    clippy::too_many_lines,
    reason = "damage bounds are clamped to the u32 surface before narrowing; one bounded GPU transaction keeps retained-target correctness visible in one place"
)]
fn rasterize_damage(
    gpu: &crate::gpu::DeviceHandle,
    surface: &RenderSurface<'static>,
    renderer: &mut Renderer,
    damage_cache: &mut DamageCache,
    frame: &FrameContent<'_>,
) -> Result<u64, vello::Error> {
    let crate::gpu::DeviceHandle { device, queue, .. } = gpu;
    let FrameContent {
        scene,
        width,
        height,
        base_color,
        damage_regions,
        full_redraw,
    } = *frame;
    let full_pixels = u64::from(width).saturating_mul(u64::from(height));
    let render_full = |renderer: &mut Renderer| {
        renderer.render_to_texture(
            device,
            queue,
            scene,
            &surface.target_view,
            &RenderParams {
                base_color,
                width,
                height,
                antialiasing_method: vello::AaConfig::Area,
            },
        )
    };
    if full_redraw {
        if damage_cache
            .last_region_count
            .is_some_and(|previous| previous != 0)
        {
            renderer.trim_pooled_buffers();
        }
        while let Some(target) = damage_cache.targets.pop() {
            renderer.invalidate_texture_bindings(&target.view);
        }
        damage_cache.last_region_count = Some(0);
        damage_cache.coarse_pool_high_water_pixels = full_pixels;
        render_full(renderer)?;
        return Ok(full_pixels);
    }
    if damage_regions.is_empty() {
        return Ok(0);
    }

    let mut damage_pixels = 0u64;
    let mut damage_region_count = 0usize;
    for rect in damage_regions {
        let x0 = rect.x0.floor().max(0.0).min(f64::from(width)) as u32;
        let y0 = rect.y0.floor().max(0.0).min(f64::from(height)) as u32;
        let x1 = rect.x1.ceil().max(0.0).min(f64::from(width)) as u32;
        let y1 = rect.y1.ceil().max(0.0).min(f64::from(height)) as u32;
        let region_width = x1.saturating_sub(x0);
        let region_height = y1.saturating_sub(y0);
        damage_pixels = damage_pixels
            .saturating_add(u64::from(region_width).saturating_mul(u64::from(region_height)));
        damage_region_count = damage_region_count
            .saturating_add(usize::from(region_width != 0 && region_height != 0));
    }
    // A large union costs less as one Vello pass and must not retain a second
    // near-window-sized texture. This is both the time and memory bound.
    if damage_pixels == 0 || damage_pixels >= full_pixels.saturating_div(2) {
        if damage_cache
            .last_region_count
            .is_some_and(|previous| previous != 0)
        {
            renderer.trim_pooled_buffers();
        }
        while let Some(target) = damage_cache.targets.pop() {
            renderer.invalidate_texture_bindings(&target.view);
        }
        damage_cache.last_region_count = Some(0);
        damage_cache.coarse_pool_high_water_pixels = full_pixels;
        render_full(renderer)?;
        return Ok(full_pixels);
    }

    let regions = coalesce_regions(
        damage_regions
            .iter()
            .filter_map(|rect| {
                let x0 = rect.x0.floor().max(0.0).min(f64::from(width)) as u32;
                let y0 = rect.y0.floor().max(0.0).min(f64::from(height)) as u32;
                let x1 = rect.x1.ceil().max(0.0).min(f64::from(width)) as u32;
                let y1 = rect.y1.ceil().max(0.0).min(f64::from(height)) as u32;
                let region_width = x1.saturating_sub(x0);
                let region_height = y1.saturating_sub(y0);
                (region_width != 0 && region_height != 0).then_some(Region {
                    x: x0,
                    y: y0,
                    width: region_width,
                    height: region_height,
                })
            })
            .collect(),
    );
    let prefix_width = regions
        .iter()
        .map(|r| r.x.saturating_add(r.width))
        .max()
        .unwrap_or(0);
    let prefix_height = regions
        .iter()
        .map(|r| r.y.saturating_add(r.height))
        .max()
        .unwrap_or(0);
    let raster_pixels = u64::from(prefix_width).saturating_mul(u64::from(prefix_height));
    if renderer.sparse_fine_supported() {
        if damage_cache.coarse_pool_high_water_pixels >= full_pixels.saturating_div(2)
            && raster_pixels.saturating_mul(4) <= damage_cache.coarse_pool_high_water_pixels
        {
            renderer.trim_pooled_buffers();
            damage_cache.coarse_pool_high_water_pixels = 0;
        }
        let fine_damage: Vec<FineDamageRect> = regions
            .iter()
            .map(|region| FineDamageRect {
                x: region.x,
                y: region.y,
                width: region.width,
                height: region.height,
            })
            .collect();
        if let Some(stats) = renderer.render_to_texture_with_damage(
            device,
            queue,
            scene,
            &surface.target_view,
            &RenderParams {
                base_color,
                width: prefix_width,
                height: prefix_height,
                antialiasing_method: vello::AaConfig::Area,
            },
            &fine_damage,
        )? {
            while let Some(target) = damage_cache.targets.pop() {
                renderer.invalidate_texture_bindings(&target.view);
            }
            damage_cache.last_region_count = Some(damage_region_count);
            damage_cache.coarse_pool_high_water_pixels = damage_cache
                .coarse_pool_high_water_pixels
                .max(stats.coarse_pixels);
            return Ok(stats.fine_pixels);
        }
    }
    if raster_pixels >= full_pixels.saturating_div(2) {
        if damage_cache
            .last_region_count
            .is_some_and(|previous| previous != 0)
        {
            renderer.trim_pooled_buffers();
        }
        while let Some(target) = damage_cache.targets.pop() {
            renderer.invalidate_texture_bindings(&target.view);
        }
        damage_cache.last_region_count = Some(0);
        damage_cache.coarse_pool_high_water_pixels = full_pixels;
        render_full(renderer)?;
        return Ok(full_pixels);
    }
    // Damage topology is volatile; preserve warm partial pools, but discard
    // full-target scratch before switching to the smaller prefix target.
    if damage_cache.last_region_count == Some(0) {
        renderer.trim_pooled_buffers();
    }
    damage_cache.last_region_count = Some(damage_region_count);
    damage_cache.coarse_pool_high_water_pixels = damage_cache
        .coarse_pool_high_water_pixels
        .max(raster_pixels);
    let texture_limit = device.limits().max_texture_dimension_2d;
    let allocated_width = if damage_cache.geometric_capacity {
        damage_texture_geometric_width_extent(prefix_width, width, texture_limit)
    } else {
        damage_texture_extent(prefix_width, texture_limit)
    };
    let allocated_height = damage_texture_height_extent(prefix_height, texture_limit);
    let recreate = damage_cache.targets.first().is_none_or(|target| {
        if damage_cache.geometric_capacity {
            damage_texture_geometric_needs_replacement(
                target.width,
                target.height,
                prefix_width,
                prefix_height,
                allocated_width,
                allocated_height,
            )
        } else {
            damage_texture_needs_replacement(
                target.width,
                target.height,
                prefix_width,
                prefix_height,
            )
        }
    });
    if recreate {
        if let Some(target) = damage_cache.targets.first() {
            renderer.invalidate_texture_bindings(&target.view);
        }
        let texture = device.create_texture(&vello::wgpu::TextureDescriptor {
            label: Some("ui-native-damage-target"),
            size: vello::wgpu::Extent3d {
                width: allocated_width,
                height: allocated_height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: vello::wgpu::TextureDimension::D2,
            format: vello::wgpu::TextureFormat::Rgba8Unorm,
            usage: vello::wgpu::TextureUsages::STORAGE_BINDING
                | vello::wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&vello::wgpu::TextureViewDescriptor::default());
        let replacement = DamageTarget {
            width: allocated_width,
            height: allocated_height,
            texture,
            view,
        };
        if let Some(target) = damage_cache.targets.first_mut() {
            *target = replacement;
        } else {
            damage_cache.targets.push(replacement);
        }
    }
    let target = damage_cache
        .targets
        .first()
        .expect("damage target was inserted above");
    renderer.render_to_texture_with_post_render(
        device,
        queue,
        scene,
        &target.view,
        &RenderParams {
            base_color,
            width: prefix_width,
            height: prefix_height,
            antialiasing_method: vello::AaConfig::Area,
        },
        |encoder| {
            for region in &regions {
                encoder.copy_texture_to_texture(
                    vello::wgpu::TexelCopyTextureInfo {
                        texture: &target.texture,
                        mip_level: 0,
                        origin: vello::wgpu::Origin3d {
                            x: region.x,
                            y: region.y,
                            z: 0,
                        },
                        aspect: vello::wgpu::TextureAspect::All,
                    },
                    vello::wgpu::TexelCopyTextureInfo {
                        texture: &surface.target_texture,
                        mip_level: 0,
                        origin: vello::wgpu::Origin3d {
                            x: region.x,
                            y: region.y,
                            z: 0,
                        },
                        aspect: vello::wgpu::TextureAspect::All,
                    },
                    vello::wgpu::Extent3d {
                        width: region.width,
                        height: region.height,
                        depth_or_array_layers: 1,
                    },
                );
            }
        },
    )?;
    while damage_cache.targets.len() > 1 {
        let target = damage_cache.targets.pop().expect("length checked above");
        renderer.invalidate_texture_bindings(&target.view);
    }
    Ok(raster_pixels)
}

/// One frame's scene, surface size and damage, as the worker received it.
#[derive(Clone, Copy)]
struct FrameContent<'a> {
    scene: &'a Scene,
    width: u32,
    height: u32,
    base_color: Color,
    damage_regions: &'a [Rect],
    full_redraw: bool,
}

/// How a presented frame is identified and observed.
#[derive(Clone, Copy)]
struct FramePresentation<'a> {
    frame_ix: u64,
    cpu_frame_id: Option<u64>,
    present_dbg: bool,
    observer: Option<&'a crate::presentation_feedback::Observer>,
}

#[allow(
    clippy::too_many_lines,
    reason = "one render-thread frame transaction; splitting it would separate acquire/encode/present state and timings"
)]
fn render_one_frame(
    render_cx: &RenderContext,
    surface: &RenderSurface<'static>,
    renderer: &mut Renderer,
    damage_cache: &mut DamageCache,
    frame: FrameContent<'_>,
    present: FramePresentation<'_>,
) -> FrameOutcome {
    let FrameContent {
        scene,
        width,
        height,
        ..
    } = frame;
    let FramePresentation {
        frame_ix,
        cpu_frame_id,
        present_dbg,
        observer: presentation,
    } = present;
    let Some(device_handle) = render_cx.devices.get(surface.dev_id) else {
        tracing::info!("render device index out of range; stopping the render thread");
        return FrameOutcome::SurfaceGone;
    };
    if frame_ix == 0 {
        tilo_startup_metrics::mark("render-worker-first-acquire-start");
    }

    // Acquire before encoding. An occluded, timed-out, outdated, or destroyed
    // swapchain cannot consume the frame; discovering that only after Vello
    // packed and uploaded the scene wasted the entire GPU pass.
    let t_acquire = present_dbg.then(crate::time::now);
    let surface_texture = match surface.surface.get_current_texture() {
        vello::wgpu::CurrentSurfaceTexture::Success(t) => t,
        vello::wgpu::CurrentSurfaceTexture::Outdated
        | vello::wgpu::CurrentSurfaceTexture::Suboptimal(_) => {
            // Surface drifted (resize, scale change). Reconfigure
            // and ask main to redraw.
            if let Err(err) = configure_surface_checked(render_cx, surface) {
                tracing::info!(%err, "drawing surface is gone; abandoning the frame");
                return FrameOutcome::SurfaceGone;
            }
            if let Some(observer) = presentation.filter(|_| timing::enabled()) {
                observer.log_native_config(surface);
            }
            return FrameOutcome::Reconfigured;
        }
        vello::wgpu::CurrentSurfaceTexture::Occluded => {
            if present_dbg {
                eprintln!("render-skip {frame_ix}: occluded");
            }
            return FrameOutcome::Occluded;
        }
        vello::wgpu::CurrentSurfaceTexture::Timeout => {
            if present_dbg {
                eprintln!("render-skip {frame_ix}: timeout");
            }
            return FrameOutcome::Skipped;
        }
        vello::wgpu::CurrentSurfaceTexture::Lost
        | vello::wgpu::CurrentSurfaceTexture::Validation => {
            // Same shutdown race as the reconfigure above; panicking here
            // used to abort the process for an ordinary end of session.
            tracing::info!("drawing surface is no longer usable; stopping the render thread");
            return FrameOutcome::SurfaceGone;
        }
    };

    let acquire_us = t_acquire.map(|t| t.elapsed().as_secs_f64() * 1e6);
    if frame_ix == 0 {
        tilo_startup_metrics::mark("render-worker-first-acquire-done");
    }

    let t_encode = present_dbg.then(crate::time::now);
    if frame_ix == 0 {
        tilo_startup_metrics::mark("render-worker-first-raster-start");
    }
    let raster_pixels = match rasterize_damage(
        device_handle,
        surface,
        renderer,
        damage_cache,
        &FrameContent {
            full_redraw: frame.full_redraw
                || POST_PROCESSOR_ACTIVE.load(std::sync::atomic::Ordering::Acquire),
            ..frame
        },
    ) {
        Ok(pixels) => pixels,
        Err(err) => {
            // A programmer bug (bad encoding / ran out of GPU resources); log and
            // skip rather than panic, which would tear down the app with no
            // chance for main to clean up.
            tracing::error!(?err, "vello render_to_texture failed");
            return FrameOutcome::Skipped;
        }
    };
    if frame_ix == 0 {
        tilo_startup_metrics::mark("render-worker-first-raster-done");
    }

    // Composite any post-processor content (the browser's zero-copy WebView
    // texture) into the intermediate texture before it goes to the surface.
    if POST_PROCESSOR_ACTIVE.load(std::sync::atomic::Ordering::Acquire)
        && let Ok(mut guard) = POST_PROCESSOR.lock()
        && let Some(p) = guard.as_mut()
    {
        p.post_render(
            &device_handle.device,
            &device_handle.queue,
            &surface.target_view,
            width,
            height,
        );
    }

    let encode_us = t_encode.map(|t| t.elapsed().as_secs_f64() * 1e6);

    let t_submit = present_dbg.then(crate::time::now);
    if frame_ix == 0 {
        tilo_startup_metrics::mark("render-worker-first-blit-start");
    }
    let mut encoder =
        device_handle
            .device
            .create_command_encoder(&vello::wgpu::CommandEncoderDescriptor {
                label: Some("ui-render-thread blit-to-surface"),
            });
    if frame_ix == 0 {
        tilo_startup_metrics::mark("render-worker-first-encoder-done");
    }
    if surface
        .config
        .usage
        .contains(vello::wgpu::TextureUsages::COPY_DST)
    {
        if frame_ix == 0 {
            tilo_startup_metrics::mark("render-worker-first-direct-copy-start");
        }
        encoder.copy_texture_to_texture(
            vello::wgpu::TexelCopyTextureInfo {
                texture: &surface.target_texture,
                mip_level: 0,
                origin: vello::wgpu::Origin3d::ZERO,
                aspect: vello::wgpu::TextureAspect::All,
            },
            vello::wgpu::TexelCopyTextureInfo {
                texture: &surface_texture.texture,
                mip_level: 0,
                origin: vello::wgpu::Origin3d::ZERO,
                aspect: vello::wgpu::TextureAspect::All,
            },
            vello::wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        if frame_ix == 0 {
            tilo_startup_metrics::mark("render-worker-first-direct-copy-done");
        }
    } else {
        surface.blitter.copy(
            &device_handle.device,
            &mut encoder,
            &surface.target_view,
            &surface_texture
                .texture
                .create_view(&vello::wgpu::TextureViewDescriptor::default()),
        );
    }
    if frame_ix == 0 {
        tilo_startup_metrics::mark("render-worker-first-blit-copy-done");
    }
    if frame_ix == 0 && tilo_startup_metrics::enabled() {
        #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
        install_first_frame_sigill_probe();
    }
    let command_buffer = encoder.finish();
    if frame_ix == 0 {
        tilo_startup_metrics::mark("render-worker-first-encoder-finish-done");
    }
    device_handle.queue.submit([command_buffer]);
    if frame_ix == 0 {
        tilo_startup_metrics::mark("render-worker-first-blit-done");
    }

    let submit_us = t_submit.map(|t| t.elapsed().as_secs_f64() * 1e6);

    let t_present = present_dbg.then(crate::time::now);
    if frame_ix == 0 {
        tilo_startup_metrics::mark("render-worker-first-present-start");
    }
    if let Some(observer) = presentation {
        observer.arm(&surface.surface, frame_ix, cpu_frame_id);
    }
    let ((), present_bounds) = timing::around(presentation.is_some() && timing::enabled(), || {
        surface_texture.present();
    });
    if frame_ix == 0 {
        tilo_startup_metrics::mark("render-worker-first-present-done");
    }
    if present_dbg {
        let present_us = t_present.map_or(0.0, |t| t.elapsed().as_secs_f64() * 1e6);
        let encoding = scene.encoding();
        let scene_bytes = std::mem::size_of_val(encoding.path_tags.as_slice())
            .saturating_add(std::mem::size_of_val(encoding.path_data.as_slice()))
            .saturating_add(std::mem::size_of_val(encoding.draw_tags.as_slice()))
            .saturating_add(std::mem::size_of_val(encoding.draw_data.as_slice()))
            .saturating_add(std::mem::size_of_val(encoding.transforms.as_slice()))
            .saturating_add(std::mem::size_of_val(encoding.styles.as_slice()))
            .saturating_add(std::mem::size_of_val(encoding.resources.patches.as_slice()))
            .saturating_add(std::mem::size_of_val(
                encoding.resources.color_stops.as_slice(),
            ))
            .saturating_add(std::mem::size_of_val(encoding.resources.glyphs.as_slice()))
            .saturating_add(std::mem::size_of_val(
                encoding.resources.glyph_runs.as_slice(),
            ))
            .saturating_add(std::mem::size_of_val(
                encoding.resources.normalized_coords.as_slice(),
            ));
        // The present pass samples every RGBA8 pixel from Vello's target into
        // the swapchain. This is GPU-local traffic, not a CPU readback.
        let blit_bytes = u64::from(width)
            .saturating_mul(u64::from(height))
            .saturating_mul(4);
        eprintln!(
            "present-frame {frame_ix}: acquire={:.0}us encode={:.0}us submit={:.0}us present={:.0}us scene={}B paths={} draw_tags={} raster_pixels={} gpu_blit={}B ({}x{})",
            acquire_us.unwrap_or(0.0),
            encode_us.unwrap_or(0.0),
            submit_us.unwrap_or(0.0),
            present_us,
            scene_bytes,
            encoding.n_paths,
            encoding.draw_tags.len(),
            raster_pixels,
            blit_bytes,
            width,
            height,
        );
    }
    if let (Some(observer), Some(bounds)) = (presentation, present_bounds) {
        observer.log_native_present(frame_ix, cpu_frame_id, bounds);
    }
    // `Poll` is the non-blocking variant — nudges the queue without stalling.
    let _ = device_handle.device.poll(vello::wgpu::PollType::Poll);
    if frame_ix == 0 {
        tilo_startup_metrics::mark("render-worker-first-poll-done");
    }
    FrameOutcome::Presented
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    #[test]
    fn occlusion_admission_keeps_payloads_probes_and_sibling_projections() {
        // Case `occluded_primary_keeps_visible_sibling_projection_fresh_through_real_admission`.
        {
            use crate::widget::Widget;
            use automerge::transaction::Transactable;
            // The primary is the sole owner of this retained root's update/sync;
            // auxiliary painting observes its Text projection rather than syncing.
            struct Root {
                text: crate::widget::Text,
                pending: Option<&'static str>,
            }
            impl Widget for Root {
                fn update(
                    &mut self,
                    doc: &mut crate::widget::AppModel,
                    path: &crate::widget::ModelPath,
                ) -> bool {
                    let (doc, path) = crate::doc::document(doc, path);
                    if let Some(value) = self.pending.take() {
                        doc.put(path, "label", value).unwrap();
                        true
                    } else {
                        false
                    }
                }
                fn sync(
                    &mut self,
                    doc: &mut crate::widget::AppModel,
                    path: &crate::widget::ModelPath,
                ) {
                    self.text.sync(doc, path);
                }
                fn build_layout(
                    &mut self,
                    tree: &mut taffy::TaffyTree<()>,
                    theme: &crate::Theme,
                    map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
                ) -> taffy::NodeId {
                    self.text.build_layout(tree, theme, map)
                }
                fn apply_layout(
                    &mut self,
                    tree: &taffy::TaffyTree<()>,
                    node: taffy::NodeId,
                    offset: vello::kurbo::Point,
                ) {
                    self.text.apply_layout(tree, node, offset);
                }
                fn draw(
                    &mut self,
                    scene: &mut Scene,
                    transform: vello::kurbo::Affine,
                    renderer: &mut crate::text_render::TextRenderer,
                    theme: &crate::Theme,
                    presence: &crate::presence::PresenceCtx,
                ) {
                    self.text.draw(scene, transform, renderer, theme, presence);
                }
                fn handle_event(
                    &mut self,
                    event: &winit::event::WindowEvent,
                    ctx: &mut crate::widget::EventCtx<'_>,
                ) {
                    self.text.handle_event(event, ctx);
                }
                fn get_rect(&self) -> Rect {
                    self.text.get_rect()
                }
                fn name(&self) -> &'static str {
                    "shared-projection"
                }
                fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
                    self.text.accessibility()
                }
            }
            let mut root = Root {
                text: crate::widget::Text::from_document_string("label"),
                pending: Some("initial"),
            };
            let mut doc = automerge::AutoCommit::new();
            root.update(&mut doc, &automerge::ROOT);
            root.sync(&mut doc, &automerge::ROOT);
            let now = crate::time::now();
            let mut retry = retry::RenderRetryState::default();
            for _ in 0..8 {
                retry.observe(FrameOutcome::Occluded, now);
            }
            let (tx, rx) = mpsc::sync_channel(2);
            let mut buffers = BufferPool::new(rx, Arc::new(AtomicBool::new(false)));
            for (index, value) in ["animation tick", "auxiliary input"]
                .into_iter()
                .enumerate()
            {
                let tick = now + retry::FRAME_INTERVAL * (index as u32 + 1);
                retry.observe_for_shared_root(
                    FrameOutcome::Occluded,
                    tick - retry::FRAME_INTERVAL,
                    true,
                );
                tx.send((Scene::new(), Vec::<Rect>::new())).unwrap();
                root.pending = Some(value);
                let admitted_at = if index == 1 {
                    assert!(retry.known_occluded());
                    assert!(
                        retry.request_probe(),
                        "auxiliary input also probes the shared primary"
                    );
                    tick - retry::FRAME_INTERVAL
                } else {
                    tick
                };
                if prepare_frame(&mut retry, &mut buffers, admitted_at).unwrap() {
                    assert!(root.update(&mut doc, &automerge::ROOT));
                    root.sync(&mut doc, &automerge::ROOT);
                    let _ = buffers.recv().unwrap();
                }
                assert_eq!(
                    root.accessibility().unwrap().name.as_deref(),
                    Some(value),
                    "visible sibling must observe each shared update, not a 250ms-old projection"
                );
            }
            for _ in 0..8 {
                retry.observe_for_shared_root(FrameOutcome::Occluded, now, true);
            }
            assert!(
                retry.known_occluded(),
                "a sibling must not erase primary occlusion history"
            );
            retry.observe_for_shared_root(FrameOutcome::Occluded, now, false);
            assert_eq!(
                retry.deadline(),
                Some(now + crate::time::Duration::from_millis(250))
            );
        }

        // Case `occlusion_admission_preserves_dirty_payload_and_probe_until_buffers_return`.
        {
            let now = crate::time::now();
            let mut retry = retry::RenderRetryState::default();
            for _ in 0..8 {
                retry.observe(FrameOutcome::Occluded, now);
            }
            let (tx, rx) = mpsc::sync_channel(2);
            let waiting = Arc::new(AtomicBool::new(false));
            let mut pool = BufferPool::new(rx, waiting.clone());
            tx.send(("scene", vec![1, 2, 3])).unwrap();
            assert!(!prepare_frame(&mut retry, &mut pool, now).unwrap());
            assert!(
                !pool.redraw_deferred(),
                "occlusion must not arm a false buffer wait"
            );
            retry.request_probe();
            retry.observe(FrameOutcome::Occluded, now); // Older completion drained after input.
            assert!(prepare_frame(&mut retry, &mut pool, now).unwrap());
            assert_eq!(pool.recv().unwrap(), ("scene", vec![1, 2, 3]));
            retry.observe(FrameOutcome::Occluded, now);
            retry.request_probe();
            assert!(!prepare_frame(&mut retry, &mut pool, now).unwrap());
            assert!(
                retry.allows(now),
                "buffer deferral must preserve explicit probe"
            );
            tx.send(("latest dirty scene", vec![4, 5])).unwrap();
            notify_render_completion(&waiting, &|| {}, FrameOutcome::Occluded);
            retry.observe(FrameOutcome::Occluded, now);
            assert!(prepare_frame(&mut retry, &mut pool, now).unwrap());
            assert_eq!(pool.recv().unwrap(), ("latest dirty scene", vec![4, 5]));
            retry.observe(FrameOutcome::Occluded, now);
            assert_eq!(retry.deadline(), Some(now + retry::FRAME_INTERVAL));
        }

        // Case `admission_defers_without_consuming_partial_state_and_retains_reservation`.
        {
            let (tx, rx) = mpsc::sync_channel(2);
            let waiting = Arc::new(AtomicBool::new(false));
            let mut pool = BufferPool::new(rx, waiting.clone());
            assert!(!pool.prepare().unwrap());
            assert!(waiting.load(Ordering::SeqCst));
            tx.send(41).unwrap();
            assert!(pool.prepare().unwrap());
            assert!(!waiting.load(Ordering::SeqCst));
            // An aborted layout keeps the reservation for the next redraw.
            assert!(pool.prepare().unwrap());
            assert_eq!(pool.recv().unwrap(), 41);
            assert!(!pool.prepare().unwrap());
            drop(tx);
            assert!(pool.prepare().is_err());
        }
    }

    #[test]
    fn completion_and_failure_wake_deferred_work_without_losing_buffers() {
        // Case `failed_outcomes_wake_even_without_deferred_work`.
        {
            use std::cell::Cell;
            let waiting = AtomicBool::new(false);
            let wakes = Cell::new(0);
            let wake = || wakes.set(wakes.get() + 1);
            notify_render_completion(&waiting, &wake, FrameOutcome::Presented);
            assert_eq!(wakes.get(), 0);
            for outcome in [
                FrameOutcome::Skipped,
                FrameOutcome::Reconfigured,
                FrameOutcome::Occluded,
                FrameOutcome::SurfaceGone,
            ] {
                notify_render_completion(&waiting, &wake, outcome);
            }
            assert_eq!(wakes.get(), 4);
            waiting.store(true, Ordering::SeqCst);
            notify_render_completion(&waiting, &wake, FrameOutcome::Presented);
            assert_eq!(wakes.get(), 5);
            assert!(!waiting.load(Ordering::SeqCst));
        }

        // Case `completion_wakes_only_deferred_work_and_cannot_lose_ready_buffers`.
        {
            use std::cell::Cell;
            let (tx, rx) = mpsc::sync_channel(2);
            let waiting = Arc::new(AtomicBool::new(false));
            let mut pool = BufferPool::new(rx, waiting.clone());
            let wakes = Cell::new(0);
            let wake = || wakes.set(wakes.get() + 1);
            notify_buffer_waiter(&waiting, &wake);
            assert_eq!(wakes.get(), 0, "static completions must stay idle");
            assert!(!pool.prepare().unwrap());
            assert!(
                pool.redraw_deferred(),
                "periodic redraw demand must defer while busy"
            );
            tx.send(7).unwrap();
            notify_buffer_waiter(&waiting, &wake);
            notify_buffer_waiter(&waiting, &wake);
            assert_eq!(wakes.get(), 1, "one wake services a deferred redraw");
            assert!(
                !pool.redraw_deferred(),
                "completion re-enables periodic redraws"
            );
            assert!(pool.prepare().unwrap());
            assert_eq!(pool.recv().unwrap(), 7);
            // Completion before admission needs no wake: admission finds the pair.
            tx.send(8).unwrap();
            notify_buffer_waiter(&waiting, &wake);
            assert!(pool.prepare().unwrap());
            assert_eq!(pool.recv().unwrap(), 8);
            assert_eq!(wakes.get(), 1);
            assert!(!pool.prepare().unwrap());
            drop(tx);
            notify_buffer_waiter(&waiting, &wake);
            assert_eq!(wakes.get(), 2, "worker exit wakes a pending admission");
            assert!(pool.prepare().is_err());
        }
    }
}
