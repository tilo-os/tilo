//! The compositor in a window of the desktop it runs inside, for
//! development.
//!
//! The window is an X11 window (smithay's X11 backend): that one path runs on
//! X11 desktops and, through Xwayland, on Wayland ones, and smithay drives it
//! entirely (window, DRI3 buffers, Present timing, input). The window is one
//! output; resizing the window changes its mode. Frames are composited with
//! GLES on the GPU the X server renders with and presented when the server
//! is ready for the next one. Pointer and keyboard input of the window
//! becomes [`InputEvent`]s; the pointer is drawn as on a real screen, in
//! place of the host's.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::Instant;

use smithay::backend::allocator::dmabuf::DmabufAllocator;
use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::egl::{EGLContext, EGLDisplay};
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::{Bind as _, ImportDma as _};
use smithay::backend::x11::{Window, WindowBuilder, X11Backend, X11Event, X11Surface};
use smithay::output::{Mode, Output};
use smithay::reexports::calloop::LoopHandle;
use smithay::utils::{DeviceFd, Logical, Size};

use super::{Backend, BackendError, failed, refuse_disabling};
use crate::input::InputEvent;
use crate::output::{OutputChange, OutputConfig, apply_placement, refresh_interval};
use crate::present::{Presentation, Shown};
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
}

/// A scale clients can render at: finite and positive.
pub(crate) fn usable_scale(scale: f64) -> Option<f64> {
    (scale.is_finite() && scale > 0.0).then_some(scale)
}

/// The output mode of a window of `size` physical pixels, if it has any.
fn window_mode(size: Size<u16, Logical>) -> Option<Mode> {
    (size.w > 0 && size.h > 0).then(|| Mode {
        size: (i32::from(size.w), i32::from(size.h)).into(),
        refresh: REFRESH_MHZ,
    })
}

struct Inner {
    /// Kept for the surface: the window closes when it drops.
    _window: Window,
    surface: X11Surface,
    renderer: GlesRenderer,
    output: Output,
    damage: OutputDamageTracker,
    /// The frame with the X server, until it says the frame was shown.
    presenting: Option<Presentation>,
    /// When a frame may be drawn though the last one changed nothing.
    next_frame: Instant,
    closed: bool,
}

/// The window backend. Created by [`NestedBackend::start`], which opens the
/// window and registers the X11 connection in the compositor's loop.
pub struct NestedBackend {
    inner: Rc<RefCell<Inner>>,
}

impl NestedBackend {
    /// Open the window on the X server `$DISPLAY` names and make it an
    /// output.
    pub fn start(
        handle: &LoopHandle<'static, LoopData>,
        state: &mut State,
        config: &NestedConfig,
    ) -> Result<Self, Error> {
        let backend = X11Backend::new().map_err(|error| failed("X11 connection", error))?;
        let x11 = backend.handle();
        let (node, fd) = x11
            .drm_node()
            .map_err(|error| failed("the X server's GPU (DRI3)", error))?;
        let gbm =
            GbmDevice::new(DeviceFd::from(fd)).map_err(|error| failed("GBM device", error))?;
        // SAFETY: the display is created from the GBM device of the X
        // server's GPU, which the allocator and renderer keep alive.
        let display = unsafe { EGLDisplay::new(gbm.clone()) }
            .map_err(|error| failed("EGL display", error))?;
        let context = EGLContext::new(&display).map_err(|error| failed("EGL context", error))?;
        let modifiers: HashSet<_> = context
            .dmabuf_render_formats()
            .iter()
            .map(|format| format.modifier)
            .collect();
        let window = WindowBuilder::new()
            .title(&config.title)
            .size(config.size.into())
            .build(&x11)
            .map_err(|error| failed("X11 window", error))?;
        let surface = x11
            .create_surface(
                &window,
                DmabufAllocator(GbmAllocator::new(gbm, GbmBufferFlags::RENDERING)),
                modifiers.into_iter(),
            )
            .map_err(|error| failed("X11 surface", error))?;
        // SAFETY: the context is current only on this thread, the loop's.
        let renderer = unsafe { GlesRenderer::new(context) }
            .map_err(|error| failed("GLES renderer", error))?;
        state.enable_dmabuf(
            renderer.dmabuf_formats().iter().copied().collect(),
            Some(node),
        );

        let size = window.size();
        let mut output_config = OutputConfig::new(
            OUTPUT_NAME,
            i32::from(size.w).max(1),
            i32::from(size.h).max(1),
        )
        .with_scale(config.output_scale());
        output_config.refresh_mhz = REFRESH_MHZ;
        let output = output_config.create(&state.display, "nested");
        state.add_output(&output, output_config.location());
        let damage = OutputDamageTracker::from_output(&output);
        // The compositor draws the pointer: clients' cursor shapes show.
        window.set_cursor_visible(false);
        window.map();

        let inner = Rc::new(RefCell::new(Inner {
            _window: window,
            surface,
            renderer,
            output,
            damage,
            presenting: None,
            next_frame: Instant::now(),
            closed: false,
        }));
        let events = Rc::clone(&inner);
        handle
            .insert_source(backend, move |event, (), data| {
                handle_event(&events, &mut data.state, event);
            })
            .map_err(|error| failed("X11 source", error.error))?;
        Ok(Self { inner })
    }
}

