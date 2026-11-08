//! Outputs with no display: composited on the GPU into offscreen images that
//! tests, screenshots and a viewer read back.
//!
//! There is no window and no display device, so the renderer makes its own
//! EGL display, surfaceless: Mesa picks the machine's GPU, or its software
//! rasterizer where there is none. Its outputs are EGL images, not scanout
//! buffers.
//!
//! The renderer is GLES, the one the real backends use, because clients hand
//! their frames over as dma-bufs allocated on the GPU the session tells them
//! to allocate on (`zwp_linux_dmabuf_feedback_v1`). A CPU renderer can only
//! map a buffer the CPU can read, and a hardware client's swapchain is
//! device-local: it would reject every accelerated window, and a headless
//! capture would show a desktop with no apps in it. Reading them on the GPU
//! is what the session does on a real display, so what a test or a capture
//! sees here is the desktop's own rendering, software-rasterized where the
//! machine has no GPU to draw it.
//!
//! Input arrives through [`Compositor::input`](crate::Compositor::input).
//! Frames are drawn when asked ([`Compositor::render`](crate::Compositor::render)),
//! each drawing only what changed since the last.
//!
//! The simulated display shows a frame once per refresh, as a real one does:
//! a session on this backend draws at its mode's rate and no faster, and a
//! frame is built one lead before the blank that shows it, as on a display
//! ([`Pacing`]), so it carries what arrived up to that blank and the client
//! hears the frame is on the display at the blank instead of a refresh later
//! (a test that wants a frame after one it just drew waits the same way the
//! session's loop does,
//! [`Compositor::draw_frame`](crate::Compositor::draw_frame)).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use smithay::backend::allocator::Fourcc;
use smithay::backend::egl::native::EGLSurfacelessDisplay;
use smithay::backend::egl::{EGLContext, EGLDevice, EGLDisplay};
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::{Bind, ExportMem, ImportDma as _, Offscreen, Texture};
use smithay::output::Output;
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Size, Transform};
use tilo_color_pipeline::hdr_surface::{HdrLayer, HdrLayerRecording};

use super::{Backend, BackendError, Screenshot, failed};
use crate::capture::{CaptureError, render_to_memory};
use crate::output::{OutputChange, OutputConfig, apply_placement, refresh_interval};
use crate::pacing::Pacing;
use crate::render::{BACKGROUND, output_elements};
use crate::state::State;

struct HeadlessOutput {
    output: Output,
    /// The place its configuration asked for; `None` while the layout puts it beside the others.
    place: Option<(i32, i32)>,
    damage: OutputDamageTracker,
    target: GlesTexture,
    /// Whether the target holds the previous frame (the damage tracker's buffer age is then 1, else
    /// 0).
    drawn: bool,
    /// The vertical blank the simulated display next shows a frame at: the grid its mode's refresh
    /// sets.
    next_blank: Instant,
    /// When that frame is built: one lead before its blank, the lead being the worst recent cost of
    /// building a frame plus a margin. Building at the blank instead misses every commit and input
    /// event of the refresh the frame is for, and shows it a whole refresh late.
    due: Instant,
    /// What the frames have cost, for the lead.
    pacing: Pacing,
    /// Switched off: nothing is drawn, the image goes black.
    dark: bool,
    /// The gamma ramp the simulated display engine applies to what it shows (screenshots), not to
    /// what is drawn (captures).
    gamma: Option<crate::gamma::Ramp>,
    /// What the panel is configured to show, as a real sink's EDID and signal would say.
    colour: tilo_color_pipeline::hdr_display::DisplayCapabilities,
}

/// The vertical blank after `blank` that is still ahead of `drawn` : one refresh on, or the first
/// one after a frame that took longer than a refresh missed its blanks instead of bursting to catch
/// up on them.
fn blank_after(blank: Instant, interval: Duration, drawn: Instant) -> Instant {
    let mut next = blank.checked_add(interval).unwrap_or(drawn);
    while next <= drawn {
        match next.checked_add(interval) {
            Some(later) => next = later,
            None => return drawn,
        }
    }
    next
}

