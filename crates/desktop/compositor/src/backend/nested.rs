//! The compositor in a window of the desktop it runs inside, for development.
//! The window is winit's: a Wayland client where the desktop runs Wayland and an X11 window where
//! it does not, so the session is a native window either way. The window is one output; resizing it
//! changes its mode, and the output draws at the desktop's own scale (fractional included), which
//! it follows as the window moves between the desktop's screens. Frames are composited with GLES on
//! the GPU that draws the window and presented to the host with its damage. Pointer and keyboard
//! input of the window becomes [`InputEvent`]s; the pointer is drawn as on a real screen, in place
//! of the host's.
//! A window whose graphics are lost (a failed present, a refused surface) gets them made again on
//! the same window while the session runs on ([`recovery`]), for as long as it takes.

mod graphics;
mod recovery;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use smithay::backend::allocator::Fourcc;
use smithay::backend::egl::{EGLDevice, EGLDisplay};
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::{
    Bind as _, ExportMem as _, ImportDma as _, ImportMemWl as _, Offscreen as _,
};
use smithay::backend::winit::{WinitEvent, init_from_attributes};
use smithay::output::{Mode, Output, Scale};
use smithay::reexports::calloop::LoopHandle;
use smithay::utils::{Buffer, Physical, Point, Rectangle, Size, Transform};
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::window::{Window as WinitWindow, WindowAttributes};

use super::{Backend, BackendError, Screenshot, failed, refuse_disabling};
use crate::input::InputEvent;
use crate::output::{OutputChange, OutputConfig, apply_placement, refresh_interval};
use crate::present::Shown;
use crate::render::{BACKGROUND, OutputRenderElement, output_elements};
use crate::state::State;
use crate::{Error, LoopData};
use graphics::{HostRef, SharedEvents, WindowGraphics};
use recovery::{Frame, Painter, Recovery};
use tilo_session_core::surface::{BackdropImage, BackdropRequest};

/// The name of the window's output.
pub const OUTPUT_NAME: &str = "NESTED-1";

/// The refresh the window's output reports; the host decides the real one.
const REFRESH_MHZ: i32 = 60_000;

/// How the window starts.
#[derive(Clone, Debug, PartialEq)]
pub struct NestedConfig {
    /// The window's first size, in the host's logical pixels, or in physical pixels when
    /// [`Self::logical`] is off.
    pub size: (u16, u16),
    /// Whether [`Self::size`] is in the host's logical pixels: the window then opens at the same
    /// apparent size on a desktop at any scale, with the host turning it into the pixels it shows.
    /// Off for a caller that means pixels (`tilo-compositor --size`).
    pub logical: bool,
    /// The scale to draw at when the host reports none (`tilo-compositor --scale`): a Wayland
    /// desktop that says its preferred scale otherwise decides it, fractional included.
    pub scale: f64,
    /// The window's title.
    pub title: String,
}

impl Default for NestedConfig {
    fn default() -> Self {
        Self {
            size: (1280, 800),
            logical: false,
            scale: 1.0,
            title: "Tilo".into(),
        }
    }
}

impl NestedConfig {
    /// The scale the output draws at until the host says its own: the configured one if it is
    /// usable, else 1.
    fn configured_scale(&self) -> f64 {
        usable_scale(self.scale).unwrap_or(1.0)
    }

    fn attributes(&self) -> WindowAttributes {
        let attributes = WinitWindow::default_attributes().with_title(&self.title);
        let (width, height) = (u32::from(self.size.0), u32::from(self.size.1));
        if self.logical {
            attributes.with_inner_size(LogicalSize::new(width, height))
        } else {
            attributes.with_inner_size(PhysicalSize::new(width, height))
        }
        .with_visible(true)
    }
}

