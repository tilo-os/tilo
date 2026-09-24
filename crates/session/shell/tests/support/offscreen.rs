//! Headless offscreen render harness for screenshot-based E2E tests.
//!
//! Bypasses winit + display entirely: a wgpu device with no surface, a
//! vello [`Renderer`] against it, a scene rendered into an offscreen
//! texture and copied back to the CPU — so screenshots work in CI-like
//! environments with neither a GNOME Shell nor a compositor.
//!
//! `copy_texture_to_buffer` requires a row pitch that is a multiple of
//! `COPY_BYTES_PER_ROW_ALIGNMENT` (256 on every Vulkan/Metal/D3D12 backend
//! shipped today), so a non-aligned width allocates the aligned size and
//! strips padding on readback; callers see a tight `width * 4` buffer.

#![allow(dead_code)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::path::Path;

use pollster::FutureExt;
use vello::peniko::Color;
use vello::util::RenderContext;
use vello::{AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, Scene};

/// An offscreen wgpu + vello pipeline sized at `width × height`. Reuse one
/// instance across renders to amortise adapter/device creation.
pub struct OffscreenRenderer {
    pub width: u32,
    pub height: u32,
    context: RenderContext,
    dev_id: usize,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    readback_buf: wgpu::Buffer,
    padded_bytes_per_row: u32,
    renderer: Renderer,
}

impl OffscreenRenderer {
    /// Panics if no suitable adapter exists (e.g. llvmpipe isn't installed);
    /// callers wanting a soft-fail path should wrap this in
    /// `std::panic::catch_unwind`.
    pub fn new(width: u32, height: u32) -> Self {
        assert!(width > 0 && height > 0, "width and height must be > 0");

        let mut context = RenderContext::new();
        let dev_id = context
            .device(None)
            .block_on()
            .expect("couldn't request a wgpu adapter — is llvmpipe installed?");
        let device = &context.devices[dev_id].device;
        let queue = &context.devices[dev_id].queue;
        let _ = queue;

        let format = wgpu::TextureFormat::Rgba8Unorm;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("quick-ui-offscreen-render-target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        // Padded buffer for the texture→buffer copy: row pitch must be a multiple
        // of 256 (wgpu::COPY_BYTES_PER_ROW_ALIGNMENT).
        let bytes_per_pixel = 4u32;
        let unpadded = width.saturating_mul(bytes_per_pixel);
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = unpadded.div_ceil(align).saturating_mul(align);
        let readback_size = u64::from(padded_bytes_per_row).saturating_mul(u64::from(height));
        let readback_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("quick-ui-offscreen-readback"),
            size: readback_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let renderer = Renderer::new(
            device,
            RendererOptions {
                use_cpu: false,
                antialiasing_support: AaSupport::all(),
                num_init_threads: None,
                pipeline_cache: None,
            },
        )
        .expect("vello Renderer::new");

        Self {
            width,
            height,
            context,
            dev_id,
            texture,
            view,
            readback_buf,
            padded_bytes_per_row,
            renderer,
        }
    }

    /// Raw RGBA pixels, tight-packed at `4 * width` bytes per row.
    pub fn render(&mut self, scene: &Scene, bg: Color) -> Vec<u8> {
        let device = &self.context.devices[self.dev_id].device;
        let queue = &self.context.devices[self.dev_id].queue;

        self.renderer
            .render_to_texture(
                device,
                queue,
                scene,
                &self.view,
                &RenderParams {
                    base_color: bg,
                    width: self.width,
                    height: self.height,
                    antialiasing_method: AaConfig::Area,
                },
            )
            .expect("render_to_texture");

        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &self.readback_buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.padded_bytes_per_row),
                    rows_per_image: Some(self.height),
                },
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(std::iter::once(encoder.finish()));

        // `map_async` is async but `poll(Wait)` drives it to completion without
        // an executor.
        let slice = self.readback_buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            tx.send(r).ok();
        });
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        rx.recv()
            .expect("map_async channel")
            .expect("buffer map failed");

        let mapped = slice.get_mapped_range();
        let mut tight =
            Vec::with_capacity((self.width.saturating_mul(self.height).saturating_mul(4)) as usize);
        let row_bytes = self.width.saturating_mul(4) as usize;
        for row in 0..self.height as usize {
            let start = row.saturating_mul(self.padded_bytes_per_row as usize);
            tight.extend_from_slice(&mapped[start..start.saturating_add(row_bytes)]);
        }
        drop(mapped);
        self.readback_buf.unmap();
        tight
    }

    /// Render a deterministic screenshot frame after Vello has populated its
    /// first-use glyph/path state. The live compositor naturally submits a
    /// following frame; one-shot tests must do that explicitly or can capture
    /// partially populated text and icon geometry.
    pub fn render_resilient(&mut self, scene: &Scene, bg: Color) -> Vec<u8> {
        let _warmup = self.render(scene, bg);
        self.render(scene, bg)
    }

    /// Render `scene` to `path` as a PNG, creating parent dirs as needed.
    pub fn render_png<P: AsRef<Path>>(&mut self, scene: &Scene, bg: Color, path: P) {
        let rgba = self.render_resilient(scene, bg);
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        ui::png::write_rgba8(path, self.width, self.height, &rgba).expect("write png file");
    }

    /// Sample the RGBA at `(x, y)` from the last render — tight-packed, not padded.
    pub fn sample(rgba: &[u8], width: u32, x: u32, y: u32) -> (u8, u8, u8, u8) {
        let idx = (y.saturating_mul(width).saturating_add(x).saturating_mul(4)) as usize;
        (
            rgba[idx],
            rgba[idx.saturating_add(1)],
            rgba[idx.saturating_add(2)],
            rgba[idx.saturating_add(3)],
        )
    }
}
