//! Headless offscreen render harness. Renders a Vello `Scene` into a
//! GPU texture without a window/surface, copies the bytes back to the
//! CPU, and (optionally) writes a PNG.
//!
//! Same pipeline the e2e snapshot tests use. Public so callers outside
//! the test target (binaries, examples, downstream crates) can take
//! deterministic screenshots at any resolution.
//!
//! # Why wgpu row-pitch alignment matters
//! `copy_texture_to_buffer` requires the buffer's row pitch to be a
//! multiple of `COPY_BYTES_PER_ROW_ALIGNMENT` (256 on every Vulkan /
//! Metal / D3D12 backend shipped today). For a non-aligned width we
//! allocate the aligned size and strip padding while reading back so
//! callers see a tight `width * 4` bytes-per-row buffer.

use std::path::Path;

use pollster::FutureExt;
use vello::peniko::Color;
use vello::util::RenderContext;
use vello::{AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, Scene};

/// An offscreen wgpu + vello pipeline sized at `width × height`. Reuse
/// a single instance for multiple renders to amortise adapter / device
/// creation (which is the expensive part).
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
    /// Spin up a new headless renderer at the given dimensions.
    ///
    /// Panics if no suitable adapter exists (e.g. llvmpipe isn't
    /// installed in the test container). Callers that want a soft-fail
    /// path should wrap this in `std::panic::catch_unwind`.
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

        // Padded buffer for the texture→buffer copy. Row pitch must be
        // a multiple of 256 (wgpu::COPY_BYTES_PER_ROW_ALIGNMENT).
        let bytes_per_pixel = 4u32;
        let unpadded = width * bytes_per_pixel;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = ((unpadded + align - 1) / align) * align;
        let readback_size = (padded_bytes_per_row * height) as u64;
        let readback_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("quick-ui-offscreen-readback"),
            size: readback_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let renderer = Renderer::new(
            device,
            RendererOptions {
                surface_format: None,
                use_cpu: false,
                antialiasing_support: AaSupport::all(),
                num_init_threads: None,
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

    /// Render `scene` with the given background colour and return the
    /// raw RGBA pixels, tight-packed at `4 * width` bytes per row.
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

        // Stage the copy texture → readback buffer.
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        encoder.copy_texture_to_buffer(
            wgpu::ImageCopyTexture {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::ImageCopyBuffer {
                buffer: &self.readback_buf,
                layout: wgpu::ImageDataLayout {
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

        // Map + read back. `map_async` is async but `poll(Wait)` drives
        // it to completion without an executor for our purposes.
        let slice = self.readback_buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            tx.send(r).ok();
        });
        device.poll(wgpu::Maintain::Wait);
        rx.recv()
            .expect("map_async channel")
            .expect("buffer map failed");

        let mapped = slice.get_mapped_range();
        let mut tight = Vec::with_capacity((self.width * self.height * 4) as usize);
        let row_bytes = (self.width * 4) as usize;
        for row in 0..self.height as usize {
            let start = row * self.padded_bytes_per_row as usize;
            tight.extend_from_slice(&mapped[start..start + row_bytes]);
        }
        drop(mapped);
        self.readback_buf.unmap();
        tight
    }

    /// Convenience: render `scene` and write the result to `path` as a
    /// PNG. Creates parent dirs if they don't exist.
    pub fn render_png<P: AsRef<Path>>(&mut self, scene: &Scene, bg: Color, path: P) {
        let rgba = self.render(scene, bg);
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let file = std::fs::File::create(path).expect("create png file");
        let mut w = std::io::BufWriter::new(file);
        let mut encoder = png::Encoder::new(&mut w, self.width, self.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("png header");
        writer.write_image_data(&rgba).expect("png write");
    }

    /// Sample the RGBA at `(x, y)` from a tight-packed buffer (4 *
    /// width bytes per row). Returns `(r, g, b, a)`.
    pub fn sample(rgba: &[u8], width: u32, x: u32, y: u32) -> (u8, u8, u8, u8) {
        let idx = ((y * width + x) * 4) as usize;
        (rgba[idx], rgba[idx + 1], rgba[idx + 2], rgba[idx + 3])
    }
}
