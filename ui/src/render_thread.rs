//! UI render worker thread (Item 4 of the 2026-05-23 perf plan).
//!
//! Moves the seven GPU steps that previously ran on the main winit
//! thread inside `RedrawRequested` —
//!
//! 1. `vello::Renderer::render_to_texture`
//! 2. `surface.get_current_texture()` (the **swapchain block**)
//! 3. `device.create_command_encoder(...)`
//! 4. `surface.blitter.copy(...)`
//! 5. `queue.submit(...)`
//! 6. `surface_texture.present()`
//! 7. `device.poll(Poll)`
//!
//! — onto a dedicated `std::thread::Builder::new().name("ui-render")`
//! worker. Main keeps ownership of the widget tree + scene encoding;
//! the worker owns the wgpu `RenderContext`, `RenderSurface<'static>`,
//! and `vello::Renderer`.
//!
//! ## Channel layout
//!
//! Three channels live between main and the worker:
//!
//! - **`render_tx` / `rx`** (main → worker, `sync_channel(1)`):
//!   a fully-encoded `Scene`, target dimensions, and the base
//!   color. Capacity 1 makes it a strict double-buffer — main
//!   blocks here if the worker hasn't drained the previous
//!   frame, providing backpressure.
//!
//! - **`scene_pool_tx` / `scene_pool`** (worker → main,
//!   `sync_channel(2)`): the worker resets and pushes each
//!   processed `Scene` straight back into the pool; main pulls
//!   from `scene_pool` at the top of the next `RedrawRequested`
//!   so the encoding buffer is recycled instead of reallocated.
//!   The pool is seeded with two fresh `Scene`s so the first
//!   two frames find a buffer waiting.
//!
//! - **`outcome_tx` / `frame_complete`** (worker → main,
//!   `sync_channel(8)`): one message per frame so main can
//!   react to `Outdated / Suboptimal / Occluded / Timeout` by
//!   re-requesting a redraw. Small burst tolerance; main drains
//!   it on every `RedrawRequested`.
//!
//! The spec mentions a per-request `done` channel as an
//! alternative; we use the single shared pool channel because it
//! avoids spawning a tiny detached thread per frame.
//!
//! ## Resize
//!
//! Option 1 from the spec: the `RenderRequest` carries the latest
//! `width / height` and the worker compares against the surface's
//! current dims, calling `render_cx.resize_surface(...)` itself when
//! they differ. Main never touches the surface after handing it off
//! at spawn time, which is what lets the surface live on the worker
//! without sharing wgpu state across threads.
//!
//! ## Shutdown
//!
//! When the winit loop calls `exiting`, the App sends
//! `RenderRequest::Shutdown` and `join()`s the worker. The worker
//! drops its `RenderSurface<'static>` BEFORE the `Window` (whose
//! `Arc` lives in `AppHandler::window`) is dropped, which avoids the
//! wgpu surface-lifetime panic the spec flagged. If the worker is
//! already dead (`render_tx.send` errored earlier), the join is a
//! no-op.

use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::JoinHandle;

use vello::peniko::Color;
use vello::util::{RenderContext, RenderSurface};
use vello::{RenderParams, Renderer, Scene};

/// A hook run on the render thread *after* vello encodes the frame into its
/// intermediate texture and *before* that texture is blit to the surface.
/// It may composite extra GPU content into `target` — used by the browser
/// to draw its zero-copy WebView texture (which vello cannot sample) over
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

/// Install a [`FramePostProcess`] hook (replacing any previous). Called by
/// an app before its window comes up. A no-op for apps that don't set one,
/// so the shared render path is unchanged for everyone else.
pub fn set_frame_post_processor(p: Box<dyn FramePostProcess>) {
    *POST_PROCESSOR.lock().unwrap() = Some(p);
}

/// Compile-time `Send` check. If a future `vello` / `wgpu` version
/// regresses the `Send` impl on either `Renderer` or `Scene`, the
/// crate stops compiling here rather than failing at thread spawn.
///
/// Pattern stolen from `static_assertions::assert_impl_all!`; written
/// out long-hand to avoid a dep on a one-trick macro crate.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<Renderer>();
    assert_send::<Scene>();
    assert_send::<RenderSurface<'static>>();
    // RenderContext owns the wgpu Instance + Device handles. Both are
    // `Send` in wgpu 29; we only need `Send` (one worker owns it).
    assert_send::<RenderContext>();
};

