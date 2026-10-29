//! Outputs with no display: composited on the CPU into images that tests,
//! screenshots and a viewer read back.
//!
//! No GPU and no devices; input arrives through
//! [`Compositor::input`](crate::Compositor::input). Frames are drawn when
//! asked ([`Compositor::render`](crate::Compositor::render)), each drawing
//! only what changed since the last.

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::pixman::PixmanRenderer;
use smithay::backend::renderer::{Bind, ExportMem, ImportDma as _, Offscreen};
use smithay::output::Output;
use smithay::reexports::pixman::Image;
use smithay::utils::{Buffer, Rectangle, Size};

use super::{Backend, BackendError, Screenshot, failed};
use crate::output::{OutputChange, OutputConfig, apply_placement};
use crate::render::{BACKGROUND, output_elements};
use crate::state::State;

struct HeadlessOutput {
    output: Output,
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
}

impl Backend for HeadlessBackend {
    fn add_output(&mut self, state: &mut State, config: &OutputConfig) -> Result<(), BackendError> {
        let output = config.create(&state.display, "headless");
        let size = Size::<i32, Buffer>::from(config.size);
        let target = self
            .renderer
            .create_buffer(Fourcc::Argb8888, size)
            .map_err(|error| failed("output image", error))?;
        state.add_output(&output, config.location());
        state.gamma.set_size(&config.name, Some(GAMMA_SIZE));
        let damage = OutputDamageTracker::from_output(&output);
        self.outputs.push(HeadlessOutput {
            output,
            damage,
            target,
            drawn: false,
            dark: false,
            gamma: None,
        });
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
            output.damage = OutputDamageTracker::from_output(&output.output);
        }
        Ok(())
    }

    fn render(&mut self, state: &mut State) -> Result<(), BackendError> {
        for output in &mut self.outputs {
            if output.dark {
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

    fn capture(
        &mut self,
        state: &mut State,
        frame: &crate::capture::Frame,
    ) -> Result<crate::capture::Copied, crate::capture::CaptureError> {
        crate::capture::copy::<_, Image<'static, 'static>>(&mut self.renderer, state, frame)
    }

    fn screenshot(&mut self, name: &str) -> Result<Screenshot, BackendError> {
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
