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

mod cpu_raster;
mod native_gpu_trace;

use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::JoinHandle;

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
        /// The backdrops the scene's floating surfaces were recorded over
        /// (`crate::backdrop`), in physical pixels. Empty draws no blur.
        backdrop: crate::backdrop::BackdropFrame,
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
/// Why the drawing surface was lost, kept for the window that stops on it.
static SURFACE_LOSS: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Record why the drawing surface was lost; the first reason is the cause.
fn note_surface_loss(reason: String) {
    let mut slot = SURFACE_LOSS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    slot.get_or_insert(reason);
}

/// The reason recorded by the render worker when it lost its surface.
#[cfg(feature = "native")]
pub(crate) fn take_surface_loss() -> Option<String> {
    SURFACE_LOSS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
}

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

#[path = "render_bump_recovery.rs"]
mod bump_recovery;

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
    /// Set by the worker when presented pixels lost geometry to a Vello bump
    /// overflow; the next frame main submits must repaint everything.
    full_repaint: Arc<AtomicBool>,
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
    /// Whether the worker asked for a whole-surface repaint since the last
    /// call. It wakes the window when it asks, so the request does not wait
    /// for an unrelated event.
    #[cfg(feature = "native")]
    pub(crate) fn take_full_repaint(&self) -> bool {
        self.full_repaint.swap(false, Ordering::SeqCst)
    }

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
pub fn spawn_render_thread(
    render_cx: RenderContext,
    surface: RenderSurface<'static>,
    renderer: Renderer,
    wake: Arc<dyn Fn() + Send + Sync>,
) -> Result<RenderThreadHandle, String> {
    spawn_render_thread_for_window(render_cx, surface, renderer, wake, None)
}