/// The scale the window's output draws at, from what the host reports: the desktop's own scale for
/// the screen the window is on, fractional included (a Tilo desktop at 200% tells a client 2.0, and
/// one at 175% 1.75 through `wp_fractional_scale_v1`). A host that reports no usable scale gets the
/// configured one, so a nested session still draws at the density a caller named.
fn host_scale(reported: f64, configured: f64) -> f64 {
    usable_scale(reported).unwrap_or(configured)
}

/// The session's window on the desktop it runs inside, opened before the session builds anything
/// else.
/// A session started in a window is handed the desktop as `WAYLAND_SOCKET` : one connected socket,
/// which a client takes once (`wl_display_connect` removes the variable). A session that builds its
/// up its shell first — the surface apps' renderers ask Vulkan for an adapter, and Mesa's device-select
/// layer connects to the desktop to pick it — leaves the window a connection that is already spent,
/// and the window then opens on no desktop at all. So the window is opened first, while the
/// handover is still there, and handed over ([`NestedBackend::start`]).
pub struct HostWindow {
    // Dropped before the window they draw into.
    graphics: WindowGraphics,
    display: EGLDisplay,
    host: HostRef,
    events: SharedEvents,
}

/// A scale clients can render at: finite and positive.
pub(crate) fn usable_scale(scale: f64) -> Option<f64> {
    (scale.is_finite() && scale > 0.0).then_some(scale)
}

/// The output mode of a window of `size` physical pixels, if it has any.
fn window_mode(size: Size<i32, Physical>) -> Option<Mode> {
    (size.w > 0 && size.h > 0).then(|| Mode {
        size: (size.w, size.h).into(),
        refresh: REFRESH_MHZ,
    })
}

/// Offer `mode` as the one size `output` has: the window is the mode, so the sizes it had before a
/// resize are no displays to offer — picking one would ask the window for a size it does not have.
fn one_mode(output: &Output, mode: Mode) {
    for stale in output.modes().into_iter().filter(|known| *known != mode) {
        output.delete_mode(stale);
    }
}

struct Inner {
    /// What draws into the window; none while it is being made again, or once the window is given
    /// up. Dropped before the window.
    graphics: Option<WindowGraphics>,
    /// The display the window's graphics are made on, kept across their losses: its formats are the
    /// ones clients were offered.
    display: EGLDisplay,
    host: HostRef,
    recovery: Recovery,
    /// When the lost graphics are next made again; none while the window shows.
    rebuild_due: Option<Instant>,
    output: Output,
    damage: OutputDamageTracker,
    /// When a frame may be drawn though the last one changed nothing.
    next_frame: Instant,
    closed: bool,
    /// Switched off: the window keeps its last frame and nothing is drawn.
    dark: bool,
}

/// The window backend. Created by [`NestedBackend::start`] from the window
/// [`NestedBackend::open_window`] opened: it makes that window the output and registers the host's
/// event loop in the compositor's.
pub struct NestedBackend {
    inner: Rc<RefCell<Inner>>,
}

impl NestedBackend {
    /// Open the session's window on the desktop it runs inside, to hand to
    /// [`NestedBackend::start`].
    ///
    /// # Errors
    /// The desktop refused the session, or has no connection for it.
    pub fn open_window(config: &NestedConfig) -> Result<HostWindow, Error> {
        let (mut window, events) = init_from_attributes::<GlesRenderer>(config.attributes())
            .map_err(|error| failed("window", error))?;
        let display = window.renderer().egl_context().display().clone();
        let events = Rc::new(RefCell::new(events));
        // SAFETY: smithay's window backend and its event loop hold the same window.
        let host = unsafe { HostRef::new(&events, window.window()) };
        // smithay's surface on the window goes before the session's own is made: EGL makes one
        // surface per window at a time.
        drop(window);
        let graphics = WindowGraphics::new(&host, &display)
            .map_err(|error| failed("window graphics", error))?;
        Ok(HostWindow {
            graphics,
            display,
            host,
            events: SharedEvents(events),
        })
    }

