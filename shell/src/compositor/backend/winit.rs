//! Phase 4 follow-up: drive the host window via vello+wgpu directly
//! rather than smithay's `WinitGraphicsBackend<GlowRenderer>`.
//!
//! Vello renders via wgpu while smithay's winit backend hands out an
//! OpenGL/Glow surface — the two cannot trivially share a surface. The
//! shortest unblock for "the host window actually paints chrome" is to
//! own the winit window ourselves and run the same vello pipeline that
//! `quick-ui::App::run` and `bin/shots` use. Smithay still owns the
//! wayland-server protocol state; this backend only owns the rendered
//! window. When Phase 6+ adds smithay-client surface presentation
//! (foreign tabs) we can layer a glow path or texture-import back in
//! on top — but until then this backend is the only consumer.
//!
//! The event integration uses winit 0.30's
//! [`pump_app_events`][pump] so the calloop event loop in
//! [`crate::compositor::native::run`] stays the driver: each
//! iteration calls [`Backend::poll_events`] (which pumps winit
//! non-blocking), then `frame_builder.paint`, then
//! [`Backend::submit_frame`].
//!
//! [pump]: winit::platform::pump_events::EventLoopExtPumpEvents::pump_app_events

use std::sync::Arc;
use std::time::Duration;

use vello::peniko::Color;
use vello::util::{RenderContext, RenderSurface};
use vello::{AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, Scene, wgpu};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::platform::pump_events::{EventLoopExtPumpEvents, PumpStatus};
use winit::window::{Window, WindowAttributes, WindowId};

use super::{Backend, BackendEvent, Output, OutputId};
use crate::compositor::present::Presenter;

/// Initial host-window size — matches the `shots` default so a fresh
/// `--mode native` launch is comparable to a `cargo run --bin shots`
/// screenshot.
const INITIAL_WIDTH: u32 = 1600;
const INITIAL_HEIGHT: u32 = 900;

/// Default base color used for the first few frames before any
/// `BackendEvent::OutputResized` has carried a theme through. Matches
/// `ui::theme::Theme::dark().bg_app` so the host window doesn't
/// flash white on boot. The compositor overrides this each frame via
/// [`WinitBackend::set_base_color`].
const FALLBACK_BG: Color = Color::from_rgb8(0x1A, 0x1B, 0x26);

pub struct WinitBackend {
    event_loop: EventLoop<()>,
    inner: Inner,
}

/// The lifecycle-aware half: everything `ApplicationHandler` needs to
/// touch lives here so `pump_app_events` can take `&mut self`. The
/// `EventLoop` itself stays in [`WinitBackend`] because winit's API
/// surfaces `pump_app_events` as a method on `&mut EventLoop`.
struct Inner {
    /// `None` until winit fires the first `resumed`. We can't create
    /// a `Window` before the event loop has run an iteration on
    /// platforms that enforce ActiveEventLoop semantics; deferring
    /// here keeps the same code path working on every supported
    /// platform.
    window: Option<Arc<Window>>,
    /// Constructed alongside the window in `resumed`. Wraps the wgpu
    /// instance/device that vello drives.
    render: Option<RenderState>,
    outputs: Vec<Output>,
    event_buffer: Vec<BackendEvent>,
    /// Background color used by `submit_frame`. Updated by callers
    /// through [`WinitBackend::set_base_color`] so the theme's
    /// `bg_app` doesn't leak into every render call.
    base_color: Color,
    /// Set when `pump_app_events` returns `PumpStatus::Exit` so we
    /// emit a single `CloseRequested` and stop pumping further.
    closed: bool,
}

struct RenderState {
    render_cx: RenderContext,
    surface: RenderSurface<'static>,
    renderer: Renderer,
}

