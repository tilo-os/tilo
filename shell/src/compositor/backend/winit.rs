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
use vello::{wgpu, AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, Scene};
use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::platform::pump_events::{EventLoopExtPumpEvents, PumpStatus};
use winit::window::{Window, WindowAttributes, WindowId};

use super::{Backend, BackendEvent, Output, OutputId};

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
        let outputs = vec![Output {
            id: OutputId(0),
            size: (INITIAL_WIDTH, INITIAL_HEIGHT),
            scale: 1.0,
        }];
        Ok(Self {
            event_loop,
            inner: Inner {
                window: None,
                render: None,
                outputs,
                event_buffer: Vec::new(),
                base_color: FALLBACK_BG,
                closed: false,
            },
        })
    }

    /// Override the per-frame clear color. The compositor calls this
    /// each tick with `theme.bg_app` so changing themes at runtime is
    /// reflected immediately without touching the backend trait.
    pub fn set_base_color(&mut self, color: Color) {
        self.inner.base_color = color;
    }
}

impl Backend for WinitBackend {
    fn submit_frame(&mut self, scene: &Scene, _output: OutputId) {
        let inner = &mut self.inner;
        let Some(render) = inner.render.as_mut() else {
            return;
        };
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
                base_color: inner.base_color,
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
            wgpu::CurrentSurfaceTexture::Outdated
            | wgpu::CurrentSurfaceTexture::Suboptimal(_) => {
                render.render_cx.configure_surface(&render.surface);
                return;
            }
            wgpu::CurrentSurfaceTexture::Occluded
            | wgpu::CurrentSurfaceTexture::Timeout => return,
            wgpu::CurrentSurfaceTexture::Lost => {
                eprintln!("nirios compositor: surface lost");
                return;
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                eprintln!("nirios compositor: surface validation error");
                return;
            }
        };
        let mut encoder = device_handle
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
        let _ = device_handle.device.poll(wgpu::PollType::Poll);
    }

    fn poll_events(&mut self, callback: &mut dyn FnMut(BackendEvent)) {
        if !self.inner.closed {
            let status = self
                .event_loop
                .pump_app_events(Some(Duration::ZERO), &mut self.inner);
            if let PumpStatus::Exit(_) = status {
                self.inner.closed = true;
                self.inner
                    .event_buffer
                    .push(BackendEvent::CloseRequested);
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
            .with_inner_size(winit::dpi::PhysicalSize::new(
                INITIAL_WIDTH,
                INITIAL_HEIGHT,
            ));
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

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                self.event_buffer.push(BackendEvent::CloseRequested);
                event_loop.exit();
            }
            WindowEvent::Resized(size) => {
                if size.width == 0 || size.height == 0 {
                    return;
                }
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
                self.event_buffer.push(BackendEvent::ModifiersChanged(modifiers));
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
