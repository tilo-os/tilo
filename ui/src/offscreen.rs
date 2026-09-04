//! Headless offscreen render harness: renders a Vello `Scene` into a GPU
//! texture with no window, reads the bytes back, optionally writes a PNG.
//!
//! `copy_texture_to_buffer` needs the buffer's row pitch aligned to
//! `COPY_BYTES_PER_ROW_ALIGNMENT` (256 today), so a non-aligned width is
//! padded and the padding stripped on readback.
//!
//! Two renders of the *same* `Scene` are not guaranteed bit-identical:
//! vello assigns path-segment order via `atomicAdd` (GPU scheduling), and
//! float addition isn't associative, so coverage on a quantisation
//! boundary can land ±1 in a channel. Measured here: ≤4/57 600 pixels,
//! always ±1, ink-count unchanged — a race between concurrent renders in
//! one process, not a single-frame artifact. Compare with
//! [`OffscreenRenderer::pixels_differing_beyond_aa`], not byte equality.
//!
//! Never cache one in a `thread_local!`: dropping wgpu resources touches
//! wgpu's own thread-local, and TLS destructors run in reverse
//! registration order, so a cache registered before the first wgpu call is
//! torn down after wgpu's — `SIGABRT`, killing the process. Use a
//! process-wide `static` instead (never drops); this type is `Send` to
//! allow that.

use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use crate::gpu::RenderContext;
use pollster::FutureExt;
use vello::kurbo::{Affine, Rect};
use vello::peniko::Color;
use vello::{AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, Scene};

/// Ensures only ONE `wgpu::Instance` per process — constructing several in
/// parallel (the default `cargo test` pattern) races inside
/// `vkCreateInstance` on at least radv + intel-iris and SIGSEGVs. `Arc` lets
/// `OffscreenRenderers` keep the context alive past process-stack frames;
/// `Mutex` because `RenderContext::device(...)` takes `&mut self`.
static SHARED_RENDER_CONTEXT: OnceLock<Arc<Mutex<RenderContext>>> = OnceLock::new();

/// A complex first frame can make Vello grow its bump reservation more than
/// once before it produces pixels. Four submissions covered the observed
/// two-growth case with one spare attempt, while remaining strictly bounded
/// for intentionally transparent scenes.
const RESILIENT_RENDER_ATTEMPTS: usize = 4;

