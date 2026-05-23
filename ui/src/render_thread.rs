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
                );

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
) -> FrameOutcome {
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

    // 2. Block on the swapchain image.
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
            panic!("wgpu surface validation error");
        }
    };

    // 3. Encoder + 4. blit + 5. submit.
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

    // 6. Present.
    surface_texture.present();
    // 7. Drive the wgpu polling loop forward so submitted command
    //    buffers actually complete. `Poll` is the non-blocking
    //    variant — we don't want the worker to stall here, just to
    //    nudge the queue.
    let _ = device_handle.device.poll(vello::wgpu::PollType::Poll);
    FrameOutcome::Presented
}
