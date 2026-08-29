//! UI render worker thread.
//!
//! Moves scene encode, swapchain acquire, blit, submit, present, and poll
//! off the main winit thread onto a dedicated `ui-render` worker. Main keeps
//! the widget tree + scene encoding; the worker owns the wgpu
//! `RenderContext`, `RenderSurface<'static>`, and `vello::Renderer`.
//!
//! Three channels: `render_tx`/`rx` (capacity 1 — blocks main until the
//! worker drains the previous frame) carries an encoded `Scene` + dims +
//! base colour; `scene_pool_tx`/`scene_pool` (worker → main, capacity 2,
//! pre-seeded) recycles each `Scene`'s buffer instead of reallocating;
//! `outcome_tx`/`frame_complete` (capacity 8) reports per-frame outcomes so
//! main can re-request a redraw on Outdated/Suboptimal/Occluded/Timeout.
//!
//! The worker resizes the surface itself — main never touches it after
//! handing it off at spawn — so wgpu state never crosses threads.
//!
//! On `exiting`, the App sends `RenderRequest::Shutdown` and `join()`s the
//! worker, which drops its `RenderSurface<'static>` before the `Window` is
//! dropped — required or wgpu panics on a surface outliving its window.

use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::JoinHandle;

use crate::gpu::RenderContext;
use vello::peniko::Color;
use vello::util::RenderSurface;
use vello::{RenderParams, Renderer, Scene};

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

/// Called by an app before its window comes up. A no-op for apps that
/// don't set one, so the shared render path is unchanged for everyone else.
pub fn set_frame_post_processor(p: Box<dyn FramePostProcess>) {
    *POST_PROCESSOR.lock().unwrap() = Some(p);
}

/// Compile-time `Send` check: if a future vello/wgpu version regresses the
/// `Send` impl on `Renderer`/`Scene`, this fails to compile rather than
/// failing at thread spawn. Hand-rolled to avoid a dep on
/// `static_assertions` for one macro.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<Renderer>();
    assert_send::<Scene>();
    assert_send::<RenderSurface<'static>>();
    assert_send::<RenderContext>();
};