/// Same worker with the native window retained for standard Wayland feedback.
#[allow(
    clippy::too_many_lines,
    reason = "keep the single GPU owner's resize/render/trace/recycle/shutdown sequence and resource drop order visible together"
)]
pub(crate) fn spawn_render_thread_for_window(
    render_cx: RenderContext,
    mut surface: RenderSurface<'static>,
    mut renderer: Renderer,
    wake: Arc<dyn Fn() + Send + Sync>,
    window: Option<Arc<winit::window::Window>>,
) -> Result<RenderThreadHandle, String> {
    let (req_tx, rx) = mpsc::sync_channel::<RenderRequest>(1);
    let (pool_tx, pool_rx) = mpsc::sync_channel(2);
    let (outcome_tx, frame_complete) = mpsc::sync_channel::<FrameOutcome>(8);
    let waiting = Arc::new(AtomicBool::new(false));
    let full_repaint = Arc::new(AtomicBool::new(false));
    let worker_full_repaint = full_repaint.clone();
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
            let presentation = crate::presentation_feedback::Observer::from_env(window);
            let mut gpu_trace = presentation
                .as_ref()
                .and_then(crate::presentation_feedback::Observer::worker_id)
                .and_then(native_gpu_trace::Observer::from_environment);
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
            let mut cpu_raster = (render_cx.devices[surface.dev_id]
                .adapter
                .get_info()
                .device_type
                == vello::wgpu::DeviceType::Cpu)
                .then(cpu_raster::CpuCanvas::new);
            if cpu_raster.is_some() {
                tracing::info!("tilo-ui: CPU fine rasterization selected for software adapter");
            }
            let mut target_valid = false;
            let mut bump_recovery = bump_recovery::BumpRecovery::default();
            let mut handoff = native_timing.map(|_| timing::handoff::Recorder::default());
            let mut keep_renderer = false;
            // Built with the first frame that draws a floating surface over
            // the blurred material.
            let mut backdrop_blur: Option<crate::backdrop::BackdropBlur> = None;
            let mut had_backdrop = false;
            loop {
                let gpu_poll = gpu_trace
                    .as_ref()
                    .is_some_and(native_gpu_trace::Observer::poll_pending);
                let wait = match (bump_recovery.wait(), gpu_poll) {
                    (Some(wait), true) => Some(wait.min(std::time::Duration::from_millis(5))),
                    (None, true) => Some(std::time::Duration::from_millis(5)),
                    (wait, false) => wait,
                };
                let (message, receive_bounds) =
                    timing::around(native_timing.is_some(), || match wait {
                        Some(timeout) => rx.recv_timeout(timeout),
                        None => rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected),
                    });
                let before_receive = receive_bounds.and_then(|bounds| bounds.before);
                let received = receive_bounds.and_then(|bounds| bounds.after);
                let msg = match message {
                    Ok(msg) => msg,
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if gpu_poll && let Some(observer) = gpu_trace.as_mut() {
                            if let Some(device) = render_cx.devices.get(surface.dev_id) {
                                let _ = device.device.poll(vello::wgpu::PollType::Poll);
                            }
                            observer.idle_poll(&mut renderer);
                        }
                        if poll_bump_recovery(
                            &mut bump_recovery,
                            &mut renderer,
                            &render_cx,
                            &surface,
                        ) {
                            request_full_repaint(
                                &mut target_valid,
                                &worker_full_repaint,
                                wake.as_ref(),
                            );
                        }
                        continue;
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                };
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
                    backdrop,
                ) = match msg {
                    RenderRequest::Frame {
                        scene,
                        width,
                        height,
                        base_color,
                        damage_regions,
                        full_redraw,
                        cpu_frame_id,
                        backdrop,
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
                            backdrop,
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
                        tracing::warn!(%err, "drawing surface is gone; stopping the render thread");
                        note_surface_loss(format!(
                            "its drawing surface could not be resized: {err}"
                        ));
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

                let full_redraw = full_redraw || !target_valid;
                if full_redraw {
                    // Any whole-surface frame answers a pending overflow repaint.
                    worker_full_repaint.store(false, Ordering::SeqCst);
                }
                if let Some(observer) = gpu_trace.as_mut() {
                    observer.observe(&mut renderer, None);
                }
                let render_start = present_dbg.then(crate::time::now);
                let render_host_start = native_timing.and_then(|_| timing::capture());
                let outcome = render_one_frame(
                    &render_cx,
                    &surface,
                    FrameRasterizers {
                        renderer: &mut renderer,
                        damage_cache: &mut damage_cache,
                        backdrop_blur: &mut backdrop_blur,
                        cpu: cpu_raster.as_mut(),
                    },
                    FrameContent {
                        scene: &scene,
                        width,
                        height,
                        base_color,
                        damage_regions: &damage_regions,
                        // A blurred surface samples everything under it, and
                        // the frame after one repaints what it covered.
                        full_redraw: full_redraw
                            || !target_valid
                            || !backdrop.is_empty()
                            || had_backdrop,
                        backdrop: &backdrop,
                    },
                    FramePresentation {
                        frame_ix,
                        cpu_frame_id,
                        present_dbg,
                        observer: presentation.as_ref(),
                    },
                );
                let render_host_end = native_timing.and_then(|_| timing::capture());
                if let Some(observer) = gpu_trace.as_mut() {
                    observer.frame(&mut renderer, frame_ix, cpu_frame_id);
                }
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
                had_backdrop = !backdrop.is_empty();
                drop(backdrop);
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
                    if let Some(line) = tilo_startup_metrics::summary_line(&startup_app_name()) {
                        crate::diag!("{line}");
                    }
                    tilo_startup_metrics::close_startup();
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
                    // The window's surface was destroyed under the app (the
                    // compositor closed it, or the device was lost): say so,
                    // since the app exits on it.
                    tracing::warn!(
                        "drawing surface is gone after a frame; stopping the render thread"
                    );
                    break;
                }
                // After the outcome is published, so main's per-frame
                // accounting is untouched: a repaint is a later, ordinary frame.
                if poll_bump_recovery(&mut bump_recovery, &mut renderer, &render_cx, &surface) {
                    request_full_repaint(&mut target_valid, &worker_full_repaint, wake.as_ref());
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
        full_repaint,
        join: Some(join),
    })
}

/// Collect finished bump counters; `true` when presented pixels lost geometry.
fn poll_bump_recovery(
    recovery: &mut bump_recovery::BumpRecovery,
    renderer: &mut Renderer,
    render_cx: &RenderContext,
    surface: &RenderSurface<'static>,
) -> bool {
    let Some(device_handle) = render_cx.devices.get(surface.dev_id) else {
        return false;
    };
    recovery.poll(renderer, &device_handle.device) == bump_recovery::BumpStep::Repaint
}

