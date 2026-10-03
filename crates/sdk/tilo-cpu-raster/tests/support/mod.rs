//! Reference rendering through vello's GPU pipeline and side-by-side
//! comparison with the CPU replay.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "test harness: failures are the assertion"
)]

use std::path::PathBuf;

use pollster::FutureExt;
use tilo_cpu_raster::compare::{Diff, compare, premultiply};
use tilo_cpu_raster::{CpuRasterizer, Options, Region, Target};
use vello::peniko::Color;
use vello::util::RenderContext;
use vello::wgpu;
use vello::{AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, Scene};

/// vello on whatever wgpu adapter is present (lavapipe in CI).
pub struct Gpu {
    context: RenderContext,
    dev: usize,
    renderer: Renderer,
}

impl Gpu {
    /// `None` when no adapter exists.
    pub fn new() -> Option<Self> {
        let mut context = RenderContext::new();
        let dev = context.device(None).block_on()?;
        let renderer = Renderer::new(
            &context.devices[dev].device,
            RendererOptions {
                use_cpu: false,
                antialiasing_support: AaSupport::area_only(),
                num_init_threads: None,
                pipeline_cache: None,
            },
        )
        .ok()?;
        Some(Self {
            context,
            dev,
            renderer,
        })
    }

    pub fn adapter(&self) -> String {
        let info = self.context.devices[self.dev].adapter().get_info();
        format!("{} ({:?})", info.name, info.backend)
    }

    /// Premultiplied RGBA8, tightly packed.
    pub fn render(&mut self, scene: &Scene, width: u32, height: u32, base: Color) -> Vec<u8> {
        let device = &self.context.devices[self.dev].device;
        let queue = &self.context.devices[self.dev].queue;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let padded = (width * 4).div_ceil(256) * 256;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: u64::from(padded * height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let params = RenderParams {
            base_color: base,
            width,
            height,
            antialiasing_method: AaConfig::Area,
        };
        // The first render populates per-scene state; the second is the frame.
        for _ in 0..2 {
            self.renderer
                .render_to_texture(device, queue, scene, &view, &params)
                .expect("vello renders");
        }
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);
        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            tx.send(r).ok();
        });
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        rx.recv().unwrap().unwrap();
        let mapped = slice.get_mapped_range();
        let mut out = Vec::with_capacity((width * height * 4) as usize);
        for row in 0..height as usize {
            let start = row * padded as usize;
            out.extend_from_slice(&mapped[start..start + width as usize * 4]);
        }
        drop(mapped);
        buffer.unmap();
        premultiply(&mut out);
        out
    }
}

/// Render through the CPU replay into a tight buffer.
pub fn cpu_render(
    raster: &mut CpuRasterizer,
    scene: &Scene,
    width: u32,
    height: u32,
    base: Color,
    regions: Option<&[Region]>,
) -> Vec<u8> {
    let mut out = vec![0u8; (width * height * 4) as usize];
    let mut target = Target::new(&mut out, width, height, width as usize * 4).unwrap();
    raster.render(scene.encoding(), &mut target, regions, base);
    out
}

pub fn out_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("cpu-raster");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Write premultiplied RGBA8 as a PNG (unpremultiplied for viewing).
pub fn write_png(name: &str, width: u32, height: u32, premul: &[u8]) {
    let mut straight = premul.to_vec();
    for px in straight.chunks_exact_mut(4) {
        let a = u16::from(px[3]);
        for c in &mut px[..3] {
            if let Some(v) = (u16::from(*c) * 255 + a / 2).checked_div(a) {
                *c = v.min(255) as u8;
            }
        }
    }
    let file = std::fs::File::create(out_dir().join(name)).unwrap();
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .unwrap()
        .write_image_data(&straight)
        .unwrap();
}

/// Amplified difference image: grey where equal, red where they differ.
pub fn write_diff_png(name: &str, width: u32, height: u32, a: &[u8], b: &[u8]) {
    let mut out = Vec::with_capacity(a.len());
    for (pa, pb) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        let d = pa
            .iter()
            .zip(pb)
            .map(|(x, y)| x.abs_diff(*y))
            .max()
            .unwrap_or(0);
        let luma = (u16::from(pa[0]) + u16::from(pa[1]) + u16::from(pa[2])) / 12;
        if d == 0 {
            out.extend_from_slice(&[luma as u8, luma as u8, luma as u8, 255]);
        } else {
            out.extend_from_slice(&[(64 + u16::from(d) * 6).min(255) as u8, 0, 0, 255]);
        }
    }
    write_png(name, width, height, &out);
}

/// Compare `scene` through both renderers, keep PNGs of both and the
/// difference, and return the diff.
pub fn parity(
    gpu: &mut Gpu,
    raster: &mut CpuRasterizer,
    name: &str,
    scene: &Scene,
    width: u32,
    height: u32,
    base: Color,
) -> Diff {
    let reference = gpu.render(scene, width, height, base);
    let ours = cpu_render(raster, scene, width, height, base, None);
    write_png(&format!("{name}.gpu.png"), width, height, &reference);
    write_png(&format!("{name}.cpu.png"), width, height, &ours);
    write_diff_png(
        &format!("{name}.diff.png"),
        width,
        height,
        &reference,
        &ours,
    );
    compare(&reference, &ours)
}

pub fn new_raster() -> CpuRasterizer {
    CpuRasterizer::new(Options::default())
}
