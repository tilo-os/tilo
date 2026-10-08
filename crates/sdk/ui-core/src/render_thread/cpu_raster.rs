//! Software fine rasterization; wgpu only uploads and presents these pixels.

use tilo_cpu_raster::{CpuRasterizer, Options, Region, Target};

use super::{FrameContent, bounded_present_damage};

pub(super) struct CpuCanvas {
    rasterizer: CpuRasterizer,
    pixels: Vec<u8>,
    size: (u32, u32),
    valid: bool,
    regions: Vec<Region>,
}

impl CpuCanvas {
    pub(super) fn new() -> Self {
        Self {
            rasterizer: CpuRasterizer::new(Options {
                threads: 4,
                ..Options::default()
            }),
            pixels: Vec::new(),
            size: (0, 0),
            valid: false,
            regions: Vec::new(),
        }
    }

    pub(super) fn invalidate(&mut self) {
        self.valid = false;
    }

    pub(super) fn rasterize(&mut self, frame: &FrameContent<'_>) -> Result<u64, String> {
        if frame.width == 0 || frame.height == 0 {
            return Err("CPU target dimensions must be nonzero".into());
        }
        let size = (frame.width, frame.height);
        let stride = frame.width.checked_mul(4).ok_or("CPU row overflow")?;
        let length = usize::try_from(u64::from(stride) * u64::from(frame.height))
            .map_err(|_| "CPU buffer overflow")?;
        if frame.width > u32::from(u16::MAX) || frame.height > u32::from(u16::MAX) {
            return Err("CPU target exceeds rasterizer dimensions".into());
        }
        if self.size != size {
            self.valid = false;
            if length > self.pixels.len() {
                self.pixels
                    .try_reserve(length.saturating_sub(self.pixels.len()))
                    .map_err(|error| error.to_string())?;
            }
            self.pixels.resize(length, 0);
            self.size = size;
        }
        self.regions = if frame.full_redraw || !self.valid {
            vec![Region::new(0, 0, frame.width, frame.height)]
        } else {
            bounded_present_damage(frame.width, frame.height, frame.damage_regions)
                .map(|regions| {
                    regions
                        .into_iter()
                        .map(|r| Region::new(r.x, r.y, r.width, r.height))
                        .collect()
                })
                .unwrap_or_else(|| vec![Region::new(0, 0, frame.width, frame.height)])
        };
        let mut target = Target::new(&mut self.pixels, frame.width, frame.height, stride as usize)
            .map_err(|error| error.to_string())?;
        self.rasterizer.prepare(frame.scene.encoding());
        self.rasterizer.render_prepared(
            &mut target,
            Some(&self.regions),
            tilo_cpu_raster::color::AlphaColor::new(frame.base_color.components),
        );
        self.valid = true;
        Ok(self
            .regions
            .iter()
            .map(|r| u64::from(r.width) * u64::from(r.height))
            .sum())
    }

    pub(super) fn upload(&self, queue: &vello::wgpu::Queue, texture: &vello::wgpu::Texture) {
        let stride = self.size.0.saturating_mul(4);
        for region in &self.regions {
            let offset = u64::from(region.y) * u64::from(stride) + u64::from(region.x) * 4;
            let Some(bytes) = usize::try_from(offset)
                .ok()
                .and_then(|offset| self.pixels.get(offset..))
            else {
                continue;
            };
            queue.write_texture(
                vello::wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: vello::wgpu::Origin3d {
                        x: region.x,
                        y: region.y,
                        z: 0,
                    },
                    aspect: vello::wgpu::TextureAspect::All,
                },
                bytes,
                vello::wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride),
                    rows_per_image: Some(self.size.1),
                },
                vello::wgpu::Extent3d {
                    width: region.width,
                    height: region.height,
                    depth_or_array_layers: 1,
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vello::{
        Scene,
        kurbo::{Affine, Rect},
        peniko::{Color, Fill},
    };

    fn scene(rect: Rect) -> Scene {
        let mut scene = Scene::new();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(255, 0, 0, 128),
            None,
            &rect,
        );
        scene
    }

    fn render(canvas: &mut CpuCanvas, scene: &Scene, damage: &[Rect], full: bool) {
        canvas
            .rasterize(&FrameContent {
                scene,
                width: 16,
                height: 16,
                base_color: Color::TRANSPARENT,
                damage_regions: damage,
                full_redraw: full,
                backdrop: &crate::backdrop::BackdropFrame::default(),
            })
            .expect("valid CPU frame");
    }

    #[test]
    fn damaged_move_erases_old_pixels_and_matches_full_render() {
        let mut canvas = CpuCanvas::new();
        render(
            &mut canvas,
            &scene(Rect::new(1.0, 1.0, 5.0, 5.0)),
            &[],
            true,
        );
        assert_eq!(&canvas.pixels[136..140], &[128, 0, 0, 128]);
        let next = scene(Rect::new(6.0, 1.0, 10.0, 5.0));
        render(&mut canvas, &next, &[Rect::new(1.0, 1.0, 10.0, 5.0)], false);
        let mut oracle = CpuCanvas::new();
        render(&mut oracle, &next, &[], true);
        assert_eq!(canvas.pixels, oracle.pixels);
        assert_eq!(canvas.regions, vec![Region::new(1, 1, 9, 4)]);
    }

    #[test]
    fn gpu_handoff_invalidates_cpu_damage_history() {
        let mut canvas = CpuCanvas::new();
        render(
            &mut canvas,
            &scene(Rect::new(1.0, 1.0, 5.0, 5.0)),
            &[],
            true,
        );
        canvas.invalidate();
        let next = scene(Rect::new(6.0, 1.0, 10.0, 5.0));
        render(&mut canvas, &next, &[Rect::new(6.0, 1.0, 10.0, 5.0)], false);
        let mut oracle = CpuCanvas::new();
        render(&mut oracle, &next, &[], true);
        assert_eq!(canvas.pixels, oracle.pixels);
        assert_eq!(canvas.regions, vec![Region::new(0, 0, 16, 16)]);
    }
}