/// The blank the frame after `built` is shown at, and the point one `lead` before it where it is
/// built.
fn next_frame(
    blank: Instant,
    interval: Duration,
    lead: Duration,
    built: Instant,
) -> (Instant, Instant) {
    let next_blank = blank_after(blank, interval, built);
    // A lead longer than the refresh, or a build that overran its blank,
    // puts the point in the past: the frame is built at once.
    let due = next_blank.checked_sub(lead).unwrap_or(next_blank);
    (next_blank, due)
}

impl HeadlessOutput {
    /// This display's refresh interval.
    fn interval(&self) -> Duration {
        refresh_interval(self.output.current_mode().map_or(0, |mode| mode.refresh))
    }

    /// This display showed the frame whose build began at `started` : the next blank is one refresh
    /// on, on the grid its mode keeps, and the next frame is built one lead before it, the lead
    /// being how long building one takes.
    fn showed(&mut self, started: Instant) {
        let interval = self.interval();
        let built = Instant::now();
        self.pacing.queued(started, built, interval);
        // This display has no blank to report: the image holds the frame as soon as it is built,
        // and `next_blank` is the grid that keeps frames to one per refresh.
        self.pacing.shown(built, interval, false);
        self.pacing.log(&self.output.name(), built, interval);
        (self.next_blank, self.due) =
            next_frame(self.next_blank, interval, self.pacing.lead(interval), built);
    }

    /// What changed shows at once: the next blank is now, and the frame for it is built now.
    fn restart_grid(&mut self) {
        self.next_blank = Instant::now();
        self.due = self.next_blank;
        self.pacing = Pacing::default();
    }
}

/// The ramp size headless outputs carry, as many display engines do.
const GAMMA_SIZE: usize = 256;

/// The GPU renderer and every headless output's image.
pub struct HeadlessBackend {
    renderer: GlesRenderer,
    outputs: Vec<HeadlessOutput>,
}

impl HeadlessBackend {
    /// A backend with no outputs yet.
    pub fn new(state: &mut State) -> Result<Self, BackendError> {
        // SAFETY: this backend makes the process's EGL display and context,
        // and both live only in it; no other EGL client creates or
        // terminates either.
        let display = unsafe { EGLDisplay::new(EGLSurfacelessDisplay) }
            .map_err(|error| failed("EGL display", error))?;
        // The GPU the display renders on: clients are told to allocate their dma-bufs there, so the
        // buffers they hand over are the ones this renderer imports. A machine with no GPU (a
        // software rasterizer) has no node, and its clients render through shared memory.
        let node = EGLDevice::device_for_display(&display)
            .ok()
            .and_then(|device| device.try_get_render_node().ok().flatten());
        let context = EGLContext::new(&display).map_err(|error| failed("EGL context", error))?;
        // SAFETY: the context is current on this thread, the one that draws,
        // and the renderer keeps it for as long as it draws here.
        let renderer = unsafe { GlesRenderer::new(context) }
            .map_err(|error| failed("GLES renderer", error))?;
        let formats: Vec<_> = renderer.dmabuf_formats().iter().copied().collect();
        let render_formats: Vec<_> = renderer
            .egl_context()
            .dmabuf_render_formats()
            .iter()
            .copied()
            .collect();
        state.enable_dmabuf(formats, node);
        if let Some(node) = node {
            state.enable_dmabuf_capture(node.dev_id(), render_formats);
        }
        Ok(Self {
            renderer,
            outputs: Vec::new(),
        })
    }

    /// Place every output that is on: where its configuration asked for,
    /// the rest packed left to right after those, so one plugged in appears
    /// beside the others instead of on top of one, and one unplugged leaves
    /// no hole behind. An output someone turned off keeps no place.
    fn arrange_outputs(&self, state: &mut State) {
        let mut shown: Vec<&HeadlessOutput> = self
            .outputs
            .iter()
            .filter(|output| !state.disabled_outputs.contains(&output.output))
            .collect();
        shown.sort_by_key(|output| (output.output.current_location().x, output.output.name()));
        let outputs: Vec<(String, i32)> = shown
            .iter()
            .map(|output| {
                (
                    output.output.name(),
                    crate::output::logical_width(&output.output),
                )
            })
            .collect();
        let fixed: HashMap<String, (i32, i32)> = shown
            .iter()
            .filter_map(|output| output.place.map(|place| (output.output.name(), place)))
            .collect();
        let positions = crate::output::arrange(&outputs, &fixed);
        let mut moved = false;
        for (output, position) in shown.into_iter().zip(positions) {
            let known = state.space.outputs().any(|mapped| mapped == &output.output);
            if !known || output.output.current_location() != Point::from(position) {
                output
                    .output
                    .change_current_state(None, None, None, Some(position.into()));
                state.mark_redraw(&output.output);
                moved = true;
            }
            state.space.map_output(&output.output, position);
        }
        if moved {
            // What an output carries (panels, backgrounds) follows it.
            state.place_layers();
        }
        state.rearrange();
    }
}