    /// Make a window the desktop opened ([`NestedBackend::open_window`]) an output.
    pub fn start(
        handle: &LoopHandle<'static, LoopData>,
        state: &mut State,
        config: &NestedConfig,
        host: HostWindow,
    ) -> Result<Self, Error> {
        let HostWindow {
            mut graphics,
            display,
            host,
            events,
        } = host;
        offer_renderer(state, graphics.renderer());

        let window = host.window();
        let size = graphics::window_size(window);
        // The host's own scale is the output's: a desktop at 200% tells this window 2.0, and the
        // session draws the pixels that desktop shows, so the shell is as crisp there as it is on
        // the device itself. Configured only for a host that reports nothing usable.
        let scale = host_scale(window.scale_factor(), config.configured_scale());
        let mut output_config =
            OutputConfig::new(OUTPUT_NAME, size.w.max(1), size.h.max(1)).with_scale(scale);
        output_config.refresh_mhz = REFRESH_MHZ;
        let output = output_config.create(&state.display, "nested", Point::from((0, 0)));
        state.add_output(&output, Point::from((0, 0)));
        let damage = window_damage(&output);
        // The compositor draws the pointer: clients' cursor shapes show.
        window.set_cursor_visible(false);

        let inner = Rc::new(RefCell::new(Inner {
            graphics: Some(graphics),
            display,
            host,
            recovery: Recovery::default(),
            rebuild_due: None,
            output,
            damage,
            next_frame: Instant::now(),
            closed: false,
            dark: false,
        }));
        let host = Rc::clone(&inner);
        handle
            .insert_source(events, move |event, (), data| {
                handle_event(&host, &mut data.state, event);
            })
            .map_err(|error| failed("window events", error.error))?;
        Ok(Self { inner })
    }
}

/// The tracker drawing the scene into the window.
/// A window's framebuffer is counted from its bottom edge, the opposite of the physical space the
/// scene is laid out in, so the scene goes into it flipped: a window rendering its output turned by
/// `transform` is drawn as [`flip`]`(transform)`. Smithay's own window example draws the same way.
fn window_damage(output: &Output) -> OutputDamageTracker {
    let Some(mode) = output.current_mode() else {
        return OutputDamageTracker::from_output(output);
    };
    let scale = output.current_scale().fractional_scale();
    let transform = flip(output.current_transform());
    OutputDamageTracker::new(mode.size, scale, transform)
}

/// The same turn of the output with one more flip: what drawing into the window adds to the scene.
const fn flip(transform: Transform) -> Transform {
    match transform {
        Transform::Normal => Transform::Flipped180,
        Transform::Flipped180 => Transform::Normal,
        Transform::_90 => Transform::Flipped90,
        Transform::Flipped90 => Transform::_90,
        Transform::_180 => Transform::Flipped,
        Transform::Flipped => Transform::_180,
        Transform::_270 => Transform::Flipped270,
        Transform::Flipped270 => Transform::_270,
    }
}

/// Offer clients what the window's renderer takes: the dma-buf formats it imports (on the GPU that
/// draws the window, where clients then allocate), the shm formats it draws, and the dma-buf
/// targets capture clients may hand it. Done again for each renderer the window gets: the display
/// stays, so the formats do too, and what was offered stays offered.
fn offer_renderer(state: &mut State, renderer: &GlesRenderer) {
    let node = EGLDevice::device_for_display(renderer.egl_context().display())
        .ok()
        .and_then(|device| device.try_get_render_node().ok().flatten());
    let formats: Vec<_> = renderer.dmabuf_formats().iter().copied().collect();
    let render_formats: Vec<_> = renderer
        .egl_context()
        .dmabuf_render_formats()
        .iter()
        .copied()
        .collect();
    state.enable_dmabuf(formats, node);
    state.offer_shm_formats(renderer.shm_formats());
    if let Some(node) = node {
        state.enable_dmabuf_capture(node.dev_id(), render_formats);
    }
}