pub enum RenderRequest {
    Frame {
        // Boxed so the `Shutdown` variant (which carries nothing) doesn't
        // force every enum value to reserve room for an inline `Scene`.
        scene: Box<Scene>,
        width: u32,
        height: u32,
        base_color: Color,
    },
    /// Drain the channel and exit the worker loop. Sent from
    /// `AppHandler::exiting`; the App then `join()`s the handle.
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameOutcome {
    Presented,
    /// Surface was outdated or suboptimal; worker reconfigured it and
    /// skipped the frame. Main should redraw.
    Reconfigured,
    /// Window is occluded or compositor refused the frame; main should
    /// redraw (winit will throttle if still occluded).
    Skipped,
    /// No retry will help; the worker stops after emitting this and main
    /// treats it as "close the window". Ordinary end of session, not a
    /// fault — the compositor exiting destroys the surface mid-frame, and
    /// wgpu's uncaptured-validation-error handler is `panic!`, which under
    /// `panic = "abort"` killed the process. This variant avoids that.
    SurfaceGone,
}

pub struct RenderThreadHandle {
    pub render_tx: SyncSender<RenderRequest>,
    pub scene_pool: Receiver<Scene>,
    pub frame_complete: Receiver<FrameOutcome>,
    join: Option<JoinHandle<()>>,
}

impl RenderThreadHandle {
    /// Idempotent: a second call is a no-op (the `JoinHandle` is taken on
    /// the first). The worker drops its `RenderSurface` before this
    /// returns — it must die before the `Window` it was created against,
    /// or wgpu may panic on Vulkan backends.
    pub fn shutdown_and_join(&mut self) {
        // Best-effort: the worker may already have crashed and dropped the
        // receiver, in which case the send errors and join yields immediately.
        let _ = self.render_tx.send(RenderRequest::Shutdown);
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

/// Channel capacities are documented at the module level.
pub fn spawn_render_thread(
    render_cx: RenderContext,
    mut surface: RenderSurface<'static>,
    mut renderer: Renderer,
) -> RenderThreadHandle {
    let (render_tx, rx) = mpsc::sync_channel::<RenderRequest>(1);
    let (scene_pool_tx, scene_pool) = mpsc::sync_channel::<Scene>(2);
    let (outcome_tx, frame_complete) = mpsc::sync_channel::<FrameOutcome>(8);

    // Seed the pool so main finds a Scene buffer ready on the first frame
    // instead of allocating.
    scene_pool_tx
        .send(Scene::new())
        .expect("seeded pool send must succeed");
    scene_pool_tx
        .send(Scene::new())
        .expect("seeded pool send must succeed");

    let pool_tx = scene_pool_tx.clone();
    let join = std::thread::Builder::new()
        .name("ui-render".into())
        .spawn(move || {
            // `QUICK_UI_PRESENT=1` logs per-frame encode/acquire/present timings
            // — a slow compositor/present path shows up as a large acquire or
            // present, invisible to offscreen benches. Off by default.
            let present_dbg = std::env::var("QUICK_UI_PRESENT").is_ok();
            let mut frame_ix: u64 = 0;
            while let Ok(msg) = rx.recv() {
                let (mut scene, width, height, base_color) = match msg {
                    RenderRequest::Frame {
                        scene,
                        width,
                        height,
                        base_color,
                    } => (*scene, width, height, base_color),
                    RenderRequest::Shutdown => break,
                };

                // Resize on the worker so main never touches wgpu state after
                // spawn — simpler than an explicit Resize variant, and keeps
                // every wgpu call on the thread that owns the surface.
                if (surface.config.width != width || surface.config.height != height)
                    && let Err(err) =
                        resize_surface_checked(&render_cx, &mut surface, width, height)
                {
                    // Re-attempting the same reconfigure on a destroyed
                    // surface fails identically forever.
                    tracing::info!(%err, "drawing surface is gone; stopping the render thread");
                    let _ = outcome_tx.send(FrameOutcome::SurfaceGone);
                    break;
                }

                let outcome = render_one_frame(
                    &render_cx,
                    &mut surface,
                    &mut renderer,
                    &scene,
                    width,
                    height,
                    base_color,
                    present_dbg,
                    frame_ix,
                );
                frame_ix += 1;

                // `Scene::reset` clears the encoding without freeing the
                // backing vecs, so main avoids a reallocation on reuse.
                scene.reset();
                // Non-blocking: at most one Scene is outstanding on main (the
                // one being built), so the pool (capacity 2) never fills. If
                // main has shut down and the receiver is gone, dropping is fine.
                let _ = pool_tx.send(scene);
                let _ = outcome_tx.send(outcome);
                if outcome == FrameOutcome::SurfaceGone {
                    break;
                }
            }
            // Worker exits — `surface`, `renderer`, `render_cx` drop here in
            // that order, which is the order wgpu requires (surface first
            // releases the swapchain on the instance `render_cx` owns).
        })
        .expect("ui-render thread spawn must succeed");
    // pool_tx (the worker's clone) keeps the pool alive; main never sends
    // into it, so the original can be dropped.
    drop(scene_pool_tx);

    RenderThreadHandle {
        render_tx,
        scene_pool,
        frame_complete,
        join: Some(join),
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
    let scope = render_cx.devices[surface.dev_id]
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
    let scope = render_cx.devices[surface.dev_id]
        .device
        .push_error_scope(vello::wgpu::ErrorFilter::Validation);
    render_cx.resize_surface(surface, width, height);
    match pollster::block_on(scope.pop()) {
        Some(err) => Err(err.to_string()),
        None => Ok(()),
    }
}

// One call site: this is the whole worker's per-frame encode+present step,
// and each parameter is a distinct, independently necessary piece of GPU
// state or frame metadata. Bundling would trade nine named parameters for
// an equally large ad-hoc struct with no behavioural change.
#[allow(clippy::too_many_arguments)]
fn render_one_frame(
    render_cx: &RenderContext,
    surface: &mut RenderSurface<'static>,
    renderer: &mut Renderer,
    scene: &Scene,
    width: u32,
    height: u32,
    base_color: Color,
    present_dbg: bool,
    frame_ix: u64,
) -> FrameOutcome {
    use std::time::Instant;
    let t_encode = present_dbg.then(Instant::now);
    let device_handle = &render_cx.devices[surface.dev_id];

    if let Err(err) = renderer.render_to_texture(
        &device_handle.device,
        &device_handle.queue,
        scene,
        &surface.target_view,
        &RenderParams {
            base_color,
            width,
            height,
            antialiasing_method: vello::AaConfig::Area,
        },
    ) {
        // A programmer bug (bad encoding / ran out of GPU resources); log and
        // skip rather than panic, which would tear down the app with no
        // chance for main to clean up.
        tracing::error!(?err, "vello render_to_texture failed");
        return FrameOutcome::Skipped;
    }

    // Composite any post-processor content (the browser's zero-copy WebView
    // texture) into the intermediate texture before it goes to the surface.
    if let Ok(mut guard) = POST_PROCESSOR.lock()
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

    let t_acquire = present_dbg.then(Instant::now);
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
            return FrameOutcome::Reconfigured;
        }
        vello::wgpu::CurrentSurfaceTexture::Occluded
        | vello::wgpu::CurrentSurfaceTexture::Timeout => {
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

    let t_submit = present_dbg.then(Instant::now);
    let mut encoder =
        device_handle
            .device
            .create_command_encoder(&vello::wgpu::CommandEncoderDescriptor {
                label: Some("ui-render-thread blit-to-surface"),
            });
    surface.blitter.copy(
        &device_handle.device,
        &mut encoder,
        &surface.target_view,
        &surface_texture
            .texture
            .create_view(&vello::wgpu::TextureViewDescriptor::default()),
    );
    device_handle.queue.submit([encoder.finish()]);

    let submit_us = t_submit.map(|t| t.elapsed().as_secs_f64() * 1e6);

    let t_present = present_dbg.then(Instant::now);
    surface_texture.present();
    if present_dbg {
        let present_us = t_present.map_or(0.0, |t| t.elapsed().as_secs_f64() * 1e6);
        eprintln!(
            "present-frame {frame_ix}: encode={:.0}us acquire={:.0}us submit={:.0}us present={:.0}us  ({}x{})",
            encode_us.unwrap_or(0.0),
            acquire_us.unwrap_or(0.0),
            submit_us.unwrap_or(0.0),
            present_us,
            width,
            height,
        );
    }
    // `Poll` is the non-blocking variant — nudges the queue without stalling.
    let _ = device_handle.device.poll(vello::wgpu::PollType::Poll);
    FrameOutcome::Presented
}