/// The composited frame of an output that shows HDR layers: the scene drawn twice with the layers
/// replaced by black and by their channel colours, and the two composited at each layer's signal.
fn composite_hdr(
    renderer: &mut GlesRenderer,
    state: &State,
    output: &Output,
    (width, height): (u32, u32),
    scale: f64,
    layers: &[HdrLayer],
) -> Result<Vec<u8>, BackendError> {
    let size = Size::<i32, Physical>::from((
        i32::try_from(width).map_err(|error| failed("width", error))?,
        i32::try_from(height).map_err(|error| failed("height", error))?,
    ));
    let transform = output.current_transform();
    let pass = |renderer: &mut GlesRenderer, pixels: &mut Option<Vec<u8>>| {
        let elements = output_elements(renderer, state, output);
        *pixels = Some(render_to_memory::<GlesRenderer, GlesTexture, _>(
            renderer, size, scale, transform, &elements, BACKGROUND,
        )?);
        Ok::<(), CaptureError>(())
    };
    let mut first = None;
    {
        let _black = crate::hdr::black();
        pass(renderer, &mut first).map_err(|error| failed("hdr pass", error))?;
    }
    let channels: Vec<(u64, u8)> = layers
        .iter()
        .map(|layer| (layer.plane.id(), layer.channel))
        .collect();
    let mut second = None;
    {
        let _lit = crate::hdr::lit(&channels);
        pass(renderer, &mut second).map_err(|error| failed("hdr pass", error))?;
    }
    let (Some(first), Some(second)) = (first, second) else {
        return Err(BackendError("an HDR pass drew nothing".into()));
    };
    let size = (width, height);
    Ok(crate::hdr::composite(&first, &second, size, layers))
}

/// The composited frame as the one element the output draws: every pixel of it, in the output's
/// physical size.
fn frame_element(
    renderer: &mut GlesRenderer,
    rgba: &[u8],
    (width, height): (u32, u32),
    scale: f64,
) -> Result<MemoryRenderBufferRenderElement<GlesRenderer>, BackendError> {
    let width = i32::try_from(width).map_err(|error| failed("width", error))?;
    let height = i32::try_from(height).map_err(|error| failed("height", error))?;
    let buffer = MemoryRenderBuffer::from_slice(
        rgba,
        Fourcc::Abgr8888,
        (width, height),
        1,
        Transform::Normal,
        Some(vec![Rectangle::from_size(Size::from((width, height)))]),
    );
    let logical = Size::<i32, Logical>::from((
        window_frame_pixels(f64::from(width) / scale),
        window_frame_pixels(f64::from(height) / scale),
    ));
    MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        Point::<f64, Physical>::from((0.0, 0.0)),
        &buffer,
        None,
        Some(Rectangle::from_size(Size::from((
            f64::from(width),
            f64::from(height),
        )))),
        Some(logical),
        Kind::Unspecified,
    )
    .map_err(|error| failed("composited frame", error))
}

/// A logical length rounded to whole pixels.
fn window_frame_pixels(value: f64) -> i32 {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "an output's logical extent: at most a few thousand pixels"
    )]
    let pixels = value.round() as i32;
    pixels
}

