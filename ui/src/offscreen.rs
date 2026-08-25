//! Headless offscreen render harness. Renders a Vello `Scene` into a
//! GPU texture without a window/surface, copies the bytes back to the
//! CPU, and (optionally) writes a PNG.
//!
//! Same pipeline the e2e snapshot tests use. Public so callers outside
//! the test target (binaries, examples, downstream crates) can take
//! screenshots at any resolution, off any scene, with no display
//! attached. What is drawn is a pure function of the scene; the last bit
//! of the rasterisation is not — see the last section.
//!
//! # Why wgpu row-pitch alignment matters
//! `copy_texture_to_buffer` requires the buffer's row pitch to be a
//! multiple of `COPY_BYTES_PER_ROW_ALIGNMENT` (256 on every Vulkan /
//! Metal / D3D12 backend shipped today). For a non-aligned width we
//! allocate the aligned size and strip padding while reading back so
//! callers see a tight `width * 4` bytes-per-row buffer.
//!
//! # Two rasterisations of one scene are not guaranteed bit-identical
//! Rendering the *same* [`Scene`] twice can return buffers that differ
//! by one 8-bit step on a handful of antialiased edge pixels. This is a
//! property of vello's GPU pipeline, not of this harness:
//!
//! * `path_count` assigns each path segment its slot inside a tile's
//!   segment slice with an `atomicAdd`, so the order segments land in
//!   for a given tile follows GPU workgroup scheduling;
//! * `fine`'s analytic area antialiasing then accumulates
//!   `area += a * dy` over that slice, and float addition is not
//!   associative — a different order can move the coverage by one ulp;
//! * a pixel whose coverage sits on a quantisation boundary lands one
//!   step either side of it.
//!
//! Measured on this stack: identical `Scene`, ≤4 pixels out of 57 600
//! affected, always ±1 in a single channel, and the count of pixels
//! carrying ink never changes. It shows up when several threads in one
//! process drive the shared device (a fully serial process rendered the
//! same scene 600 times with no drift; three *separate* processes
//! rendering concurrently likewise had none), which is why the
//! screenshot suites mostly do not see it and the parallel test
//! binaries do.
//!
//! # Never cache one in a `thread_local!`
//! An `OffscreenRenderer` must not be dropped from a thread-local
//! destructor. Dropping wgpu resources calls
//! `wgpu_core::snatch::SnatchLock::read`, which under `debug_assertions`
//! reads wgpu's own `SNATCH_LOCK_TRACE` thread-local. TLS destructors run
//! in reverse registration order, so a cache registered before the first
//! wgpu call is torn down *after* wgpu's trace TLS — and the drop then
//! panics with "cannot access a Thread Local Storage value during or
//! after destruction". A panic in a TLS destructor is `SIGABRT`, not a
//! test failure: it kills the process and every result it had left to
//! report. A local variable is fine (it drops mid-execution); a
//! process-wide `static`, which never drops at all, is the way to cache
//! one across calls. `OffscreenRenderer` is `Send` so that this is
//! possible — see `offscreen_renderer_stays_send`.
//!
//! Consequences: byte-comparing two renders is not a sound equality
//! test. Compare with [`OffscreenRenderer::pixels_differing_beyond_aa`],
//! or compare the `Scene` the draw pass produced, which *is* exact. A
//! single frame is unaffected — the wobble is invisible (1/255 on an
//! edge pixel) and never changes what is drawn.

use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use pollster::FutureExt;
use vello::peniko::Color;
use crate::gpu::RenderContext;
use vello::{AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, Scene};

/// Process-wide shared `RenderContext` so we only ever construct ONE
/// `wgpu::Instance` per process. Creating multiple Instances in
/// parallel (the default `cargo test` pattern when many test cases
/// build OffscreenRenderers concurrently) races inside vkCreateInstance
/// on at least the radv + intel-iris Mesa drivers and SIGSEGVs. The
/// shared context dodges that entirely: every OffscreenRenderer locks
/// the same context briefly, reserves a DeviceHandle (which is cached
/// internally by vello), and proceeds.
///
/// `OnceLock<Arc<Mutex<RenderContext>>>` because: `OnceLock` is the
/// stdlib lazy-init primitive; `Arc` lets multiple OffscreenRenderers
/// keep the context alive past process-stack frames; `Mutex` is needed
/// because `RenderContext::device(...)` takes `&mut self` for the
/// internal device-list mutation.
static SHARED_RENDER_CONTEXT: OnceLock<Arc<Mutex<RenderContext>>> = OnceLock::new();