/// The retained target is no longer a faithful frame: the next one repaints
/// everything, and main is woken to submit it even if nothing else changed.
fn request_full_repaint(target_valid: &mut bool, full_repaint: &AtomicBool, wake: &dyn Fn()) {
    *target_valid = false;
    full_repaint.store(true, Ordering::SeqCst);
    wake();
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

/// The retained frame the native host rasterizes into and presents from.
/// Pixels outside a frame's damage are whatever the previous frame left, so
/// it must only ever be written through [`rasterize_damage`].
#[derive(Clone, Copy)]
struct RetainedTarget<'a> {
    texture: &'a vello::wgpu::Texture,
    view: &'a vello::wgpu::TextureView,
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::expect_used,
    clippy::too_many_lines,
    reason = "damage bounds are clamped to the u32 surface before narrowing; one bounded GPU transaction keeps retained-target correctness visible in one place"
)]
fn rasterize_damage(
    (device, queue): (&vello::wgpu::Device, &vello::wgpu::Queue),
    retained: RetainedTarget<'_>,
    renderer: &mut Renderer,
    damage_cache: &mut DamageCache,
    frame: &FrameContent<'_>,
) -> Result<u64, vello::Error> {
    let FrameContent {
        scene,
        width,
        height,
        base_color,
        damage_regions,
        full_redraw,
        ..
    } = *frame;
    let full_pixels = u64::from(width).saturating_mul(u64::from(height));
    let render_full = |renderer: &mut Renderer| {
        renderer.render_to_texture(
            device,
            queue,
            scene,
            retained.view,
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
            retained.view,
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
                        texture: retained.texture,
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
    backdrop: &'a crate::backdrop::BackdropFrame,
}

/// How a presented frame is identified and observed.
#[derive(Clone, Copy)]
struct FramePresentation<'a> {
    frame_ix: u64,
    cpu_frame_id: Option<u64>,
    present_dbg: bool,
    observer: Option<&'a crate::presentation_feedback::Observer>,
}

struct FrameRasterizers<'a> {
    renderer: &'a mut Renderer,
    damage_cache: &'a mut DamageCache,
    backdrop_blur: &'a mut Option<crate::backdrop::BackdropBlur>,
    cpu: Option<&'a mut cpu_raster::CpuCanvas>,
}