/// What one frame costs to compose, per layer: the pixels its damage asks for, and how much of that
/// area each element drawn over them is painted in. A layer that paints a bar in a frame the size
/// of a display is its bar here, and a layer that shows nothing is not in the list at all.
fn trace_passes<E: smithay::backend::renderer::element::Element>(
    elements: &[E],
    damage: &[Rectangle<i32, Physical>],
    output: &Output,
) {
    let scale = smithay::utils::Scale::from(output.current_scale().fractional_scale());
    let damage_px = damage.iter().fold(0i64, |sum, rect| {
        sum.saturating_add(i64::from(rect.size.w).saturating_mul(i64::from(rect.size.h)))
    });
    let passes: Vec<String> = elements
        .iter()
        .rev()
        .filter_map(|element| {
            let geometry = element.geometry(scale);
            let drawn = damage
                .iter()
                .filter_map(|rect| rect.intersection(geometry))
                .fold(0i64, |sum, rect| {
                    sum.saturating_add(
                        i64::from(rect.size.w).saturating_mul(i64::from(rect.size.h)),
                    )
                });
            (drawn > 0).then(|| {
                format!(
                    "{drawn}px of {}x{}@{},{}",
                    geometry.size.w, geometry.size.h, geometry.loc.x, geometry.loc.y
                )
            })
        })
        .collect();
    tracing::debug!(
        output = output.name(),
        damage_px,
        passes = passes.len(),
        ?passes,
        "frame composited"
    );
}