fn shared_render_context() -> Arc<Mutex<RenderContext>> {
    SHARED_RENDER_CONTEXT
        .get_or_init(|| Arc::new(Mutex::new(RenderContext::new())))
        .clone()
}

/// Why an [`OffscreenRenderer`] could not be built.
///
/// Every variant is an *environment* failure: the machine has no usable
/// GPU adapter, or the driver rejected vello's pipelines. Bad arguments
/// (a zero width or height) stay an `assert!` in the constructors —
/// that is a caller bug, not something the environment can hand you.
#[derive(Debug)]
pub enum OffscreenError {
    /// wgpu enumerated no adapter this process can use. On a headless
    /// machine that usually means no software rasteriser is installed
    /// (llvmpipe / lavapipe).
    NoAdapter,
    /// The adapter exists but vello could not build its compute
    /// pipelines against the resulting device.
    Renderer(vello::Error),
    /// A thread panicked while holding the process-wide
    /// `RenderContext`. Only reachable under `panic = "unwind"`; the
    /// shipping release profile aborts instead of poisoning.
    ContextPoisoned,
}

impl std::fmt::Display for OffscreenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoAdapter => f.write_str("no wgpu adapter available — is llvmpipe installed?"),
            Self::Renderer(e) => write!(f, "vello could not build a renderer: {e}"),
            Self::ContextPoisoned => f.write_str("the shared wgpu RenderContext is poisoned"),
        }
    }
}

impl std::error::Error for OffscreenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Renderer(e) => Some(e),
            Self::NoAdapter | Self::ContextPoisoned => None,
        }
    }
}

/// An offscreen wgpu + vello pipeline sized at `width × height`. Reuse
/// a single instance for multiple renders to amortise adapter / device
/// creation (which is the expensive part).
pub struct OffscreenRenderer {
    pub width: u32,
    pub height: u32,
    /// Shared with every other OffscreenRenderer in the process — see
    /// the module-level docs above for why this exists.
    context: Arc<Mutex<RenderContext>>,
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
    /// installed in the test container). **Do not try to soften that
    /// with `std::panic::catch_unwind`** — the shipping release profile
    /// sets `panic = "abort"` (workspace `Cargo.toml`), under which a
    /// panic kills the process at the panic site and `catch_unwind`
    /// never sees it. Callers that need a soft-fail path must use
    /// [`try_new`](Self::try_new), which returns the failure instead of
    /// panicking on it.
    ///
    /// `new` stays panicking because most callers are tests and example
    /// binaries for which "no GPU, no test" is the right outcome.
    pub fn new(width: u32, height: u32) -> Self {
        match Self::try_new(width, height) {
            Ok(renderer) => renderer,
            Err(e) => panic!("OffscreenRenderer::new: {e}"),
        }
    }