#[allow(
    clippy::too_many_lines,
    reason = "one render-thread frame transaction; splitting it would separate acquire/encode/present state and timings"
)]
fn render_one_frame(
    render_cx: &RenderContext,
    surface: &RenderSurface<'static>,
    rasterizers: FrameRasterizers<'_>,
    frame: FrameContent<'_>,
    present: FramePresentation<'_>,
) -> FrameOutcome {
    let FrameRasterizers {
        renderer,
        damage_cache,
        backdrop_blur,
        mut cpu,
    } = rasterizers;
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
        note_surface_loss("its render device is gone".to_owned());
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
                note_surface_loss(format!(
                    "its drawing surface could not be reconfigured: {err}"
                ));
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
            note_surface_loss("its drawing surface was lost".to_owned());
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
    // Floating surfaces over the blurred material: render, blur and
    // substitute each one's backdrop before the frame that draws it.
    let backdrop_stats = (!frame.backdrop.is_empty()).then(|| {
        backdrop_blur
            .get_or_insert_with(|| crate::backdrop::BackdropBlur::new(&device_handle.device))
            .prepare(
                &device_handle.device,
                &device_handle.queue,
                renderer,
                frame.backdrop,
                (width, height),
                frame.base_color,
            )
    });
    let raster_frame = FrameContent {
        backdrop: &crate::backdrop::BackdropFrame::default(),
        full_redraw: frame.full_redraw
            || POST_PROCESSOR_ACTIVE.load(std::sync::atomic::Ordering::Acquire),
        ..frame
    };
    let rasterized = if let Some(canvas) = cpu.as_mut().filter(|_| frame.backdrop.is_empty()) {
        canvas.rasterize(&raster_frame).inspect(|_| {
            canvas.upload(&device_handle.queue, &surface.target_texture);
        })
    } else {
        // Backdrop substitutions currently belong to Vello's GPU renderer.
        // Invalidate retained CPU pixels when that path writes the target.
        if let Some(canvas) = cpu.as_mut() {
            canvas.invalidate();
        }
        rasterize_damage(
            (&device_handle.device, &device_handle.queue),
            RetainedTarget {
                texture: &surface.target_texture,
                view: &surface.target_view,
            },
            renderer,
            damage_cache,
            &raster_frame,
        )
        .map_err(|error| error.to_string())
    };
    if let Some(blur) = backdrop_blur.as_mut() {
        blur.release(renderer);
    }
    if present_dbg && let Some(stats) = backdrop_stats {
        eprintln!(
            "present-backdrop {frame_ix}: substituted={} skipped={} encode={}us",
            stats.substituted, stats.skipped, stats.encode_us
        );
    }
    let raster_pixels = match rasterized {
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
    device_handle.queue.submit([encoder.finish()]);
    if frame_ix == 0 {
        tilo_startup_metrics::mark("render-worker-first-blit-done");
    }

    let submit_us = t_submit.map(|t| t.elapsed().as_secs_f64() * 1e6);

    let t_present = present_dbg.then(crate::time::now);
    let damage = bounded_present_damage(width, height, frame.damage_regions).filter(|_| {
        !frame.full_redraw
            && !POST_PROCESSOR_ACTIVE.load(std::sync::atomic::Ordering::Acquire)
            && frame.backdrop.is_empty()
    });
    tilo_presentation::set_next_present_damage(
        &surface.surface,
        damage.as_deref().unwrap_or_default(),
    );
    if frame_ix == 0 {
        tilo_startup_metrics::mark("render-worker-first-present-start");
    }
    if let Some(observer) = presentation {
        observer.arm(&surface.surface, frame_ix, cpu_frame_id);
    }
    let ((), present_bounds) = timing::around(presentation.is_some() && timing::enabled(), || {
        if let Some(observer) = presentation {
            observer.present_wayland(frame_ix, cpu_frame_id, || surface_texture.present());
        } else {
            surface_texture.present();
        }
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

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "finite framebuffer bounds are rounded outward and clamped to their u32 extent before narrowing"
)]
fn bounded_present_damage(
    width: u32,
    height: u32,
    damage: &[Rect],
) -> Option<Vec<tilo_presentation::DamageRect>> {
    if width == 0 || height == 0 || damage.is_empty() || damage.len() > 128 {
        return None;
    }
    let mut regions = Vec::with_capacity(damage.len());
    for rect in damage {
        if !rect.is_finite() || rect.x1 < rect.x0 || rect.y1 < rect.y0 {
            return None;
        }
        let x = rect.x0.floor().clamp(0.0, f64::from(width)) as u32;
        let y = rect.y0.floor().clamp(0.0, f64::from(height)) as u32;
        let x1 = rect.x1.ceil().clamp(0.0, f64::from(width)) as u32;
        let y1 = rect.y1.ceil().clamp(0.0, f64::from(height)) as u32;
        if x1 > x && y1 > y {
            regions.push(Region {
                x,
                y,
                width: x1.saturating_sub(x),
                height: y1.saturating_sub(y),
            });
        }
    }
    if regions.is_empty() {
        return None;
    }
    Some(
        coalesce_regions(regions)
            .into_iter()
            .map(|rect| tilo_presentation::DamageRect {
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height,
            })
            .collect(),
    )
}

#[cfg(test)]
mod present_damage_tests {
    use super::{Rect, bounded_present_damage};

    #[test]
    fn presentation_hints_cover_every_changed_framebuffer_pixel() {
        let changed = [
            Rect::new(-4.5, 1.2, 5.1, 7.8),
            Rect::new(10.7, 12.4, 23.8, 29.2),
            Rect::new(22.6, 28.1, 35.0, 37.0),
        ];
        let hints = bounded_present_damage(32, 32, &changed).expect("bounded damage");
        // Pixel-cell intersection, independently of the outward-rounding
        // implementation: every pixel touched by any submitted damage must
        // be included, including fractional edges and output clipping.
        for y in 0_u32..32 {
            for x in 0_u32..32 {
                let cell = Rect::new(
                    f64::from(x),
                    f64::from(y),
                    f64::from(x.saturating_add(1)),
                    f64::from(y.saturating_add(1)),
                );
                if changed
                    .iter()
                    .any(|rect| !rect.intersect(cell).is_zero_area())
                {
                    assert!(
                        hints.iter().any(|hint| {
                            x >= hint.x
                                && y >= hint.y
                                && x < hint.x.saturating_add(hint.width)
                                && y < hint.y.saturating_add(hint.height)
                        }),
                        "changed framebuffer pixel ({x}, {y}) omitted"
                    );
                }
            }
        }
        assert!(hints.iter().all(|hint| {
            hint.x.saturating_add(hint.width) <= 32 && hint.y.saturating_add(hint.height) <= 32
        }));
    }

    #[test]
    fn uncertain_damage_preserves_whole_surface_presentation() {
        for damage in [
            vec![],
            vec![Rect::new(f64::NAN, 0.0, 1.0, 1.0)],
            vec![Rect::new(0.0, 0.0, f64::INFINITY, 1.0)],
            vec![Rect::new(3.0, 0.0, 2.0, 1.0)],
            vec![Rect::new(0.0, 0.0, 1.0, 1.0); 129],
        ] {
            assert!(bounded_present_damage(32, 32, &damage).is_none());
        }
    }
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
                    _: &crate::widget::AppModel,
                    _: &crate::widget::ModelPath,
                ) -> bool {
                    // The owner wrote the label; this pass takes it up.
                    self.pending.take().is_some()
                }
                fn sync(&mut self, doc: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
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
            doc.put(automerge::ROOT, "label", "initial").unwrap();
            root.update(&doc, &automerge::ROOT);
            root.sync(&doc, &automerge::ROOT);
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
                doc.put(automerge::ROOT, "label", value).unwrap();
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
                    assert!(root.update(&doc, &automerge::ROOT));
                    root.sync(&doc, &automerge::ROOT);
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

/// Every frame the native host presents is its retained target after
/// [`rasterize_damage`]. However frames alternate between partial and full
/// raster, what is presented must equal a full render of the same scene;
/// otherwise the screen shows holes or stale content outside the damage.
#[cfg(test)]
mod retained_damage_tests {
    use super::*;
    use vello::kurbo::{Affine, Circle, RoundedRect, Stroke};
    use vello::peniko::{Fill, Gradient};
    use vello::wgpu;

    const WIDTH: u32 = 320;
    const HEIGHT: u32 = 200;

    /// One moving or recoloured item on a still backdrop, like a field,
    /// a caret, a hovered button or a spinner.
    #[derive(Clone, Copy)]
    struct Item {
        rect: Rect,
        color: [u8; 3],
        round: bool,
    }

    fn scene_of(items: &[Item]) -> Scene {
        let mut scene = Scene::new();
        let bounds = Rect::new(0.0, 0.0, f64::from(WIDTH), f64::from(HEIGHT));
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            &Gradient::new_linear((0.0, 0.0), (f64::from(WIDTH), f64::from(HEIGHT))).with_stops(
                [
                    Color::from_rgb8(18, 20, 28),
                    Color::from_rgb8(40, 60, 110),
                    Color::from_rgb8(20, 22, 30),
                ]
                .as_slice(),
            ),
            None,
            &bounds,
        );
        for item in items {
            let [r, g, b] = item.color;
            let color = Color::from_rgb8(r, g, b);
            if item.round {
                let center = item.rect.center();
                let radius = item.rect.width().min(item.rect.height()) / 2.0;
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    color,
                    None,
                    &Circle::new(center, radius),
                );
            } else {
                let shape = RoundedRect::from_rect(item.rect, 6.0);
                scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &shape);
                scene.stroke(
                    &Stroke::new(1.5),
                    Affine::IDENTITY,
                    Color::from_rgb8(220, 225, 235),
                    None,
                    &shape,
                );
            }
        }
        scene
    }

    fn target(device: &wgpu::Device) -> (wgpu::Texture, wgpu::TextureView) {
        // The same usage the native surface target carries.
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("retained-damage-test-target"),
            size: wgpu::Extent3d {
                width: WIDTH,
                height: HEIGHT,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        (texture, view)
    }

    fn read(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> Vec<u8> {
        let row = (WIDTH * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("retained-damage-test-readback"),
            size: u64::from(row * HEIGHT),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(row),
                    rows_per_image: Some(HEIGHT),
                },
            },
            wgpu::Extent3d {
                width: WIDTH,
                height: HEIGHT,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);
        let slice = buffer.slice(..);
        let (tx, rx) = mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        rx.recv().unwrap().unwrap();
        let mapped = slice.get_mapped_range();
        let mut tight = Vec::with_capacity((WIDTH * HEIGHT * 4) as usize);
        for y in 0..HEIGHT as usize {
            let start = y * row as usize;
            tight.extend_from_slice(&mapped[start..start + (WIDTH * 4) as usize]);
        }
        drop(mapped);
        buffer.unmap();
        tight
    }

    /// A small deterministic generator, so a failure names a reproducible
    /// frame.
    struct Lcg(u64);

    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0 >> 33
        }

        fn below(&mut self, n: u64) -> f64 {
            #[allow(
                clippy::cast_precision_loss,
                reason = "test coordinates are far below 2^52"
            )]
            {
                (self.next() % n) as f64
            }
        }
    }

    /// Marching ants around a fur selection: thousands of dashes over the
    /// whole surface, far more than a fresh bump reservation holds.
    fn marching_ants() -> Scene {
        let mut scene = scene_of(&[]);
        let mut rng = Lcg(0xa17);
        let stroke = Stroke::new(1.0).with_dashes(0.0, [2.0, 2.0]);
        for _ in 0..160 {
            let mut path = vello::kurbo::BezPath::new();
            path.move_to((rng.below(u64::from(WIDTH)), rng.below(u64::from(HEIGHT))));
            for _ in 0..48 {
                path.line_to((rng.below(u64::from(WIDTH)), rng.below(u64::from(HEIGHT))));
            }
            path.close_path();
            scene.stroke(&stroke, Affine::IDENTITY, Color::WHITE, None, &path);
            scene.stroke(
                &Stroke::new(1.0).with_dashes(2.0, [2.0, 2.0]),
                Affine::IDENTITY,
                Color::BLACK,
                None,
                &path,
            );
        }
        scene
    }

    static EMPTY_BACKDROP: std::sync::OnceLock<crate::backdrop::BackdropFrame> =
        std::sync::OnceLock::new();

    fn content<'a>(
        scene: &'a Scene,
        base_color: Color,
        damage_regions: &'a [Rect],
        full_redraw: bool,
    ) -> FrameContent<'a> {
        FrameContent {
            backdrop: EMPTY_BACKDROP.get_or_init(crate::backdrop::BackdropFrame::default),
            scene,
            width: WIDTH,
            height: HEIGHT,
            base_color,
            damage_regions,
            full_redraw,
        }
    }

    #[test]
    fn cpu_raster_damage_upload_matches_full_target_without_vello_fine_raster() {
        let gpu = crate::offscreen::shared_gpu().expect("GPU context for CPU upload oracle");
        let (retained, _) = target(&gpu.device);
        let (oracle, _) = target(&gpu.device);
        let mut canvas = cpu_raster::CpuCanvas::new();
        let first = [Item {
            rect: Rect::new(30.0, 25.0, 75.0, 60.0),
            color: [220, 60, 30],
            round: false,
        }];
        let mut next = first;
        next[0].rect = Rect::new(85.0, 25.0, 130.0, 60.0);
        canvas
            .rasterize(&content(&scene_of(&first), Color::TRANSPARENT, &[], true))
            .expect("initial CPU frame");
        canvas.upload(&gpu.queue, &retained);
        canvas
            .rasterize(&content(
                &scene_of(&next),
                Color::TRANSPARENT,
                &[Rect::new(28.0, 23.0, 132.0, 62.0)],
                false,
            ))
            .expect("damaged CPU frame");
        canvas.upload(&gpu.queue, &retained);
        let mut full = cpu_raster::CpuCanvas::new();
        full.rasterize(&content(&scene_of(&next), Color::TRANSPARENT, &[], true))
            .expect("full CPU oracle");
        full.upload(&gpu.queue, &oracle);
        assert_eq!(
            read(&gpu.device, &gpu.queue, &retained),
            read(&gpu.device, &gpu.queue, &oracle)
        );
    }

    /// A frame that overflowed Vello's bump buffers was presented without
    /// some of its geometry. The worker must notice from the counters alone
    /// and repaint the whole surface; later damage-only frames, or none at
    /// all for an idle window, never touch most of the missing pixels.
    #[test]
    fn an_overflowed_frame_is_repainted_whole() {
        let gpu = crate::offscreen::shared_gpu().expect("shared GPU context for the damage oracle");
        let device = &gpu.device;
        let queue = &gpu.queue;
        let options = || vello::RendererOptions {
            use_cpu: false,
            antialiasing_support: vello::AaSupport::area_only(),
            num_init_threads: None,
            pipeline_cache: None,
        };
        let base_color = Color::from_rgb8(10, 10, 12);
        let scene = marching_ants();
        let corner = [Rect::new(0.0, 0.0, 24.0, 24.0)];

        // Software rasterization sums a tile's segments in GPU scheduling
        // order, and this dense scene's dashes then differ by more than
        // anti-aliasing between two complete renders. Both renderers sum in a
        // fixed order, so the comparison sees only missing geometry.
        let renderer_for = || {
            let mut renderer = Renderer::new(device, options()).unwrap();
            renderer.enable_canonical_segment_order(device).unwrap();
            renderer
        };
        // What a renderer that already knows this scene presents.
        let mut oracle = renderer_for();
        let (oracle_texture, oracle_view) = target(device);
        let mut oracle_cache = DamageCache::default();
        let mut fitted = false;
        for _ in 0..8 {
            rasterize_damage(
                (device, queue),
                RetainedTarget {
                    texture: &oracle_texture,
                    view: &oracle_view,
                },
                &mut oracle,
                &mut oracle_cache,
                &content(&scene, base_color, &[], true),
            )
            .unwrap();
            if !oracle.settle_bump_budget(device) {
                fitted = true;
                break;
            }
        }
        assert!(fitted, "the oracle must converge to a complete frame");
        let expected = read(device, queue, &oracle_texture);

        let mut renderer = renderer_for();
        let (retained_texture, retained_view) = target(device);
        let retained = RetainedTarget {
            texture: &retained_texture,
            view: &retained_view,
        };
        let mut cache = DamageCache::default();
        let mut recovery = bump_recovery::BumpRecovery::default();
        // The selection appears: one full frame on a fresh reservation,
        // then the ants crawl in a small corner, as damage-only frames do.
        rasterize_damage(
            (device, queue),
            retained,
            &mut renderer,
            &mut cache,
            &content(&scene, base_color, &[], true),
        )
        .unwrap();
        rasterize_damage(
            (device, queue),
            retained,
            &mut renderer,
            &mut cache,
            &content(&scene, base_color, &corner, false),
        )
        .unwrap();

        let garbled = crate::offscreen::OffscreenRenderer::pixels_differing_beyond_aa(
            &read(device, queue, &retained_texture),
            &expected,
        );
        assert!(
            garbled > 0,
            "the overflowed frame must visibly lack geometry, or this test proves nothing"
        );

        // The idle worker: poll on its receive timeout and repaint whenever
        // asked, exactly as main answers `take_full_repaint`.
        let mut repaints = 0;
        loop {
            let step = recovery.poll(&mut renderer, device);
            match step {
                bump_recovery::BumpStep::Pending => {
                    assert_eq!(recovery.wait(), Some(bump_recovery::POLL_INTERVAL));
                    let _ = device.poll(wgpu::PollType::wait_indefinitely());
                }
                bump_recovery::BumpStep::Repaint => {
                    repaints += 1;
                    assert!(
                        repaints <= usize::from(bump_recovery::MAX_REPAINTS),
                        "repaints are bounded"
                    );
                    rasterize_damage(
                        (device, queue),
                        retained,
                        &mut renderer,
                        &mut cache,
                        &content(&scene, base_color, &[], true),
                    )
                    .unwrap();
                }
                bump_recovery::BumpStep::Settled => break,
            }
        }
        assert!(
            repaints > 0,
            "the scene must overflow a fresh reservation, or this test proves nothing"
        );
        let presented = read(device, queue, &retained_texture);
        let differing =
            crate::offscreen::OffscreenRenderer::pixels_differing_beyond_aa(&presented, &expected);
        assert_eq!(
            differing, 0,
            "after {repaints} repaint(s) the presented frame still lacks {differing} pixels"
        );
    }

    /// A frame whose bump counters report no overflow drew everything. The
    /// segments reservation is separate from seg_counts and had no overflow
    /// check: a frame could outgrow it, report `failed=0`, and be presented
    /// with geometry missing, which the overflow repaint then never fixed.
    #[test]
    fn a_frame_reported_complete_is_complete() {
        let gpu = crate::offscreen::shared_gpu().expect("shared GPU context for the damage oracle");
        let device = &gpu.device;
        let queue = &gpu.queue;
        let base_color = Color::from_rgb8(10, 10, 12);
        let scene = marching_ants();
        let mut renderer = Renderer::new(
            device,
            vello::RendererOptions {
                use_cpu: false,
                antialiasing_support: vello::AaSupport::area_only(),
                num_init_threads: None,
                pipeline_cache: None,
            },
        )
        .unwrap();
        let (texture, view) = target(device);
        let mut cache = DamageCache::default();
        let mut render = |renderer: &mut Renderer| {
            rasterize_damage(
                (device, queue),
                RetainedTarget {
                    texture: &texture,
                    view: &view,
                },
                renderer,
                &mut cache,
                &content(&scene, base_color, &[], true),
            )
            .unwrap();
            renderer.settle_bump_budget(device)
        };
        let mut overflows = 0;
        while render(&mut renderer) {
            overflows += 1;
            assert!(overflows <= 8, "the reservation must converge");
        }
        assert!(
            overflows > 0,
            "the scene must overflow a fresh reservation, or this test proves nothing"
        );
        let reported_complete = read(device, queue, &texture);
        assert!(
            !render(&mut renderer),
            "a converged reservation keeps fitting"
        );
        let again = read(device, queue, &texture);
        let differing = crate::offscreen::OffscreenRenderer::pixels_differing_beyond_aa(
            &reported_complete,
            &again,
        );
        assert_eq!(
            differing, 0,
            "a frame reported complete after {overflows} overflow(s) lacked {differing} pixels"
        );
    }

    #[test]
    fn every_presented_frame_equals_a_full_render() {
        let gpu = crate::offscreen::shared_gpu().expect("shared GPU context for the damage oracle");
        let device = &gpu.device;
        let queue = &gpu.queue;
        let options = || vello::RendererOptions {
            use_cpu: false,
            antialiasing_support: vello::AaSupport::area_only(),
            num_init_threads: None,
            pipeline_cache: None,
        };
        let mut renderer = Renderer::new(device, options()).unwrap();
        let mut oracle = Renderer::new(device, options()).unwrap();
        let (retained_texture, retained_view) = target(device);
        let (oracle_texture, oracle_view) = target(device);
        let mut cache = DamageCache::default();
        let mut oracle_cache = DamageCache::default();
        let base_color = Color::from_rgb8(10, 10, 12);

        let mut items: Vec<Item> = (0..6)
            .map(|i| Item {
                rect: Rect::new(
                    20.0 + f64::from(i) * 45.0,
                    30.0,
                    60.0 + f64::from(i) * 45.0,
                    64.0,
                ),
                color: [90, 120, 200],
                round: i % 2 == 1,
            })
            .collect();
        let mut rng = Lcg(0x5eed);

        for frame in 0..48 {
            let mut damage = Vec::new();
            // Frame 0 and every eleventh frame repaint everything, as a
            // relayout or a rebuilt tree does; the rest change one to three
            // items, as typing, a caret or hover does.
            let full_redraw = frame == 0 || frame % 11 == 0;
            let changes = 1 + rng.below(3) as usize;
            for _ in 0..changes {
                let index = rng.below(items.len() as u64) as usize;
                let before = items[index];
                let mut after = before;
                // Small moves keep damage partial; an occasional big one
                // crosses the half-screen threshold into one full pass.
                let reach: u64 = if rng.below(9) < 1.0 { 260 } else { 30 };
                let dx = rng.below(reach) - (reach as f64) / 2.0;
                let dy = rng.below(reach / 2) - (reach as f64) / 4.0;
                after.rect = Rect::new(
                    (before.rect.x0 + dx).clamp(0.0, f64::from(WIDTH) - 40.0),
                    (before.rect.y0 + dy).clamp(0.0, f64::from(HEIGHT) - 34.0),
                    0.0,
                    0.0,
                );
                after.rect.x1 = after.rect.x0 + before.rect.width();
                after.rect.y1 = after.rect.y0 + before.rect.height();
                after.color = [
                    rng.below(255) as u8,
                    rng.below(255) as u8,
                    rng.below(255) as u8,
                ];
                items[index] = after;
                // What the native host reports: both boxes, with its margin.
                damage.push(before.rect.union(after.rect).inflate(16.0, 16.0));
            }
            let scene = scene_of(&items);
            let content = FrameContent {
                backdrop: &crate::backdrop::BackdropFrame::default(),
                scene: &scene,
                width: WIDTH,
                height: HEIGHT,
                base_color,
                damage_regions: &damage,
                full_redraw,
            };
            rasterize_damage(
                (device, queue),
                RetainedTarget {
                    texture: &retained_texture,
                    view: &retained_view,
                },
                &mut renderer,
                &mut cache,
                &content,
            )
            .unwrap();
            rasterize_damage(
                (device, queue),
                RetainedTarget {
                    texture: &oracle_texture,
                    view: &oracle_view,
                },
                &mut oracle,
                &mut oracle_cache,
                &FrameContent {
                    backdrop: &crate::backdrop::BackdropFrame::default(),
                    full_redraw: true,
                    ..content
                },
            )
            .unwrap();
            let presented = read(device, queue, &retained_texture);
            let expected = read(device, queue, &oracle_texture);
            let differing = crate::offscreen::OffscreenRenderer::pixels_differing_beyond_aa(
                &presented, &expected,
            );
            assert_eq!(
                differing, 0,
                "frame {frame} (full redraw: {full_redraw}, damage {damage:?}) presented \
                 {differing} pixels that a full render does not have"
            );
        }
    }
}

/// The app a startup summary names: its package, else its executable.
fn startup_app_name() -> String {
    std::env::var("TILO_PACKAGE_ID").ok().unwrap_or_else(|| {
        std::env::current_exe()
            .ok()
            .and_then(|path| {
                path.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "unknown".to_owned())
    })
}
