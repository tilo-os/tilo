//! Drives the host window via vello+wgpu directly rather than smithay's
//! `WinitGraphicsBackend<GlowRenderer>` — vello renders through wgpu while
//! smithay's winit backend hands out an OpenGL/Glow surface, and the two
//! can't trivially share one. Smithay still owns the wayland-server
//! protocol state; this backend only owns the rendered window.
//!
//! Event integration uses winit 0.30's [`pump_app_events`][pump] so the
//! calloop event loop in [`crate::compositor::native::run`] stays the
//! driver: each iteration calls [`Backend::poll_events`], then
//! `frame_builder.paint`, then [`Backend::submit_frame`].
//!
//! [pump]: winit::platform::pump_events::EventLoopExtPumpEvents::pump_app_events

use std::sync::Arc;
use std::time::Duration;

use vello::peniko::Color;
// `ui::gpu`'s context, not `vello::util`'s — this window shares an
// integrated GPU with every other Tilo process and needs the same memory
// hint (see `crates/sdk/ui-core/src/gpu.rs`).
use ui::gpu::RenderContext;
use vello::util::RenderSurface;
use vello::{AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, Scene, wgpu};
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::platform::pump_events::{EventLoopExtPumpEvents, PumpStatus};
use winit::window::{Window, WindowAttributes, WindowId};

use super::apply_scale_factor_changed;
use super::{Backend, BackendEvent, Output, OutputId};
use crate::shell::present::Presenter;

/// Initial host-window size — matches the `shots` default so a fresh
/// `--mode native` launch is comparable to a `cargo run --bin shots`
/// screenshot.
const INITIAL_WIDTH: u32 = 1600;
const INITIAL_HEIGHT: u32 = 900;

/// Override for the initial window size, as `WxH`. Named rather than left
/// to a resize because resizing this backend isn't free: reconfiguring the
/// surface while a frame is in flight panics the present thread. Mirrors
/// `TILO_HEADLESS_SIZE`, which the headless backend reads for the same reason.
const SIZE_ENV: &str = "TILO_WINIT_SIZE";

/// Size the host window is created at; falls back to the defaults on
/// anything unparseable, zero, or absent — a typo should give the ordinary
/// window, never a zero-sized surface.
fn initial_size() -> (u32, u32) {
    let Ok(raw) = std::env::var(SIZE_ENV) else {
        return (INITIAL_WIDTH, INITIAL_HEIGHT);
    };
    let parsed = raw
        .split_once(['x', 'X'])
        .and_then(|(w, h)| Some((w.trim().parse().ok()?, h.trim().parse().ok()?)))
        .filter(|(w, h): &(u32, u32)| *w > 0 && *h > 0);
    if let Some(size) = parsed {
        size
    } else {
        eprintln!(
            "tilo compositor: {SIZE_ENV}={raw:?} is not a usable WxH; \
             using {INITIAL_WIDTH}x{INITIAL_HEIGHT}"
        );
        (INITIAL_WIDTH, INITIAL_HEIGHT)
    }
}

fn relative_motion_from_device_event(focused: bool, event: &DeviceEvent) -> Option<(f64, f64)> {
    let DeviceEvent::MouseMotion { delta: (dx, dy) } = event else {
        return None;
    };
    (focused && dx.is_finite() && dy.is_finite() && (*dx != 0.0 || *dy != 0.0))
        .then_some((*dx, *dy))
}

/// Base color for the first few frames before a theme arrives: a dark
/// desktop ground, so the window doesn't flash white on boot; overridden
/// each frame via [`WinitBackend::set_base_color`].
const FALLBACK_BG: Color = Color::from_rgb8(0x1A, 0x1B, 0x26);

pub struct WinitBackend {
    event_loop: EventLoop<()>,
    inner: Inner,
}

/// Lifecycle-aware half: everything `ApplicationHandler` touches lives here
/// so `pump_app_events` can take `&mut self`. `EventLoop` stays in
/// [`WinitBackend`] since winit exposes `pump_app_events` as a method on
/// `&mut EventLoop`.
struct Inner {
    /// `None` until the first `resumed` — some platforms enforce
    /// `ActiveEventLoop` semantics and forbid creating a `Window` earlier.
    window: Option<Arc<Window>>,
    /// Constructed alongside the window in `resumed`; wraps the wgpu
    /// instance/device vello drives.
    render: Option<RenderState>,
    outputs: Vec<Output>,
    event_buffer: Vec<BackendEvent>,
    /// Clear color used by `submit_frame`; updated via
    /// [`WinitBackend::set_base_color`].
    base_color: Color,
    /// Set when `pump_app_events` returns `Exit`, so we emit one
    /// `CloseRequested` and stop pumping.
    closed: bool,
    /// Raw device motion must never control Tilo while its hosted window is
    /// unfocused. Window events already obey that boundary; `DeviceEvent`
    /// deliberately does not, so we gate it explicitly.
    focused: bool,
    /// Clock for relative-motion timestamps; raw device events carry none.
    started: std::time::Instant,
}