struct DamageTexture {
    width: u32,
    height: u32,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OffscreenMemoryStats {
    pub output_target_bytes: u64,
    pub damage_target_bytes: u64,
    pub readback_bytes: u64,
    pub vello: vello::RendererMemoryStats,
}

impl OffscreenMemoryStats {
    pub fn known_gpu_bytes(self) -> u64 {
        self.output_target_bytes
            .saturating_add(self.damage_target_bytes)
            .saturating_add(self.vello.pooled_buffer_bytes)
            .saturating_add(self.vello.live_buffer_bytes)
            .saturating_add(self.vello.image_bytes)
    }
}

fn shared_render_context() -> Arc<Mutex<RenderContext>> {
    SHARED_RENDER_CONTEXT
        .get_or_init(|| Arc::new(Mutex::new(RenderContext::new())))
        .clone()
}

/// Handles to the process-wide shared wgpu device — the very same one
/// every [`OffscreenRenderer`] draws through.
///
/// Apps that run their own GPU work headlessly (the video editor
/// composites its timeline frames with a wgpu compute pipeline) must
/// borrow these rather than stand up a second `wgpu::Instance`: parallel
/// `vkCreateInstance` calls race and SIGSEGV on several Mesa drivers,
/// which is the whole reason the shared context exists.
///
/// Both handles are `Arc`-backed inside wgpu, so cloning them out of the
/// context is cheap and they stay valid once the lock is released.
pub struct SharedGpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

/// Borrow the shared device/queue, initialising the process-wide context
/// on first call. Fails with the same [`OffscreenError`]s as
/// [`OffscreenRenderer::try_new`] — a machine with no usable adapter
/// gets a `NoAdapter`, never a panic.
///
/// # Errors
///
/// Returns [`OffscreenError::NoAdapter`] when no usable GPU adapter exists, or
/// [`OffscreenError::ContextPoisoned`] when another thread panicked while
/// holding the shared render-context lock.
pub fn shared_gpu() -> Result<SharedGpu, OffscreenError> {
    let context = shared_render_context();
    let (device, queue) = {
        let mut ctx = context
            .lock()
            .map_err(|_| OffscreenError::ContextPoisoned)?;
        let dev_id = ctx
            .device(None)
            .block_on()
            .ok_or(OffscreenError::NoAdapter)?;
        let handle = ctx.devices.get(dev_id).ok_or(OffscreenError::NoAdapter)?;
        let device = handle.device.clone();
        let queue = handle.queue.clone();
        drop(ctx);
        (device, queue)
    };
    Ok(SharedGpu { device, queue })
}

/// Why an [`OffscreenRenderer`] could not be built.
///
/// Every variant is an *environment* failure. Bad arguments (a zero width
/// or height) stay an `assert!` in the constructors — that's a caller bug.
#[derive(Debug)]
pub enum OffscreenError {
    /// On a headless machine this usually means no software rasteriser is
    /// installed (llvmpipe / lavapipe).
    NoAdapter,
    Renderer(vello::Error),
    /// Only reachable under `panic = "unwind"`; the shipping release
    /// profile aborts instead of poisoning.
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
    /// Shared with every other `OffscreenRenderer` in the process — see
    /// [`SHARED_RENDER_CONTEXT`].
    context: Arc<Mutex<RenderContext>>,
    dev_id: usize,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// Allocated only for screenshot/pixel callers. GPU timing benchmarks
    /// never read the target back and therefore should not carry a second
    /// full-resolution host-visible allocation.
    readback_buf: Option<wgpu::Buffer>,
    padded_bytes_per_row: u32,
    renderer: Renderer,
    damage_textures: Vec<DamageTexture>,
    damage_scene: Scene,
    last_damage_signature: Option<(usize, u32, u32, u64)>,
    target_valid: bool,
}

impl OffscreenRenderer {
    pub fn memory_stats(&self) -> OffscreenMemoryStats {
        let rgba_bytes = |width: u32, height: u32| {
            u64::from(width)
                .saturating_mul(u64::from(height))
                .saturating_mul(4)
        };
        OffscreenMemoryStats {
            output_target_bytes: rgba_bytes(self.width, self.height),
            damage_target_bytes: self
                .damage_textures
                .iter()
                .map(|target| rgba_bytes(target.width, target.height))
                .sum(),
            readback_bytes: self.readback_buf.as_ref().map_or(0, wgpu::Buffer::size),
            vello: self.renderer.memory_stats(),
        }
    }