/// One frame of the window, as [`recovery::frame`] drives it.
struct WindowFrame<'a> {
    graphics: &'a mut Option<WindowGraphics>,
    display: &'a EGLDisplay,
    host: &'a HostRef,
    output: &'a Output,
    damage: &'a mut OutputDamageTracker,
    state: &'a mut State,
}

impl Painter for WindowFrame<'_> {
    fn present(&mut self) -> Result<(), String> {
        let graphics = self
            .graphics
            .as_mut()
            .ok_or_else(|| "the window has no graphics".to_owned())?;
        let window = self.host.window();
        let age = graphics.buffer_age();
        let (damage, states) = {
            let (renderer, mut framebuffer) = graphics.bind(window)?;
            let mut elements: Vec<OutputRenderElement<GlesRenderer>> =
                self.state.cursor_elements(renderer, self.output);
            elements.extend(output_elements(renderer, self.state, self.output));
            let drawn = self
                .damage
                .render_output(renderer, &mut framebuffer, age, &elements, BACKGROUND)
                .map_err(|error| format!("render window: {error}"))?;
            (drawn.damage.cloned(), drawn.states)
        };
        // Clients draw their next frame now. A frame the host does not take is dropped unshown,
        // which tells its clients it was discarded.
        let presentation = self.state.frame_drawn(self.output, Some(&states));
        graphics.submit(window, damage.as_deref())?;
        presentation.shown(self.state, Shown::Host);
        Ok(())
    }

    fn rebuild(&mut self) -> Result<(), String> {
        // The lost surface goes first: the window takes one at a time.
        *self.graphics = None;
        let mut graphics = WindowGraphics::new(self.host, self.display)?;
        offer_renderer(self.state, graphics.renderer());
        // The new back buffers hold nothing, and what the tracker knew was drawn into the old
        // ones: the first frame draws everything. Client buffers are imported into the new
        // renderer as they are drawn, from the buffers the surfaces still hold.
        *self.damage = window_damage(self.output);
        *self.graphics = Some(graphics);
        Ok(())
    }
}

fn handle_event(inner: &Rc<RefCell<Inner>>, state: &mut State, event: WinitEvent) {
    match event {
        WinitEvent::Input(event) => {
            let extent = state
                .output_geometries()
                .first()
                .map_or_else(|| (1, 1).into(), |geometry| geometry.size);
            if let Some(event) = InputEvent::from_device(event, extent) {
                state.handle_input(event);
            }
        }
        WinitEvent::Resized { size, scale_factor } => {
            let mut inner = inner.borrow_mut();
            let Some(mode) = window_mode(size) else {
                return;
            };
            // The desktop's scale is the output's: as the window moves to a screen at another
            // density (or the person changes it), the session draws the pixels that screen shows.
            // A scale the host reports that is not usable leaves the output at the one it has.
            let scale = usable_scale(scale_factor).map(Scale::Fractional);
            inner
                .output
                .change_current_state(Some(mode), None, scale, None);
            one_mode(&inner.output, mode);
            inner.output.set_preferred(mode);
            inner.damage = window_damage(&inner.output);
            inner.next_frame = Instant::now();
            let output = inner.output.clone();
            drop(inner);
            state.mark_redraw(&output, tilo_frame_metrics::paint_reason::PRESENTATION);
            state.rearrange();
        }
        WinitEvent::Redraw => {
            let mut inner = inner.borrow_mut();
            inner.next_frame = Instant::now();
            let output = inner.output.clone();
            drop(inner);
            state.mark_redraw(&output, tilo_frame_metrics::paint_reason::PRESENTATION);
        }
        WinitEvent::CloseRequested => inner.borrow_mut().closed = true,
        WinitEvent::Focus(_) => {}
    }
}

