//! The compositor in a window of the desktop it runs inside, for
//! development.
//!
//! The window is winit's: a Wayland client where the desktop runs Wayland and
//! an X11 window where it does not, so the session is a native window either
//! way. The window is one output; resizing it changes its mode. Frames are
//! composited with GLES on the GPU that draws the window and presented to the
//! host with its damage. Pointer and keyboard input of the window becomes
//! [`InputEvent`]s; the pointer is drawn as on a real screen, in place of the
//! host's.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use smithay::backend::allocator::Fourcc;
use smithay::backend::egl::EGLDevice;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::{Bind as _, ExportMem as _, ImportDma as _, Offscreen as _};
use smithay::backend::winit::{WinitEvent, WinitGraphicsBackend, init_from_attributes};
use smithay::output::{Mode, Output};
use smithay::reexports::calloop::LoopHandle;
use smithay::utils::{Buffer, Physical, Point, Rectangle, Size, Transform};
use winit::dpi::PhysicalSize;
use winit::window::{Window as WinitWindow, WindowAttributes};

use super::{Backend, BackendError, Screenshot, failed, refuse_disabling};
use crate::input::InputEvent;
use crate::output::{OutputChange, OutputConfig, apply_placement, refresh_interval};
use crate::present::Shown;
use crate::render::{BACKGROUND, OutputRenderElement, output_elements};
use crate::state::State;
use crate::{Error, LoopData};

/// The name of the window's output.
pub const OUTPUT_NAME: &str = "NESTED-1";

/// The refresh the window's output reports; the host decides the real one.
const REFRESH_MHZ: i32 = 60_000;

/// How the window starts.
#[derive(Clone, Debug, PartialEq)]
pub struct NestedConfig {
    /// The window's first size, in physical pixels.
    pub size: (u16, u16),
    /// The scale clients render at.
    pub scale: f64,
    /// The window's title.
    pub title: String,
}

impl Default for NestedConfig {
    fn default() -> Self {
        Self {
            size: (1280, 800),
            scale: 1.0,
            title: "Tilo".into(),
        }
    }
}

impl NestedConfig {
    /// The scale outputs get: the configured one if it is usable, else 1.
    fn output_scale(&self) -> f64 {
        usable_scale(self.scale).unwrap_or(1.0)
    }

