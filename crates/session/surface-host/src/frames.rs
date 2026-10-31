//! Where a hosted surface's frame is drawn.
//!
//! A surface app's scene is drawn the way every toolkit app's frame is: by
//! the toolkit's own renderer ([`ui::offscreen::OffscreenRenderer`]), on
//! whatever adapter it picks; where the machine has no GPU adapter to draw
//! on, by the toolkit's CPU rasterizer, exactly as it draws a normal app's
//! frame there. A device lost under the renderer gives way to the same CPU
//! rasterizer, so a GPU that stops leaves the surface slower rather than
//! stopped. Only what changed is drawn, so a moved cursor, a new time or
//! a resized panel costs what it changed rather than the whole surface.

use tilo_compositor::{HostFrame, Region};
use ui::offscreen::OffscreenRenderer;
use vello::Scene;
use vello::kurbo::Rect;
use vello::peniko::Color;
use vello::wgpu::AdapterInfo;

/// The colour a surface draws on: nothing, so the desktop shows through
/// wherever the app painted nothing.
const TRANSPARENT: Color = Color::TRANSPARENT;

/// Cores the CPU rasterizer uses where it has them.
const RASTER_THREADS: u16 = 2;

/// One hosted surface's frames: what it drew last, and what draws its next.
pub(crate) struct Frames {
    size: (u32, u32),
    /// What draws them, or `None` where this build's toolkit has no
    /// rasterizer to draw a surface with.
    target: Option<Target>,
    /// What the compositor draws: the surface's pixels, changed where it
    /// changed.
    frame: HostFrame,
}

impl Frames {
    /// Frames for a surface of `size` physical pixels. Nothing is drawn
    /// before [`Self::present`].
    pub(crate) fn new(size: (u32, u32)) -> Self {
        Self {
            size,
            target: Target::new(size),
            frame: HostFrame::new(size),
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
        self.frame = HostFrame::new(size);
    }

    /// The frame as the compositor shows it.
    pub(crate) fn frame(&self) -> HostFrame {
        self.frame.clone()
    }

    pub(crate) const fn size(&self) -> (u32, u32) {
        self.size
    }

    /// The alpha of one pixel of the last frame drawn, if it lies inside.
    pub(crate) fn alpha(&self, x: u32, y: u32) -> Option<u8> {
        alpha_of(self.target.as_ref()?.pixels(), self.size, x, y)
    }

    /// Draw `scene` (physical pixels) where `damage` says it changed (`None`:
    /// all of it) and copy what was drawn into the frame, telling it what
    /// changed.
    pub(crate) fn present(&mut self, scene: &Scene, damage: Option<&[Rect]>) {
        // A device that stopped working is noticed where the frames are
        // drawn, and the frames go on: on the software rasteriser where the
        // GPU cannot be brought back, so the surface stays there rather than
        // drawing into a dead device.
        if let Some(reason) = self.target.as_ref().and_then(Target::device_loss) {
            self.target = self
                .target
                .take()
                .and_then(|target| target.after_loss(&reason, self.size));
        }
        let Some(target) = self.target.as_mut() else {
            return;
        };
        let Some(drawn) = target.draw(scene, damage) else {
            return;
        };
        self.frame.set_pixels(target.pixels(), &drawn);
    }
}

/// Where a surface's frames are drawn: on the GPU, or on the toolkit's CPU
/// rasterizer where the machine has no GPU to draw them on.
enum Target {
    Gpu(Box<Gpu>),
    Cpu(Box<ui::offscreen::CpuSurface>),
}

impl Target {
    /// The drawing `size` frames, on the GPU where this machine has one and
    /// on the CPU where the toolkit draws every frame there; `None` where
    /// this build's toolkit has no rasterizer to draw one with.
    fn new(size: (u32, u32)) -> Option<Self> {
        if let Some(gpu) = Gpu::new(size) {
            return Some(Self::Gpu(Box::new(gpu)));
        }
        tracing::info!("surface apps' frames are drawn on the CPU: no GPU to draw them on");
        Self::cpu(size)
    }