impl Backend for HeadlessBackend {
    fn describe(&self) -> &'static str {
        "headless"
    }

    fn output_colour(
        &self,
        output: &Output,
    ) -> tilo_color_pipeline::hdr_display::DisplayCapabilities {
        self.outputs
            .iter()
            .find(|known| known.output == *output)
            .map_or_else(
                tilo_color_pipeline::hdr_display::DisplayCapabilities::sdr_default,
                |known| known.colour,
            )
    }

    fn add_output(&mut self, state: &mut State, config: &OutputConfig) -> Result<(), BackendError> {
        // Right of the outputs already here until the layout places it: a connector lit for the
        // first time never lands on one showing.
        let beside = self
            .outputs
            .iter()
            .filter(|output| !state.disabled_outputs.contains(&output.output))
            .map(|output| {
                output
                    .output
                    .current_location()
                    .x
                    .saturating_add(crate::output::logical_width(&output.output))
            })
            .max()
            .unwrap_or(0);
        let location = Point::from(config.position.unwrap_or((beside, 0)));
        let output = config.create(&state.display, "headless", location);
        let size = Size::<i32, Buffer>::from(config.size);
        let target = self
            .renderer
            .create_buffer(Fourcc::Argb8888, size)
            .map_err(|error| failed("output image", error))?;
        state.gamma.set_size(&config.name, Some(GAMMA_SIZE));
        let damage = OutputDamageTracker::from_output(&output);
        self.outputs.push(HeadlessOutput {
            output,
            place: config.position,
            damage,
            target,
            drawn: false,
            next_blank: Instant::now(),
            due: Instant::now(),
            pacing: Pacing::default(),
            dark: false,
            gamma: None,
            colour: config.colour,
        });
        self.arrange_outputs(state);
        Ok(())
    }

    fn remove_output(&mut self, state: &mut State, name: &str) -> Result<(), BackendError> {
        let Some(index) = self
            .outputs
            .iter()
            .position(|output| output.output.name() == name)
        else {
            return Err(BackendError(format!("no output named {name}")));
        };
        let output = self.outputs.remove(index).output;
        state.remove_output(&output);
        self.arrange_outputs(state);
        Ok(())
    }

    fn configure(
        &mut self,
        state: &mut State,
        changes: &[OutputChange],
        test: bool,
    ) -> Result<(), BackendError> {
        if test {
            return Ok(());
        }
        // A headless output takes any mode: a new image of that size.
        for change in changes {
            let Some(output) = self
                .outputs
                .iter_mut()
                .find(|output| output.output.name() == change.name)
            else {
                continue;
            };
            if let Some(request) = change.mode {
                let mode = smithay::output::Mode {
                    size: request.size.into(),
                    refresh: request.refresh_mhz.unwrap_or(60_000),
                };
                output.target = self
                    .renderer
                    .create_buffer(Fourcc::Argb8888, Size::<i32, Buffer>::from(request.size))
                    .map_err(|error| failed("output image", error))?;
                output
                    .output
                    .change_current_state(Some(mode), None, None, None);
                output.output.add_mode(mode);
                output.drawn = false;
                // A new mode is a new blank grid: what changed shows at once.
                output.restart_grid();
            }
            apply_placement(state, &output.output, change);
            if change.position.is_some() {
                output.place = change.position;
            }
            output.damage = OutputDamageTracker::from_output(&output.output);
        }
        Ok(())
    }

    fn render(&mut self, state: &mut State) -> Result<(), BackendError> {
        for output in &mut self.outputs {
            if output.dark {
                continue;
            }
            // The display shows one frame per refresh: what changed is built one lead before the
            // blank that shows it, so a frame it is not yet time to build leaves what it owes still
            // owed.
            let started = Instant::now();
            if started < output.due {
                continue;
            }
            // Nothing new to show: an idle output draws nothing.
            if !state.take_redraw(&output.output) {
                continue;
            }
            let width = output.target.width();
            let height = output.target.height();
            // What the scene draws while its HDR layers are recorded.
            let recording = HdrLayerRecording::begin();
            let elements = output_elements(&mut self.renderer, state, &output.output);
            let layers = recording.finish();
            if !layers.is_empty() {
                let scale = output.output.current_scale().fractional_scale();
                let rgba = composite_hdr(
                    &mut self.renderer,
                    state,
                    &output.output,
                    (width, height),
                    scale,
                    &layers,
                )?;
                let elements = [frame_element(
                    &mut self.renderer,
                    &rgba,
                    (width, height),
                    scale,
                )?];
                {
                    let mut framebuffer = self
                        .renderer
                        .bind(&mut output.target)
                        .map_err(|error| failed("bind output", error))?;
                    output
                        .damage
                        .render_output(
                            &mut self.renderer,
                            &mut framebuffer,
                            0,
                            &elements,
                            BACKGROUND,
                        )
                        .map_err(|error| failed("render output", error))?;
                }
                output.drawn = true;
                state.frame_shown(&output.output);
                output.showed(started);
                continue;
            }
            let rendered = {
                let mut framebuffer = self
                    .renderer
                    .bind(&mut output.target)
                    .map_err(|error| failed("bind output", error))?;
                let age = usize::from(output.drawn);
                output
                    .damage
                    .render_output(
                        &mut self.renderer,
                        &mut framebuffer,
                        age,
                        &elements,
                        BACKGROUND,
                    )
                    .map_err(|error| failed("render output", error))?
            };
            if tracing::enabled!(tracing::Level::DEBUG) {
                trace_passes(
                    &elements,
                    rendered.damage.map(Vec::as_slice).unwrap_or_default(),
                    &output.output,
                );
            }
            output.drawn = true;
            state.frame_shown(&output.output);
            output.showed(started);
        }
        Ok(())
    }

    fn next_deadline(&self, state: &State) -> Option<Instant> {
        self.outputs
            .iter()
            .filter(|output| !output.dark && state.redraw_owed(&output.output))
            .map(|output| output.due)
            .min()
    }

    fn set_power(&mut self, _state: &mut State, name: &str, on: bool) -> Result<(), BackendError> {
        let output = self
            .outputs
            .iter_mut()
            .find(|output| output.output.name() == name)
            .ok_or_else(|| BackendError(format!("no output named {name}")))?;
        if !on && !output.dark {
            // A dark panel shows black, as a switched-off one would.
            let mut framebuffer = self
                .renderer
                .bind(&mut output.target)
                .map_err(|error| failed("bind output", error))?;
            let size = output
                .output
                .current_mode()
                .map_or_else(Size::default, |mode| mode.size);
            let mut frame = smithay::backend::renderer::Renderer::render(
                &mut self.renderer,
                &mut framebuffer,
                size,
                smithay::utils::Transform::Normal,
            )
            .map_err(|error| failed("clear output", error))?;
            smithay::backend::renderer::Frame::clear(
                &mut frame,
                smithay::backend::renderer::Color32F::new(0.0, 0.0, 0.0, 1.0),
                &[Rectangle::from_size(size)],
            )
            .map_err(|error| failed("clear output", error))?;
            let _ = smithay::backend::renderer::Frame::finish(frame)
                .map_err(|error| failed("clear output", error))?;
        }
        output.dark = !on;
        // The image no longer holds the last frame.
        output.drawn = false;
        // A switched-on panel shows what changed at once.
        output.restart_grid();
        output.damage = OutputDamageTracker::from_output(&output.output);
        Ok(())
    }

    fn set_gamma(
        &mut self,
        _state: &mut State,
        name: &str,
        ramp: Option<&crate::gamma::Ramp>,
    ) -> Result<(), BackendError> {
        let output = self
            .outputs
            .iter_mut()
            .find(|output| output.output.name() == name)
            .ok_or_else(|| BackendError(format!("no output named {name}")))?;
        output.gamma = ramp.cloned();
        Ok(())
    }

    fn backdrop(
        &mut self,
        state: &mut State,
        region: (usize, usize),
        request: &crate::BackdropRequest,
    ) -> Result<Option<crate::BackdropImage>, BackendError> {
        crate::backdrop::draw(&mut self.renderer, state, region, request)
    }

    fn capture(
        &mut self,
        state: &mut State,
        frame: &crate::capture::Frame,
    ) -> Result<crate::capture::Copied, crate::capture::CaptureError> {
        crate::capture::copy::<_, GlesTexture>(&mut self.renderer, state, frame)
    }

    fn read_back(
        &mut self,
        state: &mut State,
        target: &crate::capture::Target,
        held: crate::capture::Held,
    ) -> Result<Screenshot, crate::capture::CaptureError> {
        crate::capture::read_back::<_, GlesTexture>(&mut self.renderer, state, target, held)
    }

    fn read_back_scaled(
        &mut self,
        state: &mut State,
        target: &crate::capture::Target,
        max: (u32, u32),
    ) -> Result<Screenshot, crate::capture::CaptureError> {
        crate::capture::read_back_scaled::<_, GlesTexture>(&mut self.renderer, state, target, max)
    }

    fn screenshot(&mut self, _state: &mut State, name: &str) -> Result<Screenshot, BackendError> {
        let output = self
            .outputs
            .iter_mut()
            .find(|output| output.output.name() == name)
            .ok_or_else(|| BackendError(format!("no output named {name}")))?;
        let width = output.target.width();
        let height = output.target.height();
        let size = Size::<i32, Buffer>::from((
            i32::try_from(width).map_err(|error| failed("width", error))?,
            i32::try_from(height).map_err(|error| failed("height", error))?,
        ));
        // ABGR8888 is R, G, B, A in memory.
        let mapping = {
            let framebuffer = self
                .renderer
                .bind(&mut output.target)
                .map_err(|error| failed("bind output", error))?;
            self.renderer
                .copy_framebuffer(&framebuffer, Rectangle::from_size(size), Fourcc::Abgr8888)
                .map_err(|error| failed("read back", error))?
        };
        let bytes = self
            .renderer
            .map_texture(&mapping)
            .map_err(|error| failed("map", error))?;
        let mut rgba = bytes.to_vec();
        if let Some(ramp) = &output.gamma {
            ramp.apply(&mut rgba);
        }
        Ok(Screenshot {
            width,
            height,
            rgba,
        })
    }
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    clippy::unchecked_time_subtraction,
    reason = "test timelines a few milliseconds from now"
)]
mod tests {
    use super::*;