impl WinitBackend {
    /// Build the event loop + initial state. The window itself is
    /// created lazily on the first `resumed` callback. Returning here
    /// only fails if winit can't construct the event loop at all.
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let event_loop = EventLoop::new()?;
        let outputs = vec![Output::new(
            OutputId(0),
            (INITIAL_WIDTH, INITIAL_HEIGHT),
            1.0,
        )];
        let mut me = Self {
            event_loop,
            inner: Inner {
                window: None,
                render: None,
                outputs,
                event_buffer: Vec::new(),
                base_color: FALLBACK_BG,
                closed: false,
            },
        };
        me.pump_until_resumed();
        Ok(me)
    }

    /// Pump the event loop until winit delivers `resumed` and the window
    /// + render state exist.
    ///
    /// The window is created lazily because `ActiveEventLoop` semantics
    /// forbid creating one before the loop has run an iteration. That
    /// laziness used to leak into everything the compositor asks a
    /// backend at startup: `outputs()` returned the placeholder size
    /// rather than the real window's, `refresh_interval()` had no monitor
    /// to ask, and `take_presenter()` found no render state and silently
    /// left presentation on the loop thread — a threaded compositor that
    /// wasn't. Draining the first `resumed` here makes construction mean
    /// "ready", so none of those three has a lazy case to get wrong.
    ///
    /// Bounded: if the platform never delivers `resumed` we return anyway
    /// and the backend behaves as it did before (lazy init on the first
    /// `poll_events`), rather than hanging the compositor at startup.
    fn pump_until_resumed(&mut self) {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while self.inner.window.is_none() && std::time::Instant::now() < deadline {
            let status = self
                .event_loop
                .pump_app_events(Some(Duration::from_millis(4)), &mut self.inner);
            if let PumpStatus::Exit(_) = status {
                self.inner.closed = true;
                return;
            }
        }
        if self.inner.window.is_none() {
            eprintln!(
                "nirios compositor: winit did not deliver `resumed` within 2s; \
                 falling back to lazy window creation"
            );
        }
    }

    /// Override the per-frame clear color. The compositor calls this
    /// each tick with `theme.bg_app` so changing themes at runtime is
    /// reflected immediately without touching the backend trait.
    pub fn set_base_color(&mut self, color: Color) {
        self.inner.base_color = color;
    }
}

/// The presentation half of [`WinitBackend`], moved onto the render
/// thread by [`Backend::take_presenter`].
///
/// Everything here is `Send`: the wgpu device/queue/surface and the
/// `Arc<Window>` all are. What is *not* `Send` — winit's `EventLoop`,
/// which must be pumped from the thread that created it — stays behind on
/// the backend, which is why the split is at the [`RenderState`] boundary
/// rather than at the whole backend.
pub struct WinitPresenter {
    render: RenderState,
    base_color: Color,
}

impl WinitPresenter {
    /// Render one frame into the swap chain and present it.
    ///
    /// Extracted verbatim from the old inline `submit_frame` so the
    /// pixels are unchanged by the move; only the thread it runs on
    /// differs. The blocking `device.poll` at the end is what paces this
    /// thread to the surface's `AutoVsync` present mode — and, now that
    /// it is off the input thread, blocking there costs a frame of
    /// throughput instead of a keystroke of latency.
    fn render_and_present(&mut self, scene: &Scene) {
        let base_color = self.base_color;
        let render = &mut self.render;
        let width = render.surface.config.width;
        let height = render.surface.config.height;
        let device_handle = &render.render_cx.devices[render.surface.dev_id];

        // vello 0.5 removed `render_to_surface`. The canonical replacement
        // is: render the scene into the surface's intermediate
        // `target_view` texture, then blit that into the swap-chain
        // texture via the bundled `TextureBlitter`, then present.
        let result = render.renderer.render_to_texture(
            &device_handle.device,
            &device_handle.queue,
            scene,
            &render.surface.target_view,
            &RenderParams {
                base_color,
                width,
                height,
                antialiasing_method: AaConfig::Area,
            },
        );
        if let Err(e) = result {
            eprintln!("nirios compositor: render_to_texture: {e:?}");
            return;
        }
        let surface_texture = match render.surface.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(tex) => tex,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Suboptimal(_) => {
                render.render_cx.configure_surface(&render.surface);
                return;
            }
            wgpu::CurrentSurfaceTexture::Occluded | wgpu::CurrentSurfaceTexture::Timeout => return,
            wgpu::CurrentSurfaceTexture::Lost => {
                eprintln!("nirios compositor: surface lost");
                return;
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                eprintln!("nirios compositor: surface validation error");
                return;
            }
        };
        let mut encoder =
            device_handle
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("nirios compositor blit"),
                });
        render.surface.blitter.copy(
            &device_handle.device,
            &mut encoder,
            &render.surface.target_view,
            &surface_texture
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default()),
        );
        device_handle.queue.submit([encoder.finish()]);
        surface_texture.present();
        // Block until this frame's GPU work completes so wgpu reclaims the
        // per-frame transient resources (vello scene-upload staging buffers,
        // the blit command buffer, the swap-chain texture view). A
        // non-blocking `Poll` here never observes the submission's fence
        // signal on the same tick, so those frees are deferred every frame
        // and the driver's allocations grow without bound — the nested
        // compositor climbed ~30 MB/s at idle and OOM'd within minutes.
        // The DRM backend (`drm/paint.rs`) and the offscreen renderer both
        // already wait here; this aligns the winit backend with them.
        let _ = device_handle
            .device
            .poll(wgpu::PollType::wait_indefinitely());
    }
}