    /// The toolkit's CPU rasterizer drawing a `size` surface: what a machine
    /// with no GPU to draw on draws with, and what a surface draws with once
    /// its GPU cannot be brought back. `None` where this build has no
    /// rasterizer.
    fn cpu(size: (u32, u32)) -> Option<Self> {
        let canvas = ui::offscreen::CpuSurface::new(size.0, size.1, RASTER_THREADS)?;
        Some(Self::Cpu(Box::new(canvas)))
    }

    /// Why this target's GPU device is gone, while it is: what the next frame
    /// is not drawn on.
    fn device_loss(&self) -> Option<String> {
        match self {
            Self::Gpu(gpu) => gpu.renderer.device_loss(),
            Self::Cpu(_) => None,
        }
    }

    /// What draws a `size` surface after this target lost its GPU device for
    /// `reason`: the answer `ui::gpu::recover` decides, and the drawing that
    /// fits it — the CPU rasterizer once the GPU cannot be brought back, the
    /// GPU again after a reset that left it working.
    fn after_loss(self, reason: &str, size: (u32, u32)) -> Option<Self> {
        let Self::Gpu(gpu) = self else {
            return Self::new(size);
        };
        let recovery = ui::gpu::recover(&gpu.adapter, gpu.finished_frames, reason);
        tracing::error!(%reason, ?recovery, "a hosted surface's GPU device was lost");
        match recovery {
            // The GPU was reset and works again: open a renderer on it. A
            // device that stays lost ends here again on the next pass, then
            // with no frame finished and gives way to the CPU.
            ui::gpu::Recovery::SameAdapter => Self::new(size),
            ui::gpu::Recovery::Software => Self::cpu(size),
        }
    }

    /// The last frame drawn, premultiplied RGBA, `width * 4` bytes a row:
    /// what the surface covers, for the pointer.
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
            Self::Gpu(gpu) => gpu.draw(scene, damage),
            Self::Cpu(cpu) => {
                let (width, height) = cpu.size();
                let changed = match damage {
                    Some(rects) => regions(rects, width, height),
                    None => vec![Region::new(0, 0, width, height)],
                };
                let rects: Vec<(u32, u32, u32, u32)> = changed
                    .iter()
                    .map(|region| (region.x, region.y, region.width, region.height))
                    .collect();
                cpu.draw(scene, &rects).then_some(changed)
            }
        }
    }
}

/// The renderer and the frame it drew last.
struct Gpu {
    renderer: OffscreenRenderer,
    /// The adapter this renderer draws on: what a loss of its device is
    /// decided by ([`ui::gpu::recover`]).
    adapter: AdapterInfo,
    /// Whether this renderer finished at least one frame before the loss: a
    /// device that did was reset and works again, one that did not is on a
    /// GPU that cannot be brought back.
    finished_frames: bool,
    /// Premultiplied RGBA, `width * 4` bytes a row, of the whole surface.
    pixels: Vec<u8>,
}

impl Gpu {
    /// The renderer for a surface of `size` physical pixels, if the toolkit
    /// has an adapter to draw it on: an emulated adapter is no GPU, and the
    /// toolkit rasterizes on the CPU there too.
    fn new(size: (u32, u32)) -> Option<Self> {
        if size.0 == 0 || size.1 == 0 {
            return None;
        }
        let renderer = match OffscreenRenderer::try_new_live(size.0, size.1) {
            Ok(renderer) => renderer,
            Err(error) => {
                tracing::warn!(%error, "a hosted surface has no renderer to draw on");
                return None;
            }
        };
        let adapter = renderer
            .adapter_info()
            .filter(|info| !ui::render_thread::rasterizes_on_cpu(info))?;
        Some(Self {
            pixels: vec![0; length(size)],
            renderer,
            adapter,
            finished_frames: false,
        })
    }