impl Backend for NestedBackend {
    fn describe(&self) -> &'static str {
        "nested"
    }

    fn render(&mut self, state: &mut State) -> Result<(), BackendError> {
        let mut guard = self.inner.borrow_mut();
        let inner = &mut *guard;
        let now = Instant::now();
        if inner.closed || inner.dark {
            return Ok(());
        }
        // Lost graphics are made again when due, whether or not anything changed.
        let rebuild = inner.rebuild_due.is_some_and(|due| now >= due);
        if now < inner.next_frame && !rebuild {
            return Ok(());
        }
        // Nothing new to show: an idle session draws nothing.
        if !state.take_redraw(&inner.output) && !rebuild {
            return Ok(());
        }
        // With nothing new on screen the window is asked again one refresh later.
        let next = now
            .checked_add(refresh_interval(
                inner
                    .output
                    .current_mode()
                    .map_or(REFRESH_MHZ, |mode| mode.refresh),
            ))
            .unwrap_or(now);
        inner.next_frame = next;
        let shown = recovery::frame(
            &mut inner.recovery,
            &mut WindowFrame {
                graphics: &mut inner.graphics,
                display: &inner.display,
                host: &inner.host,
                output: &inner.output,
                damage: &mut inner.damage,
                state: &mut *state,
            },
            now,
        );
        match shown {
            Frame::Presented => inner.rebuild_due = None,
            Frame::Lost(due) => {
                // The window shows nothing while its graphics are made again. Its clients are
                // answered as they draw, as on a headless output, and hear that those frames were
                // not shown. The loop wakes for the rebuild at `due`, whose first frame draws the
                // whole scene; nothing else wakes it while the session is idle.
                drop(state.frame_drawn(&inner.output, None));
                inner.rebuild_due = Some(due);
            }
        }
        Ok(())
    }

    fn backdrop(
        &mut self,
        state: &mut State,
        region: (usize, usize),
        request: &BackdropRequest,
    ) -> Result<Option<BackdropImage>, BackendError> {
        let mut inner = self.inner.borrow_mut();
        let renderer = inner.graphics.as_mut().ok_or_else(no_graphics)?.renderer();
        crate::backdrop::draw(renderer, state, region, request)
    }

    fn capture(
        &mut self,
        state: &mut State,
        frame: &crate::capture::Frame,
    ) -> Result<crate::capture::Copied, crate::capture::CaptureError> {
        let mut inner = self.inner.borrow_mut();
        let renderer = inner
            .graphics
            .as_mut()
            .ok_or_else(|| crate::capture::CaptureError::Backend(no_graphics()))?
            .renderer();
        crate::capture::copy::<_, smithay::backend::renderer::gles::GlesTexture>(
            renderer, state, frame,
        )
    }

    fn read_back(
        &mut self,
        state: &mut State,
        target: &crate::capture::Target,
        held: crate::capture::Held,
    ) -> Result<Screenshot, crate::capture::CaptureError> {
        let mut inner = self.inner.borrow_mut();
        let renderer = inner
            .graphics
            .as_mut()
            .ok_or_else(|| crate::capture::CaptureError::Backend(no_graphics()))?
            .renderer();
        crate::capture::read_back::<_, smithay::backend::renderer::gles::GlesTexture>(
            renderer, state, target, held,
        )
    }

    fn read_back_scaled(
        &mut self,
        state: &mut State,
        target: &crate::capture::Target,
        max: (u32, u32),
    ) -> Result<Screenshot, crate::capture::CaptureError> {
        let mut inner = self.inner.borrow_mut();
        let renderer = inner
            .graphics
            .as_mut()
            .ok_or_else(|| crate::capture::CaptureError::Backend(no_graphics()))?
            .renderer();
        crate::capture::read_back_scaled::<_, smithay::backend::renderer::gles::GlesTexture>(
            renderer, state, target, max,
        )
    }

    fn screenshot(&mut self, state: &mut State, name: &str) -> Result<Screenshot, BackendError> {
        let mut guard = self.inner.borrow_mut();
        let inner = &mut *guard;
        if name != inner.output.name() {
            return Err(BackendError(format!("no output named {name}")));
        }
        let mode = inner
            .output
            .current_mode()
            .ok_or_else(|| BackendError(format!("{name}: it has no mode")))?;
        let size = Size::<i32, Buffer>::from((mode.size.w, mode.size.h));
        let renderer = inner.graphics.as_mut().ok_or_else(no_graphics)?.renderer();
        // The scene is drawn again, off screen, and read back.
        let mapping = {
            let mut target: smithay::backend::renderer::gles::GlesTexture = renderer
                .create_buffer(Fourcc::Abgr8888, size)
                .map_err(|error| failed("screenshot image", error))?;
            let mut framebuffer = renderer
                .bind(&mut target)
                .map_err(|error| failed("bind screenshot image", error))?;
            let mut elements: Vec<OutputRenderElement<GlesRenderer>> =
                state.cursor_elements(renderer, &inner.output);
            elements.extend(output_elements(renderer, state, &inner.output));
            OutputDamageTracker::from_output(&inner.output)
                .render_output(renderer, &mut framebuffer, 0, &elements, BACKGROUND)
                .map_err(|error| failed("render screenshot", error))?;
            renderer
                .copy_framebuffer(&framebuffer, Rectangle::from_size(size), Fourcc::Abgr8888)
                .map_err(|error| failed("read back", error))?
        };
        let bytes = renderer
            .map_texture(&mapping)
            .map_err(|error| failed("map", error))?;
        let width = u32::try_from(mode.size.w).map_err(|error| failed("width", error))?;
        let height = u32::try_from(mode.size.h).map_err(|error| failed("height", error))?;
        Ok(Screenshot {
            width,
            height,
            rgba: bytes.to_vec(),
        })
    }

    fn configure(
        &mut self,
        state: &mut State,
        changes: &[OutputChange],
        test: bool,
    ) -> Result<(), BackendError> {
        refuse_disabling(changes)?;
        let mut inner = self.inner.borrow_mut();
        // The window's size is the host's to change.
        let current = inner.output.current_mode().map(|mode| mode.size);
        if let Some(change) = changes.iter().find(|change| {
            change
                .mode
                .is_some_and(|mode| Some(mode.size.into()) != current)
        }) {
            return Err(BackendError(format!(
                "{}: resize the window to change its mode",
                change.name
            )));
        }
        if test {
            return Ok(());
        }
        for change in changes {
            apply_placement(state, &inner.output, change);
        }
        inner.damage = window_damage(&inner.output);
        Ok(())
    }

    fn next_deadline(&self, state: &State) -> Option<Instant> {
        let inner = self.inner.borrow();
        if inner.closed || inner.dark {
            return None;
        }
        let frame = state.redraw_owed(&inner.output).then_some(inner.next_frame);
        match (frame, inner.rebuild_due) {
            (Some(frame), Some(rebuild)) => Some(frame.min(rebuild)),
            (frame, rebuild) => frame.or(rebuild),
        }
    }

    fn set_power(&mut self, state: &mut State, _name: &str, on: bool) -> Result<(), BackendError> {
        let mut inner = self.inner.borrow_mut();
        inner.dark = !on;
        if on {
            inner.next_frame = Instant::now();
            inner.damage = window_damage(&inner.output);
            state.mark_redraw(
                &inner.output,
                tilo_frame_metrics::paint_reason::PRESENTATION,
            );
        }
        Ok(())
    }

    fn closed(&self) -> bool {
        self.inner.borrow().closed
    }
}