struct RenderState {
    render_cx: RenderContext,
    surface: RenderSurface<'static>,
    renderer: Renderer,
    /// Blurs floating surfaces' backdrops; built with the first frame that
    /// has any.
    backdrop_blur: Option<ui::backdrop::BackdropBlur>,
}

impl WinitBackend {
    /// Build the event loop + initial state; the window is created lazily
    /// on the first `resumed` callback.
    ///
    /// # Errors
    ///
    /// Returns an error if winit fails to create the platform event loop
    /// (e.g. no display connection available).
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let event_loop = EventLoop::new()?;
        let outputs = vec![Output::new(OutputId(0), initial_size(), 1.0)];
        let mut me = Self {
            event_loop,
            inner: Inner {
                window: None,
                render: None,
                outputs,
                event_buffer: Vec::new(),
                base_color: FALLBACK_BG,
                closed: false,
                focused: false,
                started: std::time::Instant::now(),
            },
        };
        me.pump_until_resumed();
        Ok(me)
    }

    /// Pump the event loop until winit delivers `resumed` and the window +
    /// render state exist. `ActiveEventLoop` semantics forbid creating a
    /// window before the loop has run an iteration, so draining the first
    /// `resumed` here makes construction mean "ready" for callers like
    /// `outputs()` and `take_presenter()`.
    ///
    /// Bounded: if the platform never delivers `resumed`, returns after 2s
    /// and falls back to lazy init on the first `poll_events`, rather than
    /// hanging the compositor at startup.
    fn pump_until_resumed(&mut self) {
        // `checked_add` sidesteps a theoretical `Instant` overflow (would
        // need centuries of uptime); falling back to "now" just makes the
        // bounded wait below a no-op instead of panicking.
        let deadline = std::time::Instant::now()
            .checked_add(Duration::from_secs(2))
            .unwrap_or_else(std::time::Instant::now);
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
                "tilo compositor: winit did not deliver `resumed` within 2s; \
                 falling back to lazy window creation"
            );
        }
    }

    /// Override the per-frame clear color; the compositor calls this each
    /// tick with the desktop ground (`surfaces.base`) so theme changes apply without touching the
    /// backend trait.
    pub fn set_base_color(&mut self, color: Color) {
        self.inner.base_color = color;
    }
}

/// Presentation half of [`WinitBackend`], moved onto the render thread by
/// [`Backend::take_presenter`]. Everything here is `Send`; winit's
/// `EventLoop` isn't (must be pumped from its creating thread), which is
/// why the split sits at the `RenderState` boundary.
pub struct WinitPresenter {
    render: RenderState,
    base_color: Color,
}