impl Presenter for WinitPresenter {
    fn present(&mut self, scene: &Scene, _output: OutputId) {
        self.render_and_present(scene);
    }

    /// Reconfigure the swap chain for a new window size.
    ///
    /// With the render state moved onto the render thread, the resize can
    /// no longer happen inline in `Inner::window_event` — the surface
    /// isn't there any more. It arrives here through the same channel as
    /// frames, so a resize can never be applied out of order with respect
    /// to a frame already queued at the old size.
    fn resize(&mut self, size: (u32, u32)) {
        if size.0 == 0 || size.1 == 0 {
            return;
        }
        self.render
            .render_cx
            .resize_surface(&mut self.render.surface, size.0, size.1);
    }
}

impl Backend for WinitBackend {
    /// Inline presentation. Only reached when [`Backend::take_presenter`]
    /// was not called (or already handed the render state away) — the
    /// compositor normally submits through the render thread instead.
    fn submit_frame(&mut self, scene: &Scene, output: OutputId) {
        let base_color = self.inner.base_color;
        let Some(render) = self.inner.render.take() else {
            return;
        };
        let mut presenter = WinitPresenter { render, base_color };
        presenter.present(scene, output);
        self.inner.render = Some(presenter.render);
    }

    /// The host monitor's refresh rate. The nested-development backend is
    /// a Wayland *client* of the parent compositor, so what it can
    /// actually pace to is the parent's output — which is what
    /// `current_monitor` reports. When the parent doesn't publish a rate
    /// (some nested/headless parents don't), we report `None` and the
    /// compositor falls back rather than inventing one.
    fn refresh_interval(&self) -> Option<std::time::Duration> {
        let window = self.inner.window.as_ref()?;
        // Ask the monitor the window is on, then the primary, then any
        // monitor that publishes a rate. Parents differ in which of these
        // they populate — a nested headless `cage` publishes none of them,
        // which is why a nested run falls back rather than pacing to a
        // real panel.
        window
            .current_monitor()
            .into_iter()
            .chain(window.primary_monitor())
            .chain(window.available_monitors())
            .find_map(|m| m.refresh_rate_millihertz())
            .and_then(crate::compositor::pacing::interval_from_millihertz)
    }

    fn take_presenter(&mut self) -> Option<Box<dyn Presenter>> {
        let Some(render) = self.inner.render.take() else {
            // Construction is supposed to have drained `resumed`, so
            // there is no legitimate path here. Say so rather than
            // quietly leaving the GPU submit on the input thread — a
            // compositor that thinks it is threaded and isn't is the
            // worst of both.
            eprintln!(
                "nirios compositor: winit backend has no render state to hand \
                 to the render thread; presenting inline"
            );
            return None;
        };
        Some(Box::new(WinitPresenter {
            render,
            base_color: self.inner.base_color,
        }))
    }

    fn poll_events(&mut self, callback: &mut dyn FnMut(BackendEvent)) {
        if !self.inner.closed {
            let status = self
                .event_loop
                .pump_app_events(Some(Duration::ZERO), &mut self.inner);
            if let PumpStatus::Exit(_) = status {
                self.inner.closed = true;
                self.inner.event_buffer.push(BackendEvent::CloseRequested);
            }
        }
        for event in self.inner.event_buffer.drain(..) {
            callback(event);
        }
    }

    fn outputs(&self) -> &[Output] {
        &self.inner.outputs
    }
}