    /// Fallible [`new`](Self::new): returns [`OffscreenError`] rather
    /// than panicking when the machine has no usable wgpu adapter or
    /// the driver rejects vello's pipelines.
    ///
    /// This is the constructor for any code path that has to survive a
    /// GPU-less machine — the compositor's startup warmup, the headless
    /// backend, the test-control snapshot writer. Those cannot rely on
    /// `catch_unwind`: release builds are `panic = "abort"`.
    ///
    /// Still asserts on a zero width or height. A 0×0 render target is
    /// a caller bug, and turning programmer errors into `Result`s only
    /// moves them somewhere quieter.
    pub fn try_new(width: u32, height: u32) -> Result<Self, OffscreenError> {
        assert!(width > 0 && height > 0, "width and height must be > 0");

        let context = shared_render_context();
        let mut ctx = context
            .lock()
            .map_err(|_| OffscreenError::ContextPoisoned)?;
        let dev_id = ctx
            .device(None)
            .block_on()
            .ok_or(OffscreenError::NoAdapter)?;
        let device = &ctx.devices[dev_id].device;

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
                use_cpu: false,
                antialiasing_support: AaSupport::all(),
                num_init_threads: None,
                pipeline_cache: None,
            },
        )
        .map_err(OffscreenError::Renderer)?;

        // Drop the lock before returning so subsequent OffscreenRenderer
        // constructions don't have to wait on it.
        drop(ctx);

        Ok(Self {
            width,
            height,
            context,
            dev_id,
            texture,
            view,
            readback_buf,
            padded_bytes_per_row,
            renderer,
        })
    }

    /// Render `scene` with the given background colour and return the
    /// raw RGBA pixels, tight-packed at `4 * width` bytes per row.
    ///
    /// The underlying texture format is `Rgba8Unorm`, so the returned
    /// bytes are in **R, G, B, A** order (not BGRA). Callers that hand
    /// the buffer to an H.264 encoder (which also expects RGBA) can pass
    /// it through unmodified.
    pub fn render(&mut self, scene: &Scene, bg: Color) -> Vec<u8> {
        // Lock the shared context for the duration of this render.
        // wgpu internally serializes command submission on a single
        // device anyway, and the hot path (vello's render kernels) runs
        // GPU-side — the lock guards the host-side Vec<DeviceHandle>
        // indexing, not the rendering itself.
        let ctx = self.context.lock().expect("shared RenderContext poisoned");
        let device = &ctx.devices[self.dev_id].device;
        let queue = &ctx.devices[self.dev_id].queue;

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

        // Map + read back. `map_async` is async but `poll(Wait)` drives
        // it to completion without an executor for our purposes.
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

    /// Explicit alias for [`render`](Self::render) with a transparent
    /// background — the canonical entry-point for code paths that need
    /// raw RGBA frames (e.g. the `shots` binary's recording mode feeding
    /// the H.264 encoder).
    ///
    /// Kept as a thin wrapper rather than a rename so callers in the
    /// e2e suite that depend on `render` (composing custom backgrounds)
    /// continue to work unchanged.
    pub fn render_rgba(&mut self, scene: &Scene) -> Vec<u8> {
        self.render(scene, Color::TRANSPARENT)
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

    /// Count the pixels where `a` and `b` differ by more than the
    /// antialiasing wobble the GPU rasteriser is allowed (see the module
    /// docs: `atomicAdd`-ordered coverage accumulation can move an edge
    /// pixel one 8-bit step). Zero means the two buffers are the same
    /// image; anything above zero is a real difference in what was
    /// drawn, because a genuine visual change moves pixels by far more
    /// than one step.
    ///
    /// Buffers of different lengths count as differing everywhere they
    /// do not overlap, so a size mismatch can never read as a match.
    pub fn pixels_differing_beyond_aa(a: &[u8], b: &[u8]) -> usize {
        let overlapping = a
            .chunks_exact(4)
            .zip(b.chunks_exact(4))
            .filter(|(x, y)| x.iter().zip(y.iter()).any(|(p, q)| p.abs_diff(*q) > 1))
            .count();
        let unmatched = a.len().abs_diff(b.len()).div_ceil(4);
        overlapping + unmatched
    }
}

#[cfg(test)]
mod tests {
    use super::OffscreenRenderer;

    /// `OffscreenRenderer` has to stay `Send`.
    ///
    /// The compositor's snapshot path caches one in a process-wide
    /// `static` precisely so it is never dropped from a thread-local
    /// destructor — which aborts the process (see the module docs). A
    /// `static Mutex<Option<OffscreenRenderer>>` only compiles while
    /// this holds, so a field that quietly gives up `Send` would break
    /// that caller rather than this crate. Fail here, where the reason
    /// is written down.
    #[test]
    fn offscreen_renderer_stays_send() {
        fn assert_send<T: Send>() {}
        assert_send::<OffscreenRenderer>();
    }

    /// The frame comparison tolerates exactly the one-step antialiasing
    /// wobble the rasteriser is allowed and nothing wider. Without this
    /// guard the tolerance could be widened later — turning "these are
    /// the same frame" into an assertion that passes on frames that are
    /// visibly different — with no test noticing.
    #[test]
    fn frame_comparison_tolerates_one_step_and_no_more() {
        let base = [10u8, 20, 30, 255, 40, 50, 60, 255];

        assert_eq!(
            OffscreenRenderer::pixels_differing_beyond_aa(&base, &base),
            0,
            "identical buffers must not report a difference"
        );

        // Every channel off by one, in both directions: still one frame.
        let wobbled = [11u8, 19, 31, 254, 39, 51, 59, 255];
        assert_eq!(
            OffscreenRenderer::pixels_differing_beyond_aa(&base, &wobbled),
            0,
            "a one-step antialiasing wobble must read as the same frame"
        );

        // Two steps on a single channel of a single pixel is a real
        // difference.
        let two_steps = [12u8, 20, 30, 255, 40, 50, 60, 255];
        assert_eq!(
            OffscreenRenderer::pixels_differing_beyond_aa(&base, &two_steps),
            1,
            "a two-step difference must be reported"
        );

        // A truncated buffer is never a match, however similar its
        // overlapping prefix is.
        assert_eq!(
            OffscreenRenderer::pixels_differing_beyond_aa(&base, &base[..4]),
            1,
            "a size mismatch must count the pixels that have no counterpart"
        );
    }
}