    /// Panics if no suitable adapter exists. **Do not try to soften that
    /// with `std::panic::catch_unwind`** — the shipping release profile
    /// sets `panic = "abort"`, under which a panic kills the process at the
    /// panic site and `catch_unwind` never sees it. Callers that need a
    /// soft-fail path must use [`try_new`](Self::try_new).
    ///
    /// # Panics
    ///
    /// Panics if [`try_new`](Self::try_new) returns an [`OffscreenError`] —
    /// see its variants for exactly when that happens.
    #[allow(
        clippy::panic,
        reason = "new() is the deliberate panicking convenience constructor paired with the fallible try_new(); removing the panic would defeat the reason both exist"
    )]
    pub fn new(width: u32, height: u32) -> Self {
        match Self::try_new(width, height) {
            Ok(renderer) => renderer,
            Err(e) => panic!("OffscreenRenderer::new: {e}"),
        }
    }

    /// For any code path that has to survive a GPU-less machine, since
    /// `catch_unwind` can't (see [`new`](Self::new)). Still asserts on a
    /// zero width or height — that's a caller bug, not an environment one.
    ///
    /// # Errors
    ///
    /// Returns [`OffscreenError::NoAdapter`] when no wgpu adapter is
    /// available (a headless machine typically needs llvmpipe/lavapipe
    /// installed), [`OffscreenError::Renderer`] when vello fails to build a
    /// renderer for the adapter, and [`OffscreenError::ContextPoisoned`]
    /// when the shared `RenderContext` mutex was poisoned by a panic on
    /// another thread.
    ///
    /// # Panics
    ///
    /// Panics if `width` or `height` is zero — that's a caller bug, not a
    /// recoverable environment failure.
    #[allow(
        clippy::panic_in_result_fn,
        reason = "the width/height assert is a caller-bug panic, not a recoverable failure — every real failure mode already has its own OffscreenError variant"
    )]
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
        let handle = ctx.devices.get(dev_id).ok_or(OffscreenError::NoAdapter)?;
        let device = &handle.device;

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
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        // See the row-pitch alignment note in the module docs.
        let bytes_per_pixel = 4u32;
        let unpadded = width.saturating_mul(bytes_per_pixel);
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = unpadded.div_ceil(align).saturating_mul(align);
        let renderer = Renderer::new(
            device,
            RendererOptions {
                use_cpu: std::env::var_os("QUICK_UI_VELLO_CPU").is_some(),
                // Only `Area` is ever asked for; `all()` would also build
                // the msaa8/msaa16 pipelines nothing selects.
                antialiasing_support: AaSupport::area_only(),
                num_init_threads: None,
                pipeline_cache: None,
            },
        )
        .map_err(OffscreenError::Renderer)?;

        drop(ctx);

        Ok(Self {
            width,
            height,
            context,
            dev_id,
            texture,
            view,
            readback_buf: None,
            padded_bytes_per_row,
            renderer,
            damage_textures: Vec::with_capacity(4),
            damage_scene: Scene::new(),
            last_damage_signature: None,
            target_valid: false,
        })
    }

    #[allow(
        clippy::expect_used,
        clippy::significant_drop_tightening,
        reason = "pixel-test readback cannot recover from a failed GPU mapping"
    )]
    fn read_target(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> Vec<u8> {
        let readback_buf = self.readback_buf.get_or_insert_with(|| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("quick-ui-offscreen-readback"),
                size: u64::from(self.padded_bytes_per_row.saturating_mul(self.height)),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            })
        });
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
                buffer: readback_buf,
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

        let slice = readback_buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            tx.send(result).ok();
        });
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        rx.recv()
            .expect("map_async channel")
            .expect("buffer map failed");

        let mapped = slice.get_mapped_range();
        let row_bytes = (self.width as usize).saturating_mul(4);
        let mut tight = Vec::with_capacity((self.height as usize).saturating_mul(row_bytes));
        for row in 0..self.height as usize {
            let start = row.saturating_mul(self.padded_bytes_per_row as usize);
            let end = start.saturating_add(row_bytes);
            let row_slice = mapped
                .get(start..end)
                .expect("readback row out of range of the mapped buffer");
            tight.extend_from_slice(row_slice);
        }
        drop(mapped);
        readback_buf.unmap();
        tight
    }

    /// Returns raw RGBA pixels, tight-packed at `4 * width` bytes per row
    /// (the texture format is `Rgba8Unorm`, so **R, G, B, A** order, not
    /// BGRA).
    ///
    /// # Panics
    ///
    /// Panics if the shared `RenderContext` mutex is poisoned, if this
    /// renderer's device entry has gone missing from it, if vello fails to
    /// render the scene, or if the GPU readback fails (the `map_async`
    /// channel drops or the mapping itself errors). Every one of these is
    /// an environment failure this headless test/screenshot harness has no
    /// meaningful fallback for — it fails loudly rather than handing back a
    /// silently-empty buffer a comparison could misread as a real frame.
    #[allow(
        clippy::expect_used,
        reason = "render() is the harness's core primitive, called only from tests/shots; every expect() here guards an environment failure (poisoned lock, missing device, GPU/readback error) with no recoverable fallback — matching render_png's existing panic-on-failure contract below"
    )]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "already as tight as the borrow allows: `handle` below borrows from \
                  `ctx`, so `ctx` can't drop until the clones are taken, and the block \
                  already ends (dropping it) right after that"
    )]
    fn render_with_overflow(&mut self, scene: &Scene, bg: Color) -> (Vec<u8>, bool) {
        // The lock only guards host-side DeviceHandle bookkeeping, not
        // rendering — vello's kernels run GPU-side. Clone the cheap,
        // Arc-backed `Device`/`Queue` handles out and drop the guard
        // immediately, rather than holding the shared context locked for
        // every other `OffscreenRenderer` through the whole render + readback.
        let (device, queue) = {
            let ctx = self.context.lock().expect("shared RenderContext poisoned");
            let handle = ctx
                .devices
                .get(self.dev_id)
                .expect("this renderer's device entry is missing from the shared RenderContext");
            (handle.device.clone(), handle.queue.clone())
        };

        self.renderer
            .render_to_texture(
                &device,
                &queue,
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
        self.target_valid = true;
        self.last_damage_signature = Some((
            1,
            self.width,
            self.height,
            u64::from(self.width).saturating_mul(u64::from(self.height)),
        ));

        let tight = self.read_target(&device, &queue);
        let overflowed = self.renderer.settle_bump_budget(&device);
        (tight, overflowed)
    }

    pub fn render(&mut self, scene: &Scene, bg: Color) -> Vec<u8> {
        self.render_with_overflow(scene, bg).0
    }

    /// Read the already-rendered retained target without submitting another
    /// Vello frame. Used to verify partial-damage composition pixel-for-pixel.
    ///
    /// # Panics
    ///
    /// Panics if the shared GPU context or target readback fails.
    #[allow(
        clippy::expect_used,
        clippy::significant_drop_tightening,
        reason = "pixel-test readback cannot recover from unavailable shared GPU state"
    )]
    pub fn readback_current(&mut self) -> Vec<u8> {
        let (device, queue) = {
            let ctx = self.context.lock().expect("shared RenderContext poisoned");
            let handle = ctx
                .devices
                .get(self.dev_id)
                .expect("this renderer's device entry is missing from the shared RenderContext");
            (handle.device.clone(), handle.queue.clone())
        };
        self.read_target(&device, &queue)
    }

    /// Submit a render and wait for its GPU work without copying the target
    /// back to CPU memory.
    ///
    /// Performance harnesses use this to measure Vello rather than PCIe/shared
    /// memory readback plus row repacking. Screenshot and visual-regression
    /// callers should continue to use [`Self::render`].
    ///
    /// # Panics
    /// If shared GPU state is poisoned/missing or Vello rejects the render.
    #[allow(
        clippy::expect_used,
        clippy::significant_drop_tightening,
        reason = "the benchmark renderer cannot produce a meaningful sample after a GPU submission failure"
    )]
    pub fn render_gpu_wait(&mut self, scene: &Scene, bg: Color) {
        let (device, queue) = {
            let ctx = self.context.lock().expect("shared RenderContext poisoned");
            let handle = ctx
                .devices
                .get(self.dev_id)
                .expect("this renderer's device entry is missing from the shared RenderContext");
            (handle.device.clone(), handle.queue.clone())
        };

        self.renderer
            .render_to_texture(
                &device,
                &queue,
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
        self.target_valid = true;
        self.last_damage_signature = Some((
            1,
            self.width,
            self.height,
            u64::from(self.width).saturating_mul(u64::from(self.height)),
        ));

        // `settle_bump_budget` waits for the submitted work and consumes the
        // tiny allocator-counter readback when Vello requested one. It never
        // reads the rendered target image back to the host.
        let _ = self.renderer.settle_bump_budget(&device);
    }

    /// Update one native-resolution rectangle in the retained target and
    /// wait for completion. Pixels outside `damage` are preserved exactly;
    /// no resizing or upscaling is involved.
    ///
    /// Returns the number of pixels actually rasterized. An invalid or
    /// full-size damage rectangle falls back to a complete target render.
    ///
    /// # Panics
    ///
    /// Panics if the shared GPU context is unavailable or Vello rejects the
    /// render. This is a benchmark/test primitive and cannot produce a valid
    /// timing sample after either failure.
    #[allow(
        clippy::expect_used,
        clippy::significant_drop_tightening,
        reason = "the benchmark renderer cannot produce a meaningful sample after a GPU submission failure"
    )]
    pub fn render_damage_gpu_wait(&mut self, scene: &Scene, bg: Color, damage: Rect) -> u64 {
        self.render_damage_regions_gpu_wait(scene, bg, std::slice::from_ref(&damage))
    }

    /// Batched form of [`Self::render_damage_gpu_wait`]. Disconnected regions
    /// remain separate, but all submissions are completed by one final wait.
    /// Damage covering at least half the output falls back to one full render
    /// to bound pass count and scratch-texture memory.
    ///
    /// # Panics
    ///
    /// Panics under the same conditions as [`Self::render_damage_gpu_wait`].
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::expect_used,
        clippy::significant_drop_tightening,
        clippy::too_many_lines,
        reason = "damage bounds are clamped to non-negative u32 target dimensions before narrowing; the benchmark renderer cannot produce a meaningful sample after a GPU submission failure"
    )]
    pub fn render_damage_regions_gpu_wait(
        &mut self,
        scene: &Scene,
        bg: Color,
        damage: &[Rect],
    ) -> u64 {
        let full_pixels = u64::from(self.width).saturating_mul(u64::from(self.height));
        let mut damage_pixels = 0u64;
        let mut scratch_width = 0u32;
        let mut scratch_height = 0u32;
        for rect in damage {
            let x0 = rect.x0.floor().max(0.0).min(f64::from(self.width)) as u32;
            let y0 = rect.y0.floor().max(0.0).min(f64::from(self.height)) as u32;
            let x1 = rect.x1.ceil().max(0.0).min(f64::from(self.width)) as u32;
            let y1 = rect.y1.ceil().max(0.0).min(f64::from(self.height)) as u32;
            let width = x1.saturating_sub(x0);
            let height = y1.saturating_sub(y0);
            damage_pixels =
                damage_pixels.saturating_add(u64::from(width).saturating_mul(u64::from(height)));
            scratch_width = scratch_width.max(width);
            scratch_height = scratch_height.max(height);
        }
        if !self.target_valid
            || damage_pixels == 0
            || damage_pixels >= full_pixels.saturating_div(2)
        {
            self.render_gpu_wait(scene, bg);
            return full_pixels;
        }

        let (device, queue) = {
            let ctx = self.context.lock().expect("shared RenderContext poisoned");
            let handle = ctx
                .devices
                .get(self.dev_id)
                .expect("this renderer's device entry is missing from the shared RenderContext");
            (handle.device.clone(), handle.queue.clone())
        };
        let signature = (damage.len(), scratch_width, scratch_height, damage_pixels);
        if self
            .last_damage_signature
            .is_some_and(|previous| previous != signature)
        {
            self.renderer.trim_pooled_buffers();
        }
        self.last_damage_signature = Some(signature);

        let mut target_index = 0usize;
        for rect in damage {
            let x0 = rect.x0.floor().max(0.0).min(f64::from(self.width)) as u32;
            let y0 = rect.y0.floor().max(0.0).min(f64::from(self.height)) as u32;
            let x1 = rect.x1.ceil().max(0.0).min(f64::from(self.width)) as u32;
            let y1 = rect.y1.ceil().max(0.0).min(f64::from(self.height)) as u32;
            let width = x1.saturating_sub(x0);
            let height = y1.saturating_sub(y0);
            if width == 0 || height == 0 {
                continue;
            }
            let recreate = self
                .damage_textures
                .get(target_index)
                .is_none_or(|target| target.width != width || target.height != height);
            if recreate {
                let texture = device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("quick-ui-native-damage-target"),
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
                let replacement = DamageTexture {
                    width,
                    height,
                    texture,
                    view,
                };
                if let Some(target) = self.damage_textures.get_mut(target_index) {
                    *target = replacement;
                } else {
                    self.damage_textures.push(replacement);
                }
            }
            let target = self
                .damage_textures
                .get(target_index)
                .expect("damage target was inserted above");
            self.damage_scene.reset();
            self.damage_scene.append(
                scene,
                Some(Affine::translate((-(f64::from(x0)), -(f64::from(y0))))),
            );
            self.renderer
                .render_to_texture(
                    &device,
                    &queue,
                    &self.damage_scene,
                    &target.view,
                    &RenderParams {
                        base_color: bg,
                        width,
                        height,
                        antialiasing_method: AaConfig::Area,
                    },
                )
                .expect("render_to_texture");

            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("quick-ui-copy-native-damage"),
            });
            encoder.copy_texture_to_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &target.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyTextureInfo {
                    texture: &self.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: x0, y: y0, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
            queue.submit([encoder.finish()]);
            target_index = target_index.saturating_add(1);
        }
        self.damage_textures.truncate(target_index);
        let _ = self.renderer.settle_bump_budget(&device);
        damage_pixels
    }

    /// [`render`](Self::render) with a transparent background.
    pub fn render_rgba(&mut self, scene: &Scene) -> Vec<u8> {
        self.render(scene, Color::TRANSPARENT)
    }

    /// Render until Vello confirms that no bump-allocated geometry was
    /// dropped, up to a strict attempt limit. This checks the renderer's GPU
    /// counters, so partially missing glyphs are caught as reliably as a
    /// completely transparent failed frame.
    pub fn render_resilient(&mut self, scene: &Scene, bg: Color) -> Vec<u8> {
        let (mut pixels, mut overflowed) = self.render_with_overflow(scene, bg);
        let mut grew_budget = overflowed;
        for _ in 1..RESILIENT_RENDER_ATTEMPTS {
            let unexpectedly_transparent = bg == Color::TRANSPARENT
                && !scene.encoding().is_empty()
                && pixels
                    .chunks_exact(4)
                    .all(|pixel| pixel.get(3).copied().unwrap_or(0) == 0);
            if !overflowed && !unexpectedly_transparent && !grew_budget {
                break;
            }
            // The first frame that fits after a growth can still be the one
            // that finishes populating glyph/image atlas state. Require one
            // additional confirmed-fit submission, then return immediately.
            let confirm_after_growth = grew_budget && !overflowed && !unexpectedly_transparent;
            (pixels, overflowed) = self.render_with_overflow(scene, bg);
            grew_budget = if confirm_after_growth {
                overflowed
            } else {
                grew_budget || overflowed
            };
        }
        pixels
    }

    /// Render a one-shot scene without accepting Vello's transparent
    /// bump-buffer warm-up frame. Interactive rendering naturally gets a next
    /// frame after the reservation grows; screenshots, first recording frames
    /// and deterministic visual snapshots do not, so they retry here.
    pub fn render_rgba_resilient(&mut self, scene: &Scene) -> Vec<u8> {
        self.render_resilient(scene, Color::TRANSPARENT)
    }

    /// Convenience: render `scene` and write the result to `path` as a
    /// PNG. Creates parent dirs if they don't exist.
    ///
    /// # Panics
    ///
    /// Panics if `path`'s file cannot be created or PNG encoding fails —
    /// both are dev/test-environment problems (disk full, bad permissions)
    /// with no recoverable action for this infallible, shots/test-only
    /// helper to take.
    #[allow(
        clippy::expect_used,
        reason = "render_png is a debug/test screenshot helper called as infallible from shots and test call sites across the workspace; a write/encode failure is a dev-environment problem worth crashing loudly on, not a Result this signature can propagate without a wide API change"
    )]
    pub fn render_png<P: AsRef<Path>>(&mut self, scene: &Scene, bg: Color, path: P) {
        let rgba = self.render_resilient(scene, bg);
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
    ///
    /// # Panics
    ///
    /// Panics if `(x, y)` is outside `rgba`. Every real call site is test
    /// code asserting against known-valid coordinates (see the crate's
    /// render tests), so an out-of-range sample is a test bug that should
    /// fail loudly rather than silently read back `0`.
    #[allow(
        clippy::indexing_slicing,
        reason = "sample() exists only for test pixel assertions; an out-of-range coordinate is a test bug that should panic, not silently return 0"
    )]
    pub fn sample(rgba: &[u8], width: u32, x: u32, y: u32) -> (u8, u8, u8, u8) {
        let idx = (y.saturating_mul(width).saturating_add(x)).saturating_mul(4) as usize;
        (
            rgba[idx],
            rgba[idx.saturating_add(1)],
            rgba[idx.saturating_add(2)],
            rgba[idx.saturating_add(3)],
        )
    }

    /// Tolerates the ±1 antialiasing wobble described in the module docs;
    /// anything above zero is a real difference. Buffers of different
    /// lengths count as differing everywhere they don't overlap.
    pub fn pixels_differing_beyond_aa(a: &[u8], b: &[u8]) -> usize {
        let overlapping = a
            .chunks_exact(4)
            .zip(b.chunks_exact(4))
            .filter(|(x, y)| x.iter().zip(y.iter()).any(|(p, q)| p.abs_diff(*q) > 1))
            .count();
        let unmatched = a.len().abs_diff(b.len()).div_ceil(4);
        overlapping.saturating_add(unmatched)
    }
}

