//! Where a hosted surface's frame is drawn.
//!
//! A surface app's scene is drawn the way every toolkit app's frame is: on
//! the GPU, through the toolkit's vello renderer
//! ([`ui::offscreen::OffscreenRenderer`]). Where the machine has no GPU
//! adapter the toolkit's CPU rasterizer draws the frame instead, exactly as
//! it draws a normal app's on that machine. Only what changed is drawn: both
//! keep the frame they drew last, so a moved cursor, a new time or a resized
//! panel costs what it changed rather than the whole surface.

use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use tilo_cpu_raster::Region;
use ui::offscreen::OffscreenRenderer;
use vello::Scene;
use vello::kurbo::Rect;
use vello::peniko::Color;

use crate::canvas::{Canvas, RASTER_THREADS, alpha_of, regions, write_pixels};

/// The colour a surface draws on: nothing, so the desktop shows through
/// wherever the app painted nothing.
const TRANSPARENT: Color = Color::new([0.0, 0.0, 0.0, 0.0]);

/// One hosted surface's frames: what it drew last, and what draws its next.
pub(crate) struct Frames {
    size: (u32, u32),
    target: Target,
}

impl Frames {
    /// Frames for a surface of `size` physical pixels. Nothing is drawn
    /// before [`Self::present`].
    pub(crate) fn new(size: (u32, u32)) -> Self {
        Self {
            size,
            target: Target::new(size),
        }
    }

    /// The surface is now `size` physical pixels: the frames drawn for the
    /// old size are not its own.
    pub(crate) fn resize(&mut self, size: (u32, u32)) {
        if size == self.size {
            return;
        }
        self.size = size;
        self.target = Target::new(size);
    }

    pub(crate) const fn size(&self) -> (u32, u32) {
        self.size
    }

    /// The alpha of one pixel of the last frame drawn, if it lies inside.
    pub(crate) fn alpha(&self, x: u32, y: u32) -> Option<u8> {
        alpha_of(self.target.pixels(), self.size, x, y)
    }

    /// Draw `scene` (physical pixels) where `damage` says it changed (`None`:
    /// all of it) and copy what was drawn into `buffer`, telling it what
    /// changed.
    pub(crate) fn present(
        &mut self,
        scene: &Scene,
        damage: Option<&[Rect]>,
        buffer: &mut MemoryRenderBuffer,
    ) {
        let Some(drawn) = self.target.draw(scene, damage) else {
            return;
        };
        write_pixels(self.target.pixels(), self.size, &drawn, buffer);
    }
}

/// Where a surface's frames are drawn: on the GPU, or on the CPU where the
/// machine has none.
enum Target {
    Gpu(Box<Gpu>),
    Cpu(Box<Canvas>),
}

/// The GPU renderer and the frame it drew last.
struct Gpu {
    renderer: OffscreenRenderer,
    /// Premultiplied RGBA, `width * 4` bytes a row, of the whole surface.
    pixels: Vec<u8>,
}

impl Target {
    fn new(size: (u32, u32)) -> Self {
        if let Some(renderer) = Self::gpu(size) {
            return Self::Gpu(Box::new(Gpu {
                pixels: vec![0; length(size)],
                renderer,
            }));
        }
        tracing::info!("surface apps' frames are drawn on the CPU: no GPU to draw them on");
        Self::Cpu(Box::new(Canvas::new(size, RASTER_THREADS)))
    }

    /// The renderer for the GPU this machine draws on, if it has one: an
    /// emulated adapter is no GPU, and there the toolkit rasterizes frames on
    /// the CPU too.
    fn gpu(size: (u32, u32)) -> Option<OffscreenRenderer> {
        if size.0 == 0 || size.1 == 0 {
            return None;
        }
        let renderer = OffscreenRenderer::try_new_live(size.0, size.1).ok()?;
        let info = renderer.adapter_info()?;
        (!ui::render_thread::rasterizes_on_cpu(&info)).then_some(renderer)
    }

    /// The last frame drawn, premultiplied RGBA, `width * 4` bytes a row: what
    /// the surface covers, for the pointer.
    fn pixels(&self) -> &[u8] {
        match self {
            Self::Gpu(gpu) => &gpu.pixels,
            Self::Cpu(canvas) => canvas.pixels(),
        }
    }

    /// Draw `scene` where `damage` says it changed, keeping the drawn frame.
    /// The regions drawn, or `None` where nothing changed.
    fn draw(&mut self, scene: &Scene, damage: Option<&[Rect]>) -> Option<Vec<Region>> {
        match self {
            Self::Gpu(gpu) => {
                let renderer = &mut gpu.renderer;
                let changed =
                    match damage.map(|rects| regions(rects, renderer.width, renderer.height)) {
                        Some(changed) if changed.is_empty() => return None,
                        Some(changed) => {
                            let rects: Vec<Rect> = changed
                                .iter()
                                .map(|region| {
                                    Rect::new(
                                        f64::from(region.x),
                                        f64::from(region.y),
                                        f64::from(region.x.saturating_add(region.width)),
                                        f64::from(region.y.saturating_add(region.height)),
                                    )
                                })
                                .collect();
                            renderer.render_damage_regions_gpu_wait(scene, TRANSPARENT, &rects);
                            changed
                        }
                        None => {
                            renderer.render_gpu_wait(scene, TRANSPARENT);
                            vec![Region::new(0, 0, renderer.width, renderer.height)]
                        }
                    };
                gpu.pixels = renderer.readback_current();
                (gpu.pixels.len() == length((renderer.width, renderer.height))).then_some(changed)
            }
            Self::Cpu(canvas) => {
                let changed = canvas.draw(scene, damage);
                (!changed.is_empty()).then_some(changed)
            }
        }
    }
}

/// The pixels of a `size` surface.
fn length(size: (u32, u32)) -> usize {
    (size.0 as usize)
        .saturating_mul(size.1 as usize)
        .saturating_mul(4)
}
