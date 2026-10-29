//! Outputs with no display: composited on the CPU into images that tests,
//! screenshots and a viewer read back.
//!
//! No GPU and no devices; input arrives through
//! [`Compositor::input`](crate::Compositor::input). Frames are drawn when
//! asked ([`Compositor::render`](crate::Compositor::render)) and each draws
//! only what changed since the last.

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::pixman::PixmanRenderer;
use smithay::backend::renderer::{Bind, ExportMem, Offscreen};
use smithay::output::Output;
use smithay::reexports::pixman::Image;
use smithay::utils::{Buffer, Rectangle, Size};

use crate::output::OutputConfig;
use crate::render::output_elements;
use crate::state::State;

/// The colour behind every window: the desktop's background before a
/// wallpaper surface covers it.
const BACKGROUND: [f32; 4] = [0.102, 0.106, 0.149, 1.0];

/// An output's pixels, read back after a frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Screenshot {
    /// Width in physical pixels.
    pub width: u32,
    /// Height in physical pixels.
    pub height: u32,
    /// Rows of RGBA8, top first, `width * 4` bytes each.
    pub rgba: Vec<u8>,
}

impl Screenshot {
    /// The RGBA of one pixel, if it lies inside the image.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let offset = (y as usize)
            .checked_mul(self.width as usize)?
            .checked_add(x as usize)?
            .checked_mul(4)?;
        let pixel = self.rgba.get(offset..offset.checked_add(4)?)?;
        pixel.try_into().ok()
    }
}

/// Why a headless frame failed.
#[derive(Debug)]
pub struct RenderError(String);

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RenderError {}

fn failed(what: &str, error: impl std::fmt::Debug) -> RenderError {
    RenderError(format!("{what}: {error:?}"))
}

struct HeadlessOutput {
    output: Output,
    damage: OutputDamageTracker,
    target: Image<'static, 'static>,
    /// Whether the target holds the previous frame (the damage tracker's
    /// buffer age is then 1, else 0).
    drawn: bool,
}

/// The CPU renderer and every headless output's image.
pub(crate) struct HeadlessBackend {
    renderer: PixmanRenderer,
    outputs: Vec<HeadlessOutput>,
}

impl HeadlessBackend {
    pub(crate) fn new() -> Result<Self, RenderError> {
        let renderer = PixmanRenderer::new().map_err(|error| failed("pixman renderer", error))?;
        Ok(Self {
            renderer,
            outputs: Vec::new(),
        })
    }

    pub(crate) fn add_output(
        &mut self,
        state: &mut State,
        config: &OutputConfig,
    ) -> Result<(), RenderError> {
        let output = config.create(&state.display, "headless");
        let size = Size::<i32, Buffer>::from(config.size);
        let target = self
            .renderer
            .create_buffer(Fourcc::Argb8888, size)
            .map_err(|error| failed("output image", error))?;
        state.add_output(&output, config.location());
        let damage = OutputDamageTracker::from_output(&output);
        self.outputs.push(HeadlessOutput {
            output,
            damage,
            target,
            drawn: false,
        });
        Ok(())
    }

    /// Draw every output's frame and tell its clients it was shown.
    pub(crate) fn render(&mut self, state: &State) -> Result<(), RenderError> {
        for output in &mut self.outputs {
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

    /// The last frame of the output named `name`.
    pub(crate) fn screenshot(&mut self, name: &str) -> Result<Screenshot, RenderError> {
        let output = self
            .outputs
            .iter_mut()
            .find(|output| output.output.name() == name)
            .ok_or_else(|| RenderError(format!("no output named {name}")))?;
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
        Ok(Screenshot {
            width: u32::try_from(width).map_err(|error| failed("width", error))?,
            height: u32::try_from(height).map_err(|error| failed("height", error))?,
            rgba: bytes.to_vec(),
        })
    }
}