    const INTERVAL: Duration = Duration::from_micros(16_667);

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn the_frame_for_a_blank_is_built_one_lead_before_it() {
        let t0 = Instant::now();
        let (blank, due) = next_frame(t0, INTERVAL, ms(3), t0 + ms(1));
        assert_eq!(blank, t0 + INTERVAL, "the next blank on the grid");
        assert_eq!(due, t0 + INTERVAL - ms(3), "built one lead before it");
    }

    #[test]
    fn a_frame_that_overran_its_blank_lands_on_the_grid_after_it() {
        let t0 = Instant::now();
        // Built for the second blank but finished two refreshes on.
        let late = t0 + INTERVAL * 2 + ms(1);
        let (blank, due) = next_frame(t0, INTERVAL, ms(3), late);
        assert_eq!(blank, t0 + INTERVAL * 3, "no bursting to catch up");
        assert_eq!(due, t0 + INTERVAL * 3 - ms(3));
    }

    #[test]
    fn a_lead_longer_than_a_refresh_builds_at_once() {
        let t0 = Instant::now();
        let (blank, due) = next_frame(t0, INTERVAL, INTERVAL, t0);
        assert_eq!(blank, t0 + INTERVAL);
        assert_eq!(due, t0, "the blank before it is already past");
    }
}