impl WinitPresenter {
    /// Render one frame into the swap chain and present it. The blocking
    /// `device.poll` at the end paces this thread to the surface's
    /// `AutoVsync` present mode; off the input thread, that costs a frame
    /// of throughput rather than a keystroke of latency.
    fn render_and_present(
        &mut self,
        scene: &Scene,
        backdrop: Option<&ui::backdrop::BackdropFrame>,
    ) {
        let base_color = self.base_color;
        let render = &mut self.render;
        let width = render.surface.config.width;
        let height = render.surface.config.height;
        // `dev_id` is minted by `RenderContext::create_surface` alongside
        // this very surface, so a miss here would mean device state was
        // dropped out from under us — skip the frame rather than panic.
        let Some(device_handle) = render.render_cx.devices.get(render.surface.dev_id) else {
            eprintln!(
                "tilo compositor: render_and_present: no device handle for dev_id {}",
                render.surface.dev_id
            );
            return;
        };

        if let Some(backdrop) = backdrop.filter(|frame| !frame.is_empty()) {
            render
                .backdrop_blur
                .get_or_insert_with(|| ui::backdrop::BackdropBlur::new(&device_handle.device))
                .prepare(
                    &device_handle.device,
                    &device_handle.queue,
                    &mut render.renderer,
                    backdrop,
                    (width, height),
                    base_color,
                );
        }
        // vello 0.5 removed `render_to_surface`; render into the surface's
        // target_view, blit via `TextureBlitter`, then present.
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
        if let Some(blur) = render.backdrop_blur.as_mut() {
            blur.release(&mut render.renderer);
        }
        if let Err(e) = result {
            eprintln!("tilo compositor: render_to_texture: {e:?}");
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
                eprintln!("tilo compositor: surface lost");
                return;
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                eprintln!("tilo compositor: surface validation error");
                return;
            }
        };
        let mut encoder =
            device_handle
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("tilo compositor blit"),
                });
        ui::gpu::draw_target_to_surface(
            &device_handle.device,
            &mut encoder,
            &render.surface,
            &surface_texture.texture,
        );
        device_handle.queue.submit([encoder.finish()]);
        surface_texture.present();
        // Block until this frame's GPU work completes so wgpu reclaims
        // per-frame transient resources — a non-blocking poll never
        // observes the fence on the same tick, so frees are deferred every
        // frame and allocations grow without bound (observed: ~30 MB/s at
        // idle, OOM within minutes).
        //
        // Bounded, not indefinite: a fence for a frame in flight across a
        // system suspend/resume can simply never signal, wedging this
        // thread forever. One second covers any real frame; on timeout we
        // log and carry on — the stale swapchain surfaces as `Outdated` on
        // the next acquire and reconfigure brings presentation back.
        let waited = device_handle.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(Duration::from_secs(1)),
        });
        if let Err(e) = waited {
            eprintln!("tilo compositor: frame fence wait: {e:?} — carrying on");
        }
    }
}

impl Presenter for WinitPresenter {
    fn present(&mut self, scene: &Scene, _output: OutputId) {
        self.render_and_present(scene, None);
    }

    fn present_frame(&mut self, frame: &crate::shell::frame::FramePaint, _output: OutputId) {
        self.render_and_present(&frame.scene, frame.backdrop.as_deref());
    }

    /// Reconfigure the swap chain for a new window size. Arrives through
    /// the same channel as frames, so a resize can never apply out of order
    /// with respect to a frame already queued at the old size.
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
    /// The host session draws the pointer over this window, so the
    /// compositor must not paint a second one into the scene.
    fn has_cursor_plane(&self) -> bool {
        true
    }

    /// The host draws the pointer, so pass it the name of the shape; a
    /// client-drawn cursor shows as the host's arrow.
    /// Ask the host to hold or confine its pointer to this window, so a
    /// locked game keeps receiving motion. Best effort: a host may refuse.
    fn set_pointer_constraint(
        &mut self,
        constraint: crate::compositor::backend::PointerConstraintRegion,
    ) {
        use crate::compositor::backend::PointerConstraintRegion;
        use winit::window::CursorGrabMode;
        let Some(window) = self.inner.window.as_ref() else {
            return;
        };
        let mode = match constraint {
            PointerConstraintRegion::Free => CursorGrabMode::None,
            PointerConstraintRegion::Locked => CursorGrabMode::Locked,
            PointerConstraintRegion::Confined(_) => CursorGrabMode::Confined,
        };
        if let Err(error) = window.set_cursor_grab(mode) {
            tracing::debug!(%error, "host refused the pointer constraint");
        }
    }

    fn set_pointer_image(&mut self, image: crate::compositor::cursor::PointerImage) {
        use crate::compositor::cursor::{CursorShape, PointerImage};
        let Some(window) = self.inner.window.as_ref() else {
            return;
        };
        match image {
            PointerImage::Hidden => window.set_cursor_visible(false),
            PointerImage::Shape(shape) => {
                window.set_cursor_visible(true);
                window.set_cursor(shape.icon());
            }
            PointerImage::Client(_) => {
                window.set_cursor_visible(true);
                window.set_cursor(CursorShape::Default.icon());
            }
        }
    }

    /// Inline presentation fallback — only reached when
    /// [`Backend::take_presenter`] was never called or already took the
    /// render state.
    fn submit_frame(&mut self, scene: &Scene, output: OutputId) {
        let base_color = self.inner.base_color;
        let Some(render) = self.inner.render.take() else {
            return;
        };
        let mut presenter = WinitPresenter { render, base_color };
        presenter.present(scene, output);
        self.inner.render = Some(presenter.render);
    }