    /// Draw `scene` where `damage` says it changed, keeping the drawn frame.
    /// The regions drawn, or `None` where nothing changed.
    fn draw(&mut self, scene: &Scene, damage: Option<&[Rect]>) -> Option<Vec<Region>> {
        let renderer = &mut self.renderer;
        let changed = match damage.map(|rects| regions(rects, renderer.width, renderer.height)) {
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
        self.pixels = renderer.readback_current();
        let complete = self.pixels.len() == length((renderer.width, renderer.height));
        self.finished_frames |= complete;
        complete.then_some(changed)
    }
}

/// The pixels of a `size` surface.
fn length(size: (u32, u32)) -> usize {
    (size.0 as usize)
        .saturating_mul(size.1 as usize)
        .saturating_mul(4)
}

/// The alpha of one pixel of `pixels`, a `size` surface, if it lies inside.
pub(crate) fn alpha_of(pixels: &[u8], size: (u32, u32), x: u32, y: u32) -> Option<u8> {
    if x >= size.0 || y >= size.1 {
        return None;
    }
    let index = (y as usize)
        .checked_mul(size.0 as usize)?
        .checked_add(x as usize)?
        .checked_mul(4)?
        .checked_add(3)?;
    pixels.get(index).copied()
}

/// `rects` as whole-pixel regions of a `width` by `height` target: rounded
/// out, clipped, and merged where they overlap so no pixel is drawn twice.
pub(crate) fn regions(rects: &[Rect], width: u32, height: u32) -> Vec<Region> {
    let bounds = Rect::new(0.0, 0.0, f64::from(width), f64::from(height));
    let mut merged: Vec<Rect> = Vec::new();
    for rect in rects {
        let mut rect = rect.expand().intersect(bounds);
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            continue;
        }
        while let Some(index) = merged
            .iter()
            .position(|other| other.intersect(rect).area() > 0.0)
        {
            rect = rect.union(merged.swap_remove(index));
        }
        merged.push(rect);
    }
    merged
        .into_iter()
        .map(|rect| {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "whole-pixel rectangles clipped to the target"
            )]
            let region = Region::new(
                rect.x0 as u32,
                rect.y0 as u32,
                rect.width() as u32,
                rect.height() as u32,
            );
            region
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello::kurbo::Affine;
    use vello::peniko::Fill;

    #[test]
    fn damage_is_rounded_out_clipped_and_merged() {
        let rects = [
            Rect::new(1.5, 1.5, 4.2, 4.2),
            Rect::new(3.0, 3.0, 6.0, 6.0),
            Rect::new(90.0, 90.0, 120.0, 120.0),
            Rect::new(-5.0, -5.0, -1.0, -1.0),
        ];
        let mut out = regions(&rects, 100, 100);
        out.sort_by_key(|region| (region.x, region.y));
        assert_eq!(
            out,
            vec![Region::new(1, 1, 5, 5), Region::new(90, 90, 10, 10)]
        );
    }

    /// A hosted surface's device can be lost under it. The surface then draws
    /// its frames where a machine with no GPU draws them, so the person still
    /// sees everything, only slower, instead of a surface that stops or a
    /// process that panics on a dead device.
    ///
    /// The decision itself is the toolkit's (`ui::gpu::recover`, tested in
    /// `ui-core`): this drives what follows it for a surface, from the loss
    /// of a device that never finished a frame, which cannot be brought back.
    #[test]
    fn a_gpu_given_up_leaves_the_surface_drawing_on_the_cpu() {
        ui::gpu::reset_software_fallback_for_test();
        let adapter = AdapterInfo {
            name: "Adreno X1-85".to_owned(),
            vendor: 0,
            device: 0,
            device_type: vello::wgpu::DeviceType::IntegratedGpu,
            device_pci_bus_id: String::new(),
            driver: String::new(),
            driver_info: String::new(),
            backend: vello::wgpu::Backend::Vulkan,
            subgroup_min_size: 4,
            subgroup_max_size: 128,
            transient_saves_memory: false,
        };
        assert_eq!(
            ui::gpu::recover(&adapter, false, "Device is lost"),
            ui::gpu::Recovery::Software,
            "a device lost before its first frame is on a GPU that cannot come back"
        );

        let size = (64, 48);
        let mut frames = Frames::new(size);
        assert!(
            matches!(frames.target, Some(Target::Cpu(_))),
            "the surface draws on the CPU rasterizer once its GPU is given up"
        );
        let mut scene = Scene::new();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::WHITE,
            None,
            &Rect::new(8.0, 8.0, 56.0, 40.0),
        );
        frames.present(&scene, None);
        assert!(
            frames.alpha(32, 24).is_some_and(|alpha| alpha > 0),
            "the frames are still drawn, in software"
        );
        ui::gpu::reset_software_fallback_for_test();
    }
}
