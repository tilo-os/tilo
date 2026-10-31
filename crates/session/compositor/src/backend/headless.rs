//! Outputs with no display: composited on the CPU into images that tests,
//! screenshots and a viewer read back.
//!
//! No GPU and no devices; input arrives through
//! [`Compositor::input`](crate::Compositor::input). Frames are drawn when
//! asked ([`Compositor::render`](crate::Compositor::render)), each drawing
//! only what changed since the last.

use std::collections::HashMap;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::pixman::PixmanRenderer;
use smithay::backend::renderer::{Bind, ExportMem, ImportDma as _, Offscreen};
use smithay::output::Output;
use smithay::reexports::pixman::Image;
use smithay::utils::{Buffer, Point, Rectangle, Size};

use super::{Backend, BackendError, Screenshot, failed};
use crate::output::{OutputChange, OutputConfig, apply_placement};
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
    /// Switched off: nothing is drawn, the image goes black.
    dark: bool,
    /// The gamma ramp the simulated display engine applies to what it
    /// shows (screenshots), not to what is drawn (captures).
    gamma: Option<crate::gamma::Ramp>,
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

impl Backend for HeadlessBackend {
    fn describe(&self) -> &'static str {
        "headless"
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
            dark: false,
            gamma: None,
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
            // Nothing new to show: an idle output draws nothing.
            if !state.take_redraw(&output.output) {
                continue;
            }
            let elements = output_elements(&mut self.renderer, state, &output.output);
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
        }
        Ok(())
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