    fn submit_frame_with_damage(
        &mut self,
        frame: &crate::shell::frame::FramePaint,
        output: OutputId,
    ) {
        let base_color = self.inner.base_color;
        let Some(render) = self.inner.render.take() else {
            return;
        };
        let mut presenter = WinitPresenter { render, base_color };
        presenter.present_frame(frame, output);
        self.inner.render = Some(presenter.render);
    }

    /// Host monitor's refresh rate. This backend is a Wayland *client* of
    /// the parent compositor, so it can only pace to what `current_monitor`
    /// reports; when the parent doesn't publish a rate, we report `None`
    /// rather than inventing one.
    fn refresh_interval(&self) -> Option<std::time::Duration> {
        let window = self.inner.window.as_ref()?;
        // Try the window's monitor, then primary, then any monitor with a
        // rate — a nested headless `cage` publishes none, hence the fallback.
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
            // No legitimate path here — construction drains `resumed`. Log
            // rather than quietly leaving the GPU submit on the input thread.
            eprintln!(
                "tilo compositor: winit backend has no render state to hand \
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
        // `resumed` can fire more than once (mobile suspend/resume); only
        // the first is init since render state is independent of the app
        // lifecycle.
        if self.window.is_some() {
            return;
        }
        let attrs = WindowAttributes::default()
            .with_title("tilo")
            .with_inner_size({
                let (w, h) = initial_size();
                winit::dpi::PhysicalSize::new(w, h)
            });
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("tilo compositor: create_window: {e:?}");
                event_loop.exit();
                return;
            }
        };
        let size = window.inner_size();
        // A UI scale factor needs nowhere near f64's precision; the lossy
        // cast is intentional, not a truncation bug.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "monitor scale factor; f32 precision is ample for a UI scale value"
        )]
        let scale = window.scale_factor() as f32;

        let mut render_cx = RenderContext::new();
        let surface = match pollster::block_on(render_cx.create_surface(
            window.clone(),
            size.width,
            size.height,
            wgpu::PresentMode::AutoVsync,
        )) {
            Ok(s) => {
                // Present in the format vello picked — deliberately NOT an
                // `*Srgb` one. vello renders into an intermediate texture
                // already sRGB-encoded (`peniko::Color` is
                // `AlphaColor<Srgb>`), then blits to the swap-chain; forcing
                // `*Srgb` there double-encodes and washes out every surface
                // (e.g. (22,23,27) presents as #53555B).
                s
            }
            Err(e) => {
                eprintln!("tilo compositor: create_surface: {e:?}");
                event_loop.exit();
                return;
            }
        };
        // `dev_id` is minted by `create_surface` just above, alongside this
        // very surface — a miss here means device state vanished under us.
        let Some(device_handle) = render_cx.devices.get(surface.dev_id) else {
            eprintln!(
                "tilo compositor: resumed: no device handle for dev_id {}",
                surface.dev_id
            );
            event_loop.exit();
            return;
        };
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
                eprintln!("tilo compositor: Renderer::new: {e:?}");
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
            backdrop_blur: None,
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
                // A `Resized` can arrive with nothing actually changed
                // (fullscreen at the current size, a WM re-asserting
                // geometry); reconfiguring anyway panics the present thread
                // if a frame is in flight, so bail out on no-op resizes.
                // Scale is compared too — same pixel size at a new scale
                // factor is a real change.
                #[allow(
                    clippy::cast_possible_truncation,
                    reason = "monitor scale factor; f32 precision is ample for a UI scale value"
                )]
                let incoming_scale = self
                    .window
                    .as_ref()
                    .map_or(1.0, |w| w.scale_factor() as f32);
                // Exact equality is intentional: both `o.scale` and
                // `incoming_scale` come from the identical
                // `scale_factor() as f32` conversion, so an unchanged
                // scale compares bit-for-bit equal. An epsilon here would
                // risk treating a real (if tiny) scale change as a no-op.
                #[allow(
                    clippy::float_cmp,
                    reason = "both sides are `scale_factor() as f32`; exact equality is the correct no-op check"
                )]
                if self.outputs.first().is_some_and(|o| {
                    o.size == (size.width, size.height) && o.scale == incoming_scale
                }) {
                    return;
                }
                // Only reachable on the inline path — once render state
                // moves to the render thread, this is `None` and the resize
                // travels through `Presenter::resize` instead.
                if let Some(render) = self.render.as_mut() {
                    render
                        .render_cx
                        .resize_surface(&mut render.surface, size.width, size.height);
                }
                let scale = incoming_scale;
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
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                if let Some(out) = self.outputs.first_mut()
                    && let Some(event) = apply_scale_factor_changed(out, scale_factor)
                {
                    self.event_buffer.push(event);
                }
            }
            WindowEvent::Focused(focused) => {
                self.focused = focused;
                self.event_buffer.push(BackendEvent::Focus(focused));
            }
            WindowEvent::RedrawRequested => {
                // Compositor repaints every tick regardless; forwarded but not relied on.
                self.event_buffer.push(BackendEvent::Redraw);
            }
            ev @ (WindowEvent::CursorMoved { .. }
            | WindowEvent::MouseInput { .. }
            | WindowEvent::KeyboardInput { .. }
            | WindowEvent::MouseWheel { .. }) => {
                // A nested window sees one host keyboard and pointer, so
                // the event class is all it can report about the devices.
                self.event_buffer.push(match ev {
                    WindowEvent::CursorMoved { .. } => BackendEvent::PointerMotionUsed,
                    WindowEvent::KeyboardInput { .. } => {
                        BackendEvent::InputUsed(ui::InputModality::Keyboard)
                    }
                    _ => BackendEvent::InputUsed(ui::InputModality::Pointer),
                });
                // Carried whole so `compositor::native::run` can dispatch
                // via `FrameBuilder::dispatch_event` — same wire scripted
                // input uses.
                self.event_buffer.push(BackendEvent::Input(ev));
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                self.event_buffer
                    .push(BackendEvent::ModifiersChanged(modifiers));
            }
            _ => {}
        }
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: DeviceId,
        event: DeviceEvent,
    ) {
        if let Some((dx, dy)) = relative_motion_from_device_event(self.focused, &event) {
            // Unlike `CursorMoved`, raw motion keeps arriving after the host
            // cursor reaches a window edge. This gives the nested/dev backend
            // the same four-edge cross-device control contract as DRM.
            // Raw device motion is already unaccelerated.
            self.event_buffer.push(BackendEvent::RelativePointerMotion {
                dx,
                dy,
                dx_unaccel: dx,
                dy_unaccel: dy,
                time_usec: u64::try_from(self.started.elapsed().as_micros()).unwrap_or(u64::MAX),
            });
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // Outer loop already paints every frame via `frame_builder.paint`;
        // empty here is intentional.
    }
}