#[cfg(test)]
mod tests {
    use super::OffscreenRenderer;
    use vello::Scene;
    use vello::kurbo::{Affine, Rect};
    use vello::peniko::{Color, Fill};

    /// A `static Mutex<Option<OffscreenRenderer>>` (see the module docs)
    /// only compiles while this holds, so a field that quietly gave up
    /// `Send` should fail here, where the reason is written down.
    #[test]
    fn offscreen_renderer_stays_send() {
        fn assert_send<T: Send>() {}
        assert_send::<OffscreenRenderer>();
    }

    /// Without this guard the tolerance could be widened later — turning
    /// "these are the same frame" into an assertion that passes on frames
    /// that are visibly different — with no test noticing.
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

    #[test]
    fn native_damage_matches_a_full_render_and_preserves_untouched_pixels() {
        const W: u32 = 96;
        const H: u32 = 64;
        let mut initial = Scene::new();
        initial.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgb8(24, 32, 48),
            None,
            &Rect::new(8.0, 8.0, 88.0, 56.0),
        );
        let mut updated = initial.clone();
        updated.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgb8(80, 220, 120),
            None,
            &Rect::new(28.0, 20.0, 68.0, 44.0),
        );

        let mut retained = OffscreenRenderer::new(W, H);
        let before = retained.render(&initial, Color::BLACK);
        assert_eq!(
            retained.render_damage_gpu_wait(
                &updated,
                Color::BLACK,
                Rect::new(26.0, 18.0, 70.0, 46.0),
            ),
            44 * 28,
        );
        let partial = retained.readback_current();

        let mut reference = OffscreenRenderer::new(W, H);
        let full = reference.render(&updated, Color::BLACK);
        assert_eq!(
            OffscreenRenderer::pixels_differing_beyond_aa(&partial, &full),
            0,
            "native damage composition must match a complete frame",
        );
        assert_eq!(
            OffscreenRenderer::sample(&before, W, 12, 12),
            OffscreenRenderer::sample(&partial, W, 12, 12),
            "pixels outside damage stay byte-identical",
        );
    }
}