/// One frame's worth of work, sent from the main thread to the
/// worker. The `Shutdown` variant is the join signal; everything
/// else is a frame.
pub enum RenderRequest {
    Frame {
        scene: Scene,
        width: u32,
        height: u32,
        base_color: Color,
    },
    /// Drain the channel and exit the worker loop. Sent from
    /// `AppHandler::exiting`; the App then `join()`s the handle.
    Shutdown,
}

/// What happened to a frame on the worker. Main consumes these to
/// re-request a redraw for the Outdated / Suboptimal / Occluded /
/// Timeout cases that the pre-refactor inline code handled by
/// returning early and calling `window.request_redraw()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameOutcome {
    /// Frame presented successfully.
    Presented,
    /// Surface was outdated or suboptimal; worker reconfigured it
    /// and skipped the frame. Main should redraw.
    Reconfigured,
    /// Window is occluded or compositor refused the frame; main
    /// should redraw (winit will throttle if still occluded).
    Skipped,
}

/// Handle to the render worker thread. Held by `AppHandler` for the
/// process lifetime; `shutdown_and_join` is called from
/// `ApplicationHandler::exiting`.
pub struct RenderThreadHandle {
    pub render_tx: SyncSender<RenderRequest>,
    pub scene_pool: Receiver<Scene>,
    pub frame_complete: Receiver<FrameOutcome>,
    join: Option<JoinHandle<()>>,
}

