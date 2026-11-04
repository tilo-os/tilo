//! Outputs with no display: composited on the CPU into images that tests,
//! screenshots and a viewer read back.
//!
//! No GPU and no devices; input arrives through
//! [`Compositor::input`](crate::Compositor::input). Frames are drawn when
//! asked ([`Compositor::render`](crate::Compositor::render)), each drawing
//! only what changed since the last.
//!
//! The simulated display shows a frame once per refresh, as a real one does:
//! a change that arrives between two vertical blanks waits for the blank that
//! shows it, and the blank after a frame is one refresh on, so a session on
//! this backend draws at its mode's rate and no faster (a test that wants a
//! frame after one it just drew waits the same way the session's loop does,
//! [`Compositor::draw_frame`](crate::Compositor::draw_frame)).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::pixman::PixmanRenderer;
use smithay::backend::renderer::{Bind, ExportMem, ImportDma as _, Offscreen};
use smithay::output::Output;
use smithay::reexports::pixman::Image;
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Size, Transform};
use tilo_color_pipeline::hdr_surface::{HdrLayer, HdrLayerRecording};

use super::{Backend, BackendError, Screenshot, failed};
use crate::capture::{CaptureError, render_to_memory};
use crate::output::{OutputChange, OutputConfig, apply_placement, refresh_interval};
use crate::render::{BACKGROUND, output_elements};
use crate::state::State;

struct HeadlessOutput {
    output: Output,
    /// The place its configuration asked for; `None` while the layout puts
    /// it beside the others.
    place: Option<(i32, i32)>,
    damage: OutputDamageTracker,
    target: Image<'static, 'static>,
    /// Whether the target holds the previous frame (the damage tracker's
    /// buffer age is then 1, else 0).
    drawn: bool,
    /// The vertical blank the simulated display next shows a frame at: the
    /// grid its mode's refresh sets. What changed before it waits for it.
    next_blank: Instant,
    /// Switched off: nothing is drawn, the image goes black.
    dark: bool,
    /// The gamma ramp the simulated display engine applies to what it
    /// shows (screenshots), not to what is drawn (captures).
    gamma: Option<crate::gamma::Ramp>,
    /// What the panel is configured to show, as a real sink's EDID and
    /// signal would say.
    colour: tilo_color_pipeline::hdr_display::DisplayCapabilities,
}

/// The vertical blank after `blank` that is still ahead of `drawn`: one
/// refresh on, or the first one after a frame that took longer than a
/// refresh missed its blanks instead of bursting to catch up on them.
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

impl HeadlessOutput {
    /// This display showed the frame just drawn: the next blank is one
    /// refresh on, on the grid its mode keeps.
    fn showed(&mut self) {
        let interval = refresh_interval(self.output.current_mode().map_or(0, |mode| mode.refresh));
        self.next_blank = blank_after(self.next_blank, interval, Instant::now());
    }
}

/// The ramp size headless outputs carry, as many display engines do.
const GAMMA_SIZE: usize = 256;

/// The CPU renderer and every headless output's image.
pub struct HeadlessBackend {
    renderer: PixmanRenderer,
    outputs: Vec<HeadlessOutput>,
}