fn handle_event(inner: &Rc<RefCell<Inner>>, state: &mut State, event: X11Event) {
    match event {
        X11Event::Input { event, .. } => {
            let extent = state
                .output_geometries()
                .first()
                .map_or_else(|| (1, 1).into(), |geometry| geometry.size);
            if let Some(event) = InputEvent::from_device(event, extent) {
                state.handle_input(event);
            }
        }
        X11Event::Resized { new_size, .. } => {
            let mut inner = inner.borrow_mut();
            let Some(mode) = window_mode(new_size) else {
                return;
            };
            inner.output.change_current_state(Some(mode), None, None, None);
            inner.output.set_preferred(mode);
            inner.damage = OutputDamageTracker::from_output(&inner.output);
            inner.next_frame = Instant::now();
            drop(inner);
            state.rearrange();
        }
        X11Event::PresentCompleted { .. } => {
            let mut inner = inner.borrow_mut();
            if let Some(presentation) = inner.presenting.take() {
                presentation.shown(Shown::Host);
            }
            inner.next_frame = Instant::now();
        }
        X11Event::Refresh { .. } => {
            let mut inner = inner.borrow_mut();
            // The server lost what the window showed: draw all of it.
            inner.surface.reset_buffer_ages();
            inner.next_frame = Instant::now();
        }
        X11Event::CloseRequested { .. } => inner.borrow_mut().closed = true,
        X11Event::Focus { .. } => {}
    }
}

impl Backend for NestedBackend {
    fn render(&mut self, state: &mut State) -> Result<(), BackendError> {
        let mut guard = self.inner.borrow_mut();
        let inner = &mut *guard;
        let now = Instant::now();
        if inner.closed || inner.presenting.is_some() || now < inner.next_frame {
            return Ok(());
        }
        let (mut buffer, age) = inner
            .surface
            .buffer()
            .map_err(|error| failed("window buffer", error))?;
        let mut elements: Vec<OutputRenderElement<GlesRenderer>> =
            state.cursor_elements(&mut inner.renderer, &inner.output);
        elements.extend(output_elements(&mut inner.renderer, state, &inner.output));
        let drawn = {
            let mut framebuffer = inner
                .renderer
                .bind(&mut buffer)
                .map_err(|error| failed("bind window buffer", error))?;
            inner
                .damage
                .render_output(
                    &mut inner.renderer,
                    &mut framebuffer,
                    usize::from(age),
                    &elements,
                    BACKGROUND,
                )
                .map_err(|error| failed("render window", error))?
        };
        // Clients draw their next frame now; with nothing new on screen the
        // window is asked again one refresh later.
        let presentation = state.frame_drawn(&inner.output, Some(&drawn.states));
        if drawn.damage.is_some() {
            inner
                .surface
                .submit()
                .map_err(|error| failed("present window", error))?;
            inner.presenting = Some(presentation);
        } else {
            presentation.shown(Shown::Host);
        }
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
        inner.damage = OutputDamageTracker::from_output(&inner.output);
        Ok(())
    }

    fn next_deadline(&self) -> Option<Instant> {
        let inner = self.inner.borrow();
        (inner.presenting.is_none() && !inner.closed).then_some(inner.next_frame)
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
}