// The backend's event-loop construction is not unit-tested — winit panics on
// non-main-thread `EventLoop::new`, making parallel `cargo test` runs
// unreliable.  The event-to-backend state transition above is pure and is
// covered without constructing an event loop.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scale_factor_change_reaches_the_output_once_and_rejects_invalid_values() {
        // Phase `scale_factor_change_updates_output_without_a_pixel_resize`.
        {
            let mut output = Output::new(OutputId(7), (1920, 1080), 1.0);

            let event = apply_scale_factor_changed(&mut output, 1.5)
                .expect("a real DPI change must reach the compositor");

            assert_eq!(output.scale, 1.5);
            assert!(matches!(
                event,
                BackendEvent::OutputResized {
                    id: OutputId(7),
                    size: (1920, 1080),
                    scale: 1.5,
                }
            ));
        }
        // Phase `duplicate_or_invalid_scale_changes_are_ignored`.
        {
            let mut output = Output::new(OutputId(0), (1280, 720), 1.25);
            assert!(apply_scale_factor_changed(&mut output, 1.25).is_none());
            assert!(apply_scale_factor_changed(&mut output, 0.0).is_none());
            assert!(apply_scale_factor_changed(&mut output, f64::NAN).is_none());
            assert_eq!(output.scale, 1.25);
        }
    }

    #[test]
    fn raw_motion_is_forwarded_only_while_the_host_window_is_focused() {
        let motion = DeviceEvent::MouseMotion { delta: (-4.5, 2.0) };
        assert_eq!(
            relative_motion_from_device_event(true, &motion),
            Some((-4.5, 2.0))
        );
        assert_eq!(relative_motion_from_device_event(false, &motion), None);
        assert_eq!(
            relative_motion_from_device_event(
                true,
                &DeviceEvent::MouseMotion {
                    delta: (f64::NAN, 1.0),
                },
            ),
            None
        );
        assert_eq!(
            relative_motion_from_device_event(
                true,
                &DeviceEvent::MouseMotion { delta: (0.0, 0.0) },
            ),
            None
        );
    }
}