impl HeadlessBackend {
    /// A backend with no outputs yet.
    pub fn new(state: &mut State) -> Result<Self, BackendError> {
        let renderer = PixmanRenderer::new().map_err(|error| failed("pixman renderer", error))?;
        // Clients' dma-bufs are read through a mapping.
        state.enable_dmabuf(renderer.dmabuf_formats().iter().copied().collect(), None);
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

/// The composited frame of an output that shows HDR layers: the scene drawn
/// twice with the layers replaced by black and by their channel colours, and
/// the two composited at each layer's signal.
fn composite_hdr(
    renderer: &mut PixmanRenderer,
    state: &State,
    output: &Output,
    (width, height): (usize, usize),
    scale: f64,
    layers: &[HdrLayer],
) -> Result<Vec<u8>, BackendError> {
    let size = Size::<i32, Physical>::from((
        i32::try_from(width).map_err(|error| failed("width", error))?,
        i32::try_from(height).map_err(|error| failed("height", error))?,
    ));
    let transform = output.current_transform();
    let pass = |renderer: &mut PixmanRenderer, pixels: &mut Option<Vec<u8>>| {
        let elements = output_elements(renderer, state, output);
        *pixels = Some(render_to_memory::<
            PixmanRenderer,
            Image<'static, 'static>,
            _,
        >(
            renderer, size, scale, transform, &elements, BACKGROUND
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
    let size = (
        u32::try_from(width).map_err(|error| failed("width", error))?,
        u32::try_from(height).map_err(|error| failed("height", error))?,
    );
    Ok(crate::hdr::composite(&first, &second, size, layers))
}

/// The composited frame as the one element the output draws: every pixel of
/// it, in the output's physical size.
fn frame_element(
    renderer: &mut PixmanRenderer,
    rgba: &[u8],
    (width, height): (usize, usize),
    scale: f64,
) -> Result<MemoryRenderBufferRenderElement<PixmanRenderer>, BackendError> {
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
        // Right of the outputs already here until the layout places it: a
        // connector lit for the first time never lands on one showing.
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
                output.next_blank = Instant::now();
            }
            apply_placement(state, &output.output, change);
            if change.position.is_some() {
                output.place = change.position;
            }
            output.damage = OutputDamageTracker::from_output(&output.output);
        }
        Ok(())
    }

    /// Its outputs are images the CPU composes into: a hosted surface's
    /// pixels reach them as memory, never as a texture a GPU samples.
    fn composes_on_cpu(&self) -> bool {
        true
    }

    fn render(&mut self, state: &mut State) -> Result<(), BackendError> {
        for output in &mut self.outputs {
            if output.dark {
                continue;
            }
            // The display shows one frame per refresh: what changed waits for
            // the blank that shows it, so the frame it owes is still owed.
            if Instant::now() < output.next_blank {
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
                output.drawn = true;
                state.frame_shown(&output.output);
                output.showed();
                continue;
            }
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
                .map_err(|error| failed("render output", error))?;
            output.drawn = true;
            state.frame_shown(&output.output);
            output.showed();
        }
        Ok(())
    }

    fn next_deadline(&self, state: &State) -> Option<Instant> {
        self.outputs
            .iter()
            .filter(|output| !output.dark && state.redraw_owed(&output.output))
            .map(|output| output.next_blank)
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
        output.next_blank = Instant::now();
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
        crate::backdrop::draw::<_, Image<'static, 'static>>(
            &mut self.renderer,
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
        crate::capture::copy::<_, Image<'static, 'static>>(&mut self.renderer, state, frame)
    }

    fn read_back(
        &mut self,
        state: &mut State,
        target: &crate::capture::Target,
    ) -> Result<Screenshot, crate::capture::CaptureError> {
        crate::capture::read_back::<_, Image<'static, 'static>>(&mut self.renderer, state, target)
    }

    fn read_back_scaled(
        &mut self,
        state: &mut State,
        target: &crate::capture::Target,
        max: (u32, u32),
    ) -> Result<Screenshot, crate::capture::CaptureError> {
        crate::capture::read_back_scaled::<_, Image<'static, 'static>>(
            &mut self.renderer,
            state,
            target,
            max,
        )
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
        let framebuffer = self
            .renderer
            .bind(&mut output.target)
            .map_err(|error| failed("bind output", error))?;
        // ABGR8888 is R, G, B, A in memory.
        let mapping = self
            .renderer
            .copy_framebuffer(&framebuffer, Rectangle::from_size(size), Fourcc::Abgr8888)
            .map_err(|error| failed("read back", error))?;
        let bytes = self
            .renderer
            .map_texture(&mapping)
            .map_err(|error| failed("map", error))?;
        let mut rgba = bytes.to_vec();
        if let Some(ramp) = &output.gamma {
            ramp.apply(&mut rgba);
        }
        Ok(Screenshot {
            width: u32::try_from(width).map_err(|error| failed("width", error))?,
            height: u32::try_from(height).map_err(|error| failed("height", error))?,
            rgba,
        })
    }
}