impl RenderThreadHandle {
    /// Send `Shutdown` and join the worker. Called from
    /// `ApplicationHandler::exiting`. Idempotent: a second call is
    /// a no-op (the JoinHandle is taken on the first call).
    ///
    /// The worker drops its `RenderSurface` inside its own stack
    /// frame BEFORE this function returns. That ordering matters:
    /// the surface must die before the `Window` it was created
    /// against, or wgpu may panic on Vulkan backends.
    pub fn shutdown_and_join(&mut self) {
        // Best-effort: if the worker already crashed and dropped
        // the receiver, the send returns Err and we still try to
        // join (which will yield immediately).
        let _ = self.render_tx.send(RenderRequest::Shutdown);
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

/// Spawn the worker. Moves `render_cx`, `surface`, and `renderer`
/// onto the new thread; returns the channel ends the App needs.
///
/// Channel capacities follow the spec:
/// - `render_tx`: 1 — strict double-buffer. Main blocks here if the
///   worker hasn't drained the previous frame.
/// - `scene_pool`: 2 — seeded with two fresh `Scene`s so the first
///   two frames find a buffer waiting.
/// - `frame_complete`: 8 — small burst tolerance; main drains every
///   `about_to_wait`.
pub fn spawn_render_thread(
    render_cx: RenderContext,
    mut surface: RenderSurface<'static>,
    mut renderer: Renderer,
) -> RenderThreadHandle {
    let (render_tx, rx) = mpsc::sync_channel::<RenderRequest>(1);
    let (scene_pool_tx, scene_pool) = mpsc::sync_channel::<Scene>(2);
    let (outcome_tx, frame_complete) = mpsc::sync_channel::<FrameOutcome>(8);

    // Seed the pool so main finds a Scene buffer ready on the very
    // first frame instead of allocating (and so the first recv()
    // doesn't block).
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
            // Diagnostic: `QUICK_UI_PRESENT=1` logs per-frame GPU-encode /
            // swapchain-acquire / present timings from the worker so we can see
            // where the live present spends time (a slow compositor/present path
            // shows up as a large acquire or present, invisible to offscreen
            // benches). Read once; off by default, zero hot-path cost otherwise.
            let present_dbg = std::env::var("QUICK_UI_PRESENT").is_ok();
            let mut frame_ix: u64 = 0;
            while let Ok(msg) = rx.recv() {
                let (mut scene, width, height, base_color) = match msg {
                    RenderRequest::Frame { scene, width, height, base_color } => {
                        (scene, width, height, base_color)
                    }
                    RenderRequest::Shutdown => break,
                };

                // Resize the surface here, on the worker, so main
                // never touches wgpu state after spawn. Option 1
                // from the spec ("worker handles resize") —
                // simpler than an explicit Resize variant and keeps
                // every wgpu call on the same thread that owns the
                // surface.
                //
                // Clamp the requested size to the range GPU texture allocation
                // allows before resizing. `vello::resize_surface` (and the
                // `create_texture` underneath it) PANIC on a zero dimension and on
                // anything past the device's `max_texture_dimension_2d`. A
                // fullscreen configure can momentarily report a 0 dimension, and a
                // borderless-fullscreen surface spanning a large/combined output
                // can exceed the GPU limit (8192 on default limits) — either would
                // panic the render worker and take the whole app down with it
                // (that was the "fullscreen crashes the app" bug). The init path
                // already clamps with `.max(1)`; mirror it here and additionally
                // bound to the device max, then render at the clamped size so the
                // frame's `RenderParams` match the surface.
                let max_dim = render_cx.devices[surface.dev_id]
                    .device
                    .limits()
                    .max_texture_dimension_2d;
                let (width, height) = clamp_surface_dims(width, height, max_dim);
                if surface.config.width != width || surface.config.height != height {
                    render_cx.resize_surface(&mut surface, width, height);
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

                // Reset the scene's encoding buffer in-place so
                // main doesn't pay reallocation cost when it picks
                // this Scene back up from the pool. (`Scene::reset`
                // clears the path / brush encoding without freeing
                // the backing vecs.)
                scene.reset();
                // Push the recycled Scene directly into the pool.
                // If main has shut down and the pool receiver is
                // gone, dropping is fine. The pool's capacity (2)
                // makes this non-blocking under normal operation —
                // there's at most one outstanding Scene on main
                // (the one being built right now), so the pool
                // never holds more than one buffer.
                let _ = pool_tx.send(scene);
                let _ = outcome_tx.send(outcome);
            }
            // Worker exits — `surface`, `render_cx`, and
            // `renderer` drop here, IN ORDER: surface first
            // (releases the swapchain on the wgpu instance owned
            // by `render_cx`), then `renderer`, then `render_cx`.
            // This is the exact order wgpu requires.
        })
        .expect("ui-render thread spawn must succeed");
    // pool_tx was moved into the worker; the original
    // scene_pool_tx falls out of scope here. The pool stays
    // alive via the worker's clone for the entire process; main
    // never needs to send into it.
    drop(scene_pool_tx);

    RenderThreadHandle {
        render_tx,
        scene_pool,
        frame_complete,
        join: Some(join),
    }
}

/// Clamp a requested surface size into the range GPU texture allocation allows.
///
/// `vello::resize_surface` — and the `wgpu::Device::create_texture` underneath it
/// — panic on a **zero** dimension and on anything past the device's
/// `max_texture_dimension_2d`. The worker's resize path calls `resize_surface`
/// directly, so an un-clamped size panics the render thread and takes the whole
/// app with it. Two real triggers, both on the fullscreen transition:
///
/// * a Wayland fullscreen configure can momentarily report a `0` dimension;
/// * a borderless-fullscreen surface spanning a large or combined output can
///   exceed the GPU limit (8192 under `wgpu`'s default limits).
///
/// The init-time surface creation already guards zero with `.max(1)`; this is the
/// same guard for the resize path, plus an upper bound at the device max. A size
/// past the limit renders at the clamped size and the compositor scales it — a
/// slightly-soft frame instead of a dead app.
pub(crate) fn clamp_surface_dims(width: u32, height: u32, max_dim: u32) -> (u32, u32) {
    let max_dim = max_dim.max(1);
    (width.clamp(1, max_dim), height.clamp(1, max_dim))
}

/// The seven GPU steps that used to live inline in
/// `AppHandler::window_event(RedrawRequested)`. Pulled out so the
/// worker loop is short and the steps are unit-readable.
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