    fn attributes(&self) -> WindowAttributes {
        WinitWindow::default_attributes()
            .with_title(&self.title)
            .with_inner_size(PhysicalSize::new(
                u32::from(self.size.0),
                u32::from(self.size.1),
            ))
            .with_visible(true)
    }
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

struct Inner {
    window: WinitGraphicsBackend<GlesRenderer>,
    output: Output,
    damage: OutputDamageTracker,
    /// When a frame may be drawn though the last one changed nothing.
    next_frame: Instant,
    closed: bool,
    /// Switched off: the window keeps its last frame and nothing is drawn.
    dark: bool,
}

/// The window backend. Created by [`NestedBackend::start`], which opens the
/// window and registers the host's event loop in the compositor's.
pub struct NestedBackend {
    inner: Rc<RefCell<Inner>>,
}

impl NestedBackend {
    /// Open the window on the desktop's compositor and make it an output.
    pub fn start(
        handle: &LoopHandle<'static, LoopData>,
        state: &mut State,
        config: &NestedConfig,
    ) -> Result<Self, Error> {
        let (mut window, events) = init_from_attributes::<GlesRenderer>(config.attributes())
            .map_err(|error| failed("window", error))?;
        // Clients render on the GPU that draws this window, and hand their
        // frames over as dma-bufs it can import.
        let node = EGLDevice::device_for_display(window.renderer().egl_context().display())
            .ok()
            .and_then(|device| device.try_get_render_node().ok().flatten());
        let formats: Vec<_> = window.renderer().dmabuf_formats().iter().copied().collect();
        let render_formats: Vec<_> = window
            .renderer()
            .egl_context()
            .dmabuf_render_formats()
            .iter()
            .copied()
            .collect();
        state.enable_dmabuf(formats, node);
        if let Some(node) = node {
            state.enable_dmabuf_capture(node.dev_id(), render_formats);
        }

        let size = window.window_size();
        let mut output_config = OutputConfig::new(OUTPUT_NAME, size.w.max(1), size.h.max(1))
            .with_scale(
                // The host's own scale is the window's: a desktop at 150%
                // shows a 150% session.
                config.output_scale(),
            );
        output_config.refresh_mhz = REFRESH_MHZ;
        let output = output_config.create(&state.display, "nested", Point::from((0, 0)));
        state.add_output(&output, Point::from((0, 0)));
        let damage = window_damage(&output);
        // The compositor draws the pointer: clients' cursor shapes show.
        window.window().set_cursor_visible(false);

        let inner = Rc::new(RefCell::new(Inner {
            window,
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
///
/// A window's framebuffer is counted from its bottom edge, the opposite of
/// the physical space the scene is laid out in, so the scene goes into it
/// flipped: a window rendering its output turned by `transform` is drawn as
/// [`flip`]`(transform)`. Smithay's own window example draws the same way.
fn window_damage(output: &Output) -> OutputDamageTracker {
    let Some(mode) = output.current_mode() else {
        return OutputDamageTracker::from_output(output);
    };
    let scale = output.current_scale().fractional_scale();
    let transform = flip(output.current_transform());
    OutputDamageTracker::new(mode.size, scale, transform)
}

/// The same turn of the output with one more flip: what drawing into the
/// window adds to the scene.
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
        WinitEvent::Resized { size, .. } => {
            let mut inner = inner.borrow_mut();
            let Some(mode) = window_mode(size) else {
                return;
            };
            inner
                .output
                .change_current_state(Some(mode), None, None, None);
            inner.output.set_preferred(mode);
            inner.damage = window_damage(&inner.output);
            inner.next_frame = Instant::now();
            let output = inner.output.clone();
            drop(inner);
            state.mark_redraw(&output);
            state.rearrange();
        }
        WinitEvent::Redraw => {
            let mut inner = inner.borrow_mut();
            inner.next_frame = Instant::now();
            let output = inner.output.clone();
            drop(inner);
            state.mark_redraw(&output);
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
        if inner.closed || inner.dark || now < inner.next_frame {
            return Ok(());
        }
        // Nothing new to show: an idle session draws nothing.
        if !state.take_redraw(&inner.output) {
            return Ok(());
        }
        let age = inner.window.buffer_age().unwrap_or(0);
        let (damage, states) = {
            let (renderer, mut framebuffer) = inner
                .window
                .bind()
                .map_err(|error| failed("bind window", error))?;
            let mut elements: Vec<OutputRenderElement<GlesRenderer>> =
                state.cursor_elements(renderer, &inner.output);
            elements.extend(output_elements(renderer, state, &inner.output));
            let drawn = inner
                .damage
                .render_output(renderer, &mut framebuffer, age, &elements, BACKGROUND)
                .map_err(|error| failed("render window", error))?;
            (drawn.damage, drawn.states)
        };
        // Clients draw their next frame now; with nothing new on screen the
        // window is asked again one refresh later.
        let presentation = state.frame_drawn(&inner.output, Some(&states));
        let damaged = damage.map(Vec::as_slice);
        inner
            .window
            .submit(damaged)
            .map_err(|error| failed("present window", error))?;
        presentation.shown(Shown::Host);
        inner.next_frame = now
            .checked_add(refresh_interval(
                inner
                    .output
                    .current_mode()
                    .map_or(REFRESH_MHZ, |mode| mode.refresh),
            ))
            .unwrap_or(now);
        Ok(())
    }

    fn backdrop(
        &mut self,
        state: &mut State,
        region: (usize, usize),
        request: &crate::BackdropRequest,
    ) -> Result<Option<crate::BackdropImage>, BackendError> {
        crate::backdrop::draw::<_, smithay::backend::renderer::gles::GlesTexture>(
            self.inner.borrow_mut().window.renderer(),
            state,
            region,
            request,
        )
    }

    fn capture(
        &mut self,
        state: &mut State,
        frame: &crate::capture::Frame,
    ) -> Result<crate::capture::Copied, crate::capture::CaptureError> {
        crate::capture::copy::<_, smithay::backend::renderer::gles::GlesTexture>(
            self.inner.borrow_mut().window.renderer(),
            state,
            frame,
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
        let renderer = inner.window.renderer();
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
        let ready = !inner.closed && !inner.dark && state.redraw_owed(&inner.output);
        ready.then_some(inner.next_frame)
    }

    fn set_power(&mut self, state: &mut State, _name: &str, on: bool) -> Result<(), BackendError> {
        let mut inner = self.inner.borrow_mut();
        inner.dark = !on;
        if on {
            inner.next_frame = Instant::now();
            inner.damage = window_damage(&inner.output);
            state.mark_redraw(&inner.output);
        }
        Ok(())
    }

    fn closed(&self) -> bool {
        self.inner.borrow().closed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unusable_scales_fall_back_to_one() {
        let config = |scale| NestedConfig {
            scale,
            ..NestedConfig::default()
        };
        assert!((config(1.5).output_scale() - 1.5).abs() < f64::EPSILON);
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!((config(bad).output_scale() - 1.0).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn a_resized_window_is_a_new_mode_and_an_empty_one_none() {
        let mode = window_mode((1600, 900).into()).expect("a window with pixels has a mode");
        assert_eq!(mode.size, (1600, 900).into());
        assert_eq!(mode.refresh, REFRESH_MHZ);
        assert!(window_mode((0, 900).into()).is_none());
        assert!(window_mode((1600, 0).into()).is_none());
    }

    /// The flip a framebuffer adds to the scene, taken back out for the
    /// window, whatever the output's own turn: the window shows the scene as
    /// the output is turned.
    #[test]
    fn the_window_draws_the_scene_as_the_output_is_turned() {
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