/// What a request that needs the window's renderer is told while the window has none.
fn no_graphics() -> BackendError {
    BackendError("the window has no graphics: they are lost".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        /// The development window as an output.
        fn the_nested_window_is_an_output_of_its_hosts_size_scale_and_turn;

        phase the_window_draws_at_the_scales_its_host_reports;
        {
            // The desktop's own scale, fractional included, is the output's: a Tilo desktop at 175%
            // (which it tells a client 1.75) draws the session at 1.75, not at 1.
            for scale in [1.0, 1.25, 1.5, 1.75, 2.0] {
                assert!(
                    (host_scale(scale, 1.0) - scale).abs() < f64::EPSILON,
                    "a host at {scale} draws the window at {scale}"
                );
            }
            // A host that reports no usable scale draws at the configured one (1 when none is given),
            // so a window on an X11 screen or a host that says nothing still shows a sane size.
            assert!((host_scale(f64::NAN, 1.5) - 1.5).abs() < f64::EPSILON);
            assert!((host_scale(0.0, 1.5) - 1.5).abs() < f64::EPSILON);
            assert!((host_scale(-1.0, 2.0) - 2.0).abs() < f64::EPSILON);
        }

        phase a_configured_scale_covers_a_host_that_reports_none;
        {
            let config = |scale| NestedConfig {
                scale,
                ..NestedConfig::default()
            };
            assert!((config(1.5).configured_scale() - 1.5).abs() < f64::EPSILON);
            for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
                assert!((config(bad).configured_scale() - 1.0).abs() < f64::EPSILON);
            }
        }

        phase a_resized_window_is_a_new_mode_and_an_empty_one_none;
        {
            let mode = window_mode((1600, 900).into()).expect("a window with pixels has a mode");
            assert_eq!(mode.size, (1600, 900).into());
            assert_eq!(mode.refresh, REFRESH_MHZ);
            assert!(window_mode((0, 900).into()).is_none());
            assert!(window_mode((1600, 0).into()).is_none());
        }

        phase a_window_offers_the_one_size_it_has;
        // A window that was another size before offers only the size it has now: the sizes it had are
        // not displays the window can be put into.
        {
            use smithay::output::{PhysicalProperties, Subpixel};

            let output = Output::new(
                OUTPUT_NAME.to_owned(),
                PhysicalProperties {
                    size: (0, 0).into(),
                    subpixel: Subpixel::Unknown,
                    make: "Tilo".into(),
                    model: "test".into(),
                },
            );
            let small = window_mode((640, 800).into()).unwrap();
            let large = window_mode((1280, 800).into()).unwrap();
            output.change_current_state(Some(small), None, None, None);
            output.set_preferred(small);
            output.change_current_state(Some(large), None, None, None);
            one_mode(&output, large);
            output.set_preferred(large);
            assert_eq!(output.modes(), vec![large]);
            assert_eq!(output.current_mode(), Some(large));
            assert_eq!(output.preferred_mode(), Some(large));
        }

        phase the_window_draws_the_scene_as_the_output_is_turned;
        // The flip a framebuffer adds to the scene, taken back out for the window, whatever the
        // output's own turn: the window shows the scene as the output is turned.
        {
            const TURNS: [Transform; 8] = [
                Transform::Normal,
                Transform::_90,
                Transform::_180,
                Transform::_270,
                Transform::Flipped,
                Transform::Flipped90,
                Transform::Flipped180,
                Transform::Flipped270,
            ];
            for turn in TURNS {
                // What the renderer is given is the tracker's transform, inverted;
                // on top of that it flips the scene, as it does for any buffer.
                let drawn =
                    |candidate: Transform| Transform::Flipped180.matrix() * candidate.invert().matrix();
                assert_eq!(
                    drawn(flip(turn)),
                    turn.invert().matrix(),
                    "the window draws {turn:?}"
                );
                // One transform brings the scene back the way the output turns it.
                let cancels = TURNS
                    .into_iter()
                    .filter(|candidate| drawn(*candidate) == turn.invert().matrix())
                    .count();
                assert_eq!(cancels, 1, "{turn:?} is drawn back the way it is turned");
            }
        }
    }
}