    // 1. Encode the scene into the target_view intermediate texture.
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
        // Vello render failure is a programmer bug (bad encoding /
        // ran out of GPU resources). Logging and skipping is the
        // safest behaviour — a panic on the worker would tear down
        // the whole app without main getting a chance to clean up.
        tracing::error!(?err, "vello render_to_texture failed");
        return FrameOutcome::Skipped;
    }

    // Composite any post-processor content (the browser's zero-copy WebView
    // texture) into the intermediate texture before it goes to the surface.
    if let Ok(mut guard) = POST_PROCESSOR.lock() {
        if let Some(p) = guard.as_mut() {
            p.post_render(
                &device_handle.device,
                &device_handle.queue,
                &surface.target_view,
                width,
                height,
            );
        }
    }

    let encode_us = t_encode.map(|t| t.elapsed().as_secs_f64() * 1e6);

    // 2. Block on the swapchain image.
    let t_acquire = present_dbg.then(Instant::now);
    let surface_texture = match surface.surface.get_current_texture() {
        vello::wgpu::CurrentSurfaceTexture::Success(t) => t,
        vello::wgpu::CurrentSurfaceTexture::Outdated
        | vello::wgpu::CurrentSurfaceTexture::Suboptimal(_) => {
            // Surface drifted (resize, scale change). Reconfigure
            // and ask main to redraw.
            render_cx.configure_surface(surface);
            return FrameOutcome::Reconfigured;
        }
        vello::wgpu::CurrentSurfaceTexture::Occluded
        | vello::wgpu::CurrentSurfaceTexture::Timeout => {
            return FrameOutcome::Skipped;
        }
        vello::wgpu::CurrentSurfaceTexture::Lost => {
            // Surface-level failure (driver reset, etc.). The
            // pre-refactor code panicked here; preserve that
            // behaviour so the symptom is identical.
            panic!("wgpu surface lost — backend-level failure");
        }
        vello::wgpu::CurrentSurfaceTexture::Validation => {
            // A validation error acquiring the swapchain image is recoverable —
            // it typically means the surface config drifted from the window (a
            // fullscreen transition is the classic trigger). Reconfigure and ask
            // main to redraw rather than panicking the worker and killing the app.
            tracing::warn!("wgpu surface validation error — reconfiguring");
            render_cx.configure_surface(surface);
            return FrameOutcome::Reconfigured;
        }
    };

    let acquire_us = t_acquire.map(|t| t.elapsed().as_secs_f64() * 1e6);

    // 3. Encoder + 4. blit + 5. submit.
    let t_submit = present_dbg.then(Instant::now);
    let mut encoder = device_handle.device.create_command_encoder(
        &vello::wgpu::CommandEncoderDescriptor {
            label: Some("ui-render-thread blit-to-surface"),
        },
    );
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

    // 6. Present.
    let t_present = present_dbg.then(Instant::now);
    surface_texture.present();
    if present_dbg {
        let present_us = t_present.map(|t| t.elapsed().as_secs_f64() * 1e6).unwrap_or(0.0);
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
    // 7. Drive the wgpu polling loop forward so submitted command
    //    buffers actually complete. `Poll` is the non-blocking
    //    variant — we don't want the worker to stall here, just to
    //    nudge the queue.
    let _ = device_handle.device.poll(vello::wgpu::PollType::Poll);
    FrameOutcome::Presented
}

#[cfg(test)]
mod tests {
    use super::clamp_surface_dims;

    /// The fullscreen-crash regression: the resize path must never hand
    /// `vello::resize_surface` a zero or over-limit dimension (it panics on both,
    /// killing the render worker and the app). `clamp_surface_dims` is exactly the
    /// guard the production resize call runs before `resize_surface`.
    #[test]
    fn clamp_surface_dims_guards_zero_and_over_limit() {
        const MAX: u32 = 8192; // wgpu default `max_texture_dimension_2d`

        // A transient 0 dimension from a fullscreen configure → clamped to 1, never 0.
        assert_eq!(clamp_surface_dims(0, 0, MAX), (1, 1));
        assert_eq!(clamp_surface_dims(1920, 0, MAX), (1920, 1));
        assert_eq!(clamp_surface_dims(0, 1080, MAX), (1, 1080));

        // A combined/large fullscreen output past the GPU limit → clamped to MAX,
        // never above (which is what panics on allocation).
        assert_eq!(clamp_surface_dims(10_000, 4320, MAX), (MAX, 4320));
        assert_eq!(clamp_surface_dims(15_360, 8640, MAX), (MAX, MAX));

        // An ordinary in-range size passes through untouched.
        assert_eq!(clamp_surface_dims(1920, 1080, MAX), (1920, 1080));
        assert_eq!(clamp_surface_dims(3840, 2160, MAX), (3840, 2160));

        // A degenerate device limit of 0 still yields a valid (non-panicking) size.
        assert_eq!(clamp_surface_dims(1920, 1080, 0), (1, 1));
    }
}