impl ApplicationHandler for Inner {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // `resumed` can fire more than once (suspend/resume cycles on
        // mobile); we treat the first as "init" and subsequent ones
        // as no-ops because our render state is independent of the
        // application lifecycle.
        if self.window.is_some() {
            return;
        }
        let attrs = WindowAttributes::default()
            .with_title("nirios")
            .with_inner_size(winit::dpi::PhysicalSize::new(INITIAL_WIDTH, INITIAL_HEIGHT));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("nirios compositor: create_window: {e:?}");
                event_loop.exit();
                return;
            }
        };
        let size = window.inner_size();
        let scale = window.scale_factor() as f32;

        let mut render_cx = RenderContext::new();
        let surface = match pollster::block_on(render_cx.create_surface(
            window.clone(),
            size.width,
            size.height,
            wgpu::PresentMode::AutoVsync,
        )) {
            Ok(mut s) => {
                s.config.format = s.config.format.add_srgb_suffix();
                let dh = &render_cx.devices[s.dev_id];
                s.surface.configure(&dh.device, &s.config);
                // vello's `create_surface` built a `TextureBlitter` for the
                // pre-srgb format; wgpu 29's TextureBlitter pipeline must
                // match the render pass's target format exactly, so we
                // rebuild it against the now-sRGB swap-chain format.
                s.blitter = wgpu::util::TextureBlitter::new(&dh.device, s.config.format);
                s
            }
            Err(e) => {
                eprintln!("nirios compositor: create_surface: {e:?}");
                event_loop.exit();
                return;
            }
        };
        let device_handle = &render_cx.devices[surface.dev_id];
        let renderer = match Renderer::new(
            &device_handle.device,
            RendererOptions {
                use_cpu: false,
                antialiasing_support: AaSupport::all(),
                num_init_threads: None,
                pipeline_cache: None,
            },
        ) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("nirios compositor: Renderer::new: {e:?}");
                event_loop.exit();
                return;
            }
        };

        if let Some(out) = self.outputs.first_mut() {
            out.size = (size.width, size.height);
            out.scale = scale;
        }
        // Mirror the initial size onto the layout pipeline so the very
        // first frame is at the correct viewport.
        self.event_buffer.push(BackendEvent::OutputResized {
            id: OutputId(0),
            size: (size.width, size.height),
            scale,
        });
        window.request_redraw();
        self.window = Some(window);
        self.render = Some(RenderState {
            render_cx,
            surface,
            renderer,
        });
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                self.event_buffer.push(BackendEvent::CloseRequested);
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                if size.width == 0 || size.height == 0 {
                    return;
                }
                // Only reachable on the inline path. Once the render
                // state has moved to the render thread this is `None`
                // and the resize travels through the present pipe
                // instead (`Presenter::resize`), which keeps it ordered
                // against frames already queued at the old size.
                if let Some(render) = self.render.as_mut() {
                    render
                        .render_cx
                        .resize_surface(&mut render.surface, size.width, size.height);
                }
                let scale = self
                    .window
                    .as_ref()
                    .map(|w| w.scale_factor() as f32)
                    .unwrap_or(1.0);
                if let Some(out) = self.outputs.first_mut() {
                    out.size = (size.width, size.height);
                    out.scale = scale;
                }
                self.event_buffer.push(BackendEvent::OutputResized {
                    id: OutputId(0),
                    size: (size.width, size.height),
                    scale,
                });
            }
            WindowEvent::Focused(focused) => {
                self.event_buffer.push(BackendEvent::Focus(focused));
            }
            WindowEvent::RedrawRequested => {
                // The compositor repaints every tick regardless of
                // `RedrawRequested`, so we forward the signal but
                // don't rely on it.
                self.event_buffer.push(BackendEvent::Redraw);
            }
            ev @ (WindowEvent::CursorMoved { .. }
            | WindowEvent::MouseInput { .. }
            | WindowEvent::KeyboardInput { .. }
            | WindowEvent::MouseWheel { .. }) => {
                // Carry the full event so `compositor::native::run`
                // can dispatch it through `FrameBuilder::dispatch_event`
                // — the same wire `execute_planned_action` uses for
                // headless / scripted input.
                self.event_buffer.push(BackendEvent::Input(ev));
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.event_buffer
                    .push(BackendEvent::ModifiersChanged(modifiers));
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // Our outer loop already paints every frame via
        // `frame_builder.paint`, so we don't need to request a
        // redraw here. Leaving this empty is intentional.
    }
}

// Note: no unit tests for `WinitBackend` itself — winit panics on
// non-main-thread `EventLoop::new`, which makes parallel `cargo test`
// runs unreliable. Phase 4's verification is the runtime smoke ("boot
// `--mode native`, host window paints chrome") and the wider ledger
// tests that exercise `FrameBuilder` directly. When Phase 6 splits
// `Inner` into a state-only struct independent of the event loop, we
// can add focused unit tests on it.
