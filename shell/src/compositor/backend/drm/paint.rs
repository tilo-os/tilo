//! CPU-fallback vello → RGBA bridge for the DRM backend.
//!
//! Vello renders into a wgpu texture; smithay's DRM path drives GLES. Zero-copy would need
//! `wgpu::Texture` → DMA-BUF fd → `GlesRenderer::import_dmabuf` → page-flip, which on the
//! Vulkan wgpu backend needs `hal_texture` → `vulkan::Image` → external memory fd — a
//! multi-week, Vulkan-only build. Instead this renders to an offscreen wgpu texture, copies
//! bytes back to a CPU buffer, and exposes tight-packed `&[u8]` for the DRM upload side.
//!
//! `wgpu::copy_texture_to_buffer` requires `bytes_per_row` to be a multiple of
//! `COPY_BYTES_PER_ROW_ALIGNMENT` (256); for unaligned tight strides we allocate a padded
//! readback buffer and strip the padding on the way into the public `readback`.
//!
//! Tests need a wgpu device (`llvmpipe`/`vulkan_lvp` inside the distrobox); no real DRM device is touched.

use std::sync::atomic::{AtomicBool, Ordering};

use pollster::FutureExt;
use vello::peniko::Color;
// `ui::gpu`'s context, not `vello::util`'s: this device is shared with every other client on the machine, so it needs the same memory hint — see the header of `ui/src/gpu.rs`.
use ui::gpu::RenderContext;
use vello::{AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, Scene, wgpu};

/// Errors the bridge can return; each variant maps to an operator-visible message and the DRM backend logs+skips the frame.
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error(
        "failed to acquire a wgpu adapter for DRM CPU-fallback paint (is llvmpipe installed?): {0}"
    )]
    NoAdapter(String),

    #[error("failed to construct vello renderer: {0}")]
    Renderer(String),

    #[error("vello render_to_texture failed: {0}")]
    Render(String),

    #[error("wgpu buffer mapping failed: {0}")]
    Map(String),

    #[error("invalid target size: {0}")]
    InvalidSize(String),

    #[error("invalid internal device handle: {0}")]
    InvalidDevice(String),
}

/// One-shot latch so the "CPU fallback active" warning fires once per process; shared across all bridges since the message is process-global.
static WARNED_CPU_FALLBACK: AtomicBool = AtomicBool::new(false);

/// CPU-fallback bridge between a vello `Scene` and an RGBA byte buffer. Owns its wgpu device +
/// offscreen texture + readback buffer so callers can reuse it across frames. Single-output by design — multi-output is one bridge per output.
pub struct VelloDrmBridge {
    context: RenderContext,
    dev_id: usize,
    /// The vello renderer parented to the same wgpu device as `texture`, cached across frames so we don't pay the pipeline-compile cost on every paint.
    renderer: Renderer,
    /// Offscreen render target. Recreated on resize.
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// Readback buffer (padded to the wgpu row alignment requirement).
    readback_buf: wgpu::Buffer,
    /// Bytes-per-row in the padded readback buffer; may exceed `width * 4` when the tight stride doesn't divide 256.
    padded_bytes_per_row: u32,
    /// Tight (no padding) RGBA buffer the public `paint` returns.
    readback: Vec<u8>,
    /// Current size; resize triggers reallocation of `texture` + `readback_buf`.
    size: (u32, u32),
    /// Base color used to clear when the caller's scene is empty; matches the winit backend's fallback so the host window doesn't flash white.
    base_color: Color,
}

impl VelloDrmBridge {
    /// Build a bridge sized at `width × height`.
    ///
    /// # Errors
    /// Returns [`BridgeError::InvalidSize`] if `width` or `height` is `0` — DRM connector
    /// enumeration is expected to guarantee non-zero modes, but a driver could still hand back a
    /// degenerate one, and a panic here would take down the whole compositor session (`shell`
    /// runs with `panic = "abort"`). Returns [`BridgeError::NoAdapter`] if no wgpu adapter is
    /// available, [`BridgeError::InvalidDevice`] if the freshly acquired device id is out of
    /// range (defensive; cannot happen in practice), or [`BridgeError::Renderer`] if the vello
    /// renderer fails to build.
    pub fn new(width: u32, height: u32) -> Result<Self, BridgeError> {
        if width == 0 || height == 0 {
            return Err(BridgeError::InvalidSize(format!(
                "VelloDrmBridge::new: width/height must be > 0, got {width}x{height}"
            )));
        }

        let mut context = RenderContext::new();
        let dev_id = context.device(None).block_on().ok_or_else(|| {
            BridgeError::NoAdapter(
                "no wgpu adapter (install vulkan/llvmpipe drivers in the runtime)".to_owned(),
            )
        })?;
        let dev_handle = context.devices.get(dev_id).ok_or_else(|| {
            BridgeError::InvalidDevice(format!("wgpu device id {dev_id} out of range"))
        })?;
        let device = &dev_handle.device;

        let (texture, view, readback_buf, padded_bytes_per_row) =
            Self::allocate_targets(device, width, height);

        let renderer = Renderer::new(
            device,
            RendererOptions {
                use_cpu: false,
                antialiasing_support: AaSupport::all(),
                num_init_threads: None,
                pipeline_cache: None,
            },
        )
        .map_err(|e| BridgeError::Renderer(format!("{e:?}")))?;

        Ok(Self {
            context,
            dev_id,
            renderer,
            texture,
            view,
            readback_buf,
            padded_bytes_per_row,
            readback: Vec::new(),
            size: (width, height),
            base_color: Color::from_rgb8(0x1A, 0x1B, 0x26),
        })
    }

    /// Override the per-frame clear color; matches the winit backend API so callers can swap themes uniformly.
    pub fn set_base_color(&mut self, color: Color) {
        self.base_color = color;
    }

    /// Render `scene` into the offscreen texture and return a slice of the tight-packed RGBA bytes
    /// (`width * height * 4`). The slice's lifetime is tied to `&mut self`; callers must clone bytes out before the next `paint`.
    ///
    /// # Errors
    /// Returns a [`BridgeError`] if the vello render, the texture→buffer copy, or the readback
    /// buffer mapping fails. A readback row that doesn't fit the mapped buffer (shouldn't happen
    /// given `allocate_targets`, but guarded rather than trusted) is logged and truncates the
    /// returned buffer instead of panicking.
    pub fn paint(&mut self, scene: &Scene) -> Result<&[u8], BridgeError> {
        if !WARNED_CPU_FALLBACK.swap(true, Ordering::AcqRel) {
            tracing::warn!(
                "DRM paint: CPU fallback active — vello → wgpu readback → RGBA bytes; \
                 zero-copy DMA-BUF interop is the Phase 5.3 follow-up. \
                 Expect ~10ms+ per frame for a 1080p output."
            );
        }

        let (width, height) = self.size;
        self.render_to_readback(scene)?;

        // Block-map the readback buffer and copy into the tight `self.readback`, stripping row padding.
        let mapped = self.map_readback()?;
        let tight_row = (width as usize).saturating_mul(4);
        let needed = tight_row.saturating_mul(height as usize);
        self.readback.clear();
        self.readback.reserve(needed);
        for row in 0..height as usize {
            let start = row.saturating_mul(self.padded_bytes_per_row as usize);
            let end = start.saturating_add(tight_row);
            let Some(src_row) = mapped.get(start..end) else {
                tracing::warn!(
                    "DRM paint: readback row {row} out of the mapped buffer's bounds; truncating frame"
                );
                break;
            };
            self.readback.extend_from_slice(src_row);
        }
        drop(mapped);
        self.readback_buf.unmap();

        Ok(&self.readback)
    }

    /// Render `scene` and write it **directly** into an XRGB8888 destination (little-endian
    /// `[B, G, R, X]`), fusing the wgpu padding-strip and the RGBA→XRGB swizzle into one pass.
    ///
    /// The copy-eliminating sibling of [`paint`](Self::paint): `paint()` produces a tight RGBA
    /// `Vec` that the DRM scanout then swizzles into the dumb buffer (two full-frame copies after
    /// GPU readback); this swizzles the mapped wgpu readback straight into the caller's destination
    /// (a DRM dumb-buffer mmap), leaving only the unavoidable GPU→CPU readback plus one swizzle.
    ///
    /// `dst_pitch` may exceed `width * 4` for hardware pitch alignment; rows/columns are clamped to what `dst` can hold, so a short destination is logged-and-truncated rather than panicking.
    ///
    /// # Errors
    /// Returns a [`BridgeError`] under the same conditions as [`paint`](Self::paint).
    pub fn paint_into_xrgb8888(
        &mut self,
        scene: &Scene,
        dst: &mut [u8],
        dst_pitch: usize,
    ) -> Result<(), BridgeError> {
        if !WARNED_CPU_FALLBACK.swap(true, Ordering::AcqRel) {
            tracing::warn!(
                "DRM paint: CPU fallback active — vello → wgpu readback → XRGB8888 \
                 (fused swizzle, one copy eliminated vs. the tight-RGBA path); \
                 zero-copy DMA-BUF interop remains a follow-up. \
                 Expect ~several ms per frame for a 1080p output."
            );
        }

        let (width, height) = self.size;
        self.render_to_readback(scene)?;
        let mapped = self.map_readback()?;

        let tight_row = (width as usize).saturating_mul(4);
        if dst_pitch == 0 {
            self.readback_buf.unmap();
            return Ok(());
        }
        let max_rows = dst
            .len()
            .checked_div(dst_pitch)
            .unwrap_or(0)
            .min(height as usize);
        let cols = (dst_pitch / 4).min(width as usize);
        for row in 0..max_rows {
            let s_start = row.saturating_mul(self.padded_bytes_per_row as usize);
            let s_end = s_start.saturating_add(tight_row);
            let Some(srow) = mapped.get(s_start..s_end) else {
                tracing::warn!(
                    "DRM paint: readback row {row} out of the mapped buffer's bounds; stopping swizzle early"
                );
                break;
            };
            let d_start = row.saturating_mul(dst_pitch);
            let d_end = d_start.saturating_add(cols.saturating_mul(4));
            let Some(drow) = dst.get_mut(d_start..d_end) else {
                break;
            };
            for (d, s) in drow.chunks_exact_mut(4).zip(srow.chunks_exact(4)) {
                if let ([db, dg, dr, dx], [sr, sg, sb, _sa]) = (d, s) {
                    *db = *sb;
                    *dg = *sg;
                    *dr = *sr;
                    *dx = 0xFF;
                }
            }
        }
        drop(mapped);
        self.readback_buf.unmap();
        Ok(())
    }

    /// Render `scene` into the offscreen texture and stage the texture→`readback_buf` copy (padded
    /// to wgpu's row alignment); shared prologue for [`paint`](Self::paint) and [`paint_into_xrgb8888`](Self::paint_into_xrgb8888).
    fn render_to_readback(&mut self, scene: &Scene) -> Result<(), BridgeError> {
        let (width, height) = self.size;
        let dev_handle = self.context.devices.get(self.dev_id).ok_or_else(|| {
            BridgeError::InvalidDevice(format!("wgpu device id {} out of range", self.dev_id))
        })?;
        let device = &dev_handle.device;
        let queue = &dev_handle.queue;

        self.renderer
            .render_to_texture(
                device,
                queue,
                scene,
                &self.view,
                &RenderParams {
                    base_color: self.base_color,
                    width,
                    height,
                    antialiasing_method: AaConfig::Area,
                },
            )
            .map_err(|e| BridgeError::Render(format!("{e:?}")))?;

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
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(std::iter::once(encoder.finish()));
        Ok(())
    }

    /// Map the staged `readback_buf` for CPU reads, blocking until the GPU copy completes. The returned view borrows `readback_buf`; the caller must drop it then call `readback_buf.unmap()`.
    fn map_readback(&self) -> Result<wgpu::BufferView, BridgeError> {
        let dev_handle = self.context.devices.get(self.dev_id).ok_or_else(|| {
            BridgeError::InvalidDevice(format!("wgpu device id {} out of range", self.dev_id))
        })?;
        let device = &dev_handle.device;
        let slice = self.readback_buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            tx.send(r).ok();
        });
        // Bounded, not indefinite: a fence in flight across a system suspend/resume can never signal,
        // and an unbounded wait would wedge the paint path forever — the same freeze the winit
        // presenter's post-present poll had (see `backend/winit.rs`). A timeout drops this frame through the existing error path; the next one retries.
        let waited = device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(std::time::Duration::from_secs(1)),
        });
        if let Err(e) = waited {
            return Err(BridgeError::Map(format!("fence wait: {e:?}")));
        }
        rx.recv_timeout(std::time::Duration::from_secs(1))
            .map_err(|e| BridgeError::Map(format!("map_async channel: {e}")))?
            .map_err(|e| BridgeError::Map(format!("{e:?}")))?;
        Ok(slice.get_mapped_range())
    }

    /// Resize the offscreen target, reallocating texture + readback buffer; returns `Ok(())` even when the new size matches the current (no-op).
    ///
    /// # Errors
    /// Returns [`BridgeError::InvalidSize`] if `width` or `height` is `0`, or
    /// [`BridgeError::InvalidDevice`] if the bridge's device id is out of range (defensive;
    /// cannot happen in practice).
    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), BridgeError> {
        if width == 0 || height == 0 {
            return Err(BridgeError::InvalidSize(format!(
                "VelloDrmBridge::resize: width/height must be > 0, got {width}x{height}"
            )));
        }
        if (width, height) == self.size {
            return Ok(());
        }
        let dev_handle = self.context.devices.get(self.dev_id).ok_or_else(|| {
            BridgeError::InvalidDevice(format!("wgpu device id {} out of range", self.dev_id))
        })?;
        let device = &dev_handle.device;
        let (texture, view, readback_buf, padded_bytes_per_row) =
            Self::allocate_targets(device, width, height);
        self.texture = texture;
        self.view = view;
        self.readback_buf = readback_buf;
        self.padded_bytes_per_row = padded_bytes_per_row;
        self.size = (width, height);
        // Reset the tight buffer for the next `paint`; the new size may be larger, so clearing is cleaner than keeping capacity.
        self.readback.clear();
        Ok(())
    }

    /// Current (width, height); useful in tests and for comparing against the connector's mode size before resizing.
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Allocate (texture, view, `readback_buf`, `padded_bytes_per_row`) for the given size; shared by `new` and `resize`.
    fn allocate_targets(
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) -> (wgpu::Texture, wgpu::TextureView, wgpu::Buffer, u32) {
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("quick-desktop-drm-paint-target"),
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

        let bytes_per_pixel = 4u32;
        let unpadded = width.saturating_mul(bytes_per_pixel);
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = unpadded.div_ceil(align).saturating_mul(align);
        let readback_size = u64::from(padded_bytes_per_row).saturating_mul(u64::from(height));
        let readback_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("quick-desktop-drm-paint-readback"),
            size: readback_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        (texture, view, readback_buf, padded_bytes_per_row)
    }
}

/// The source half of a sub-rectangle copy/swizzle: the combined-scene RGBA buffer, its full
/// width/height, and the sub-rect's top-left origin within it. The three free functions below
/// share this shape; grouping it keeps each function under clippy's argument-count limit.
#[derive(Clone, Copy)]
pub struct SubrectSource<'a> {
    pub src_rgba: &'a [u8],
    pub src_w: usize,
    pub src_h: usize,
    pub src_x: usize,
    pub src_y: usize,
}

/// Swizzle a sub-rectangle of a tight-packed RGBA source buffer (`[R, G, B, A]`, row stride
/// `src_w * 4`) into an XRGB8888 destination (`[B, G, R, X]`, row stride `dst_pitch`).
///
/// The per-output present primitive for multi-monitor: the compositor renders one scene
/// spanning the combined output geometry into `source.src_rgba` (via [`VelloDrmBridge::paint`]),
/// then each output presents the sub-rect at its `Output.position`/`size` into its own scanout
/// dumb buffer. `source.src_x`/`source.src_y` is the sub-rect's top-left in the source; `out_w`×`out_h`
/// is the output's size.
///
/// Rows/cols outside the source or destination bounds are skipped (clamped, not panicking). For
/// a single output at the origin matching the combined geometry, this is byte-for-byte [`VelloDrmBridge::paint_into_xrgb8888`].
pub fn swizzle_subrect_rgba_to_xrgb8888(
    source: SubrectSource<'_>,
    dst: &mut [u8],
    dst_pitch: usize,
    out_w: usize,
    out_h: usize,
) {
    swizzle_subrect_rgba_to_xrgb8888_transformed(
        source,
        dst,
        dst_pitch,
        out_w,
        out_h,
        settings_store::hardware::OutputTransform::Normal,
    );
}

/// Rotation-aware sibling of [`swizzle_subrect_rgba_to_xrgb8888`]. Source
/// coordinates are in the post-transform logical plane; destination pixels
/// are in the physical KMS mode. This makes the scene, cursor, touch mapping,
/// and `wl_output` transform describe one geometry.
pub fn swizzle_subrect_rgba_to_xrgb8888_transformed(
    source: SubrectSource<'_>,
    dst: &mut [u8],
    dst_pitch: usize,
    physical_w: usize,
    physical_h: usize,
    transform: settings_store::hardware::OutputTransform,
) {
    let SubrectSource {
        src_rgba,
        src_w,
        src_h,
        src_x,
        src_y,
    } = source;
    if dst_pitch == 0 {
        return;
    }
    let max_rows = dst
        .len()
        .checked_div(dst_pitch)
        .unwrap_or(0)
        .min(physical_h);
    let cols = (dst_pitch / 4).min(physical_w);
    let (logical_w, logical_h) = match transform {
        settings_store::hardware::OutputTransform::Rotate90
        | settings_store::hardware::OutputTransform::Rotate270 => (physical_h, physical_w),
        _ => (physical_w, physical_h),
    };
    let logical = |col: usize, row: usize| match transform {
        settings_store::hardware::OutputTransform::Normal => (col, row),
        settings_store::hardware::OutputTransform::Rotate90 => {
            (row, logical_h.saturating_sub(1).saturating_sub(col))
        }
        settings_store::hardware::OutputTransform::Rotate180 => (
            logical_w.saturating_sub(1).saturating_sub(col),
            logical_h.saturating_sub(1).saturating_sub(row),
        ),
        settings_store::hardware::OutputTransform::Rotate270 => {
            (logical_w.saturating_sub(1).saturating_sub(row), col)
        }
    };
    for row in 0..max_rows {
        let (_, logical_y) = logical(0, row);
        let sy_base = src_y.saturating_add(logical_y);
        if sy_base >= src_h
            && matches!(transform, settings_store::hardware::OutputTransform::Normal)
        {
            break;
        }
        let d_start = row.saturating_mul(dst_pitch);
        let d_end = d_start.saturating_add(cols.saturating_mul(4));
        let Some(drow) = dst.get_mut(d_start..d_end) else {
            break;
        };
        for (col, dpix) in drow.chunks_exact_mut(4).enumerate() {
            let (logical_x, logical_y) = logical(col, row);
            let sx = src_x.saturating_add(logical_x);
            let sy = src_y.saturating_add(logical_y);
            if sx >= src_w || sy >= src_h {
                continue;
            }
            let Some(s) = sy
                .checked_mul(src_w)
                .and_then(|v| v.checked_add(sx))
                .and_then(|v| v.checked_mul(4))
            else {
                continue;
            };
            let Some(spix) = src_rgba.get(s..s.saturating_add(4)) else {
                continue;
            };
            if let ([db, dg, dr, dx], [sr, sg, sb, _sa]) = (dpix, spix) {
                *db = *sb;
                *dg = *sg;
                *dr = *sr;
                *dx = 0xFF;
            }
        }
    }
}

/// Copy one logical output rectangle out of the combined RGBA scene. Used
/// only when a qcms ICC transform is active; the common unprofiled path keeps
/// the zero-extra-copy swizzle above.
pub fn copy_rgba_subrect(
    source: SubrectSource<'_>,
    width: usize,
    height: usize,
    dst: &mut Vec<u8>,
) {
    let SubrectSource {
        src_rgba: src,
        src_w,
        src_h,
        src_x,
        src_y,
    } = source;
    dst.clear();
    dst.resize(width.saturating_mul(height).saturating_mul(4), 0);
    for row in 0..height {
        let Some(src_row) = src_y.checked_add(row) else {
            break;
        };
        if src_row >= src_h || src_x >= src_w {
            break;
        }
        let cols = width.min(src_w.saturating_sub(src_x));
        let Some(byte_offset) = src_row
            .checked_mul(src_w)
            .and_then(|v| v.checked_add(src_x))
            .and_then(|v| v.checked_mul(4))
        else {
            break;
        };
        let target = row.saturating_mul(width).saturating_mul(4);
        let bytes = cols.saturating_mul(4);
        let source_end = byte_offset.saturating_add(bytes);
        let target_end = target.saturating_add(bytes);
        if let (Some(dst_slice), Some(src_slice)) = (
            dst.get_mut(target..target_end),
            src.get(byte_offset..source_end),
        ) && dst_slice.len() == src_slice.len()
        {
            dst_slice.copy_from_slice(src_slice);
        }
    }
}

#[cfg(test)]
mod subrect_tests {
    //! Pure-CPU tests for the sub-rect swizzle — no wgpu/GPU needed, run everywhere `cargo test -p shell --lib` runs.
    use super::{
        SubrectSource, copy_rgba_subrect, swizzle_subrect_rgba_to_xrgb8888,
        swizzle_subrect_rgba_to_xrgb8888_transformed,
    };

    /// Build a `w×h` tight RGBA buffer where pixel `(x,y)` is `(x, y, 0, 0xFF)`, so the test can identify which source pixel landed where after the swizzle.
    fn ramp_rgba(w: usize, h: usize) -> Vec<u8> {
        let mut v = vec![0u8; w.saturating_mul(h).saturating_mul(4)];
        for y in 0..h {
            for x in 0..w {
                let o = y.saturating_mul(w).saturating_add(x).saturating_mul(4);
                v[o] = u8::try_from(x).unwrap_or(u8::MAX);
                v[o.saturating_add(1)] = u8::try_from(y).unwrap_or(u8::MAX);
                v[o.saturating_add(2)] = 0;
                v[o.saturating_add(3)] = 0xFF;
            }
        }
        v
    }

    #[test]
    fn whole_buffer_swizzles_like_paint_into_xrgb8888() {
        // A single output at the origin: src_x=src_y=0, out size == src size. Every pixel swizzles R,G,B -> B,G,R,0xFF — the fused single-output path's output.
        let (w, h) = (4usize, 3usize);
        let src = ramp_rgba(w, h);
        let pitch = w * 4;
        let mut dst = vec![0u8; pitch * h];
        swizzle_subrect_rgba_to_xrgb8888(
            SubrectSource {
                src_rgba: &src,
                src_w: w,
                src_h: h,
                src_x: 0,
                src_y: 0,
            },
            &mut dst,
            pitch,
            w,
            h,
        );
        for y in 0..h {
            for x in 0..w {
                let s = (y * w + x) * 4;
                let d = y * pitch + x * 4;
                assert_eq!(dst[d], src[s + 2], "B at ({x},{y})");
                assert_eq!(dst[d + 1], src[s + 1], "G at ({x},{y})");
                assert_eq!(dst[d + 2], src[s], "R at ({x},{y})");
                assert_eq!(dst[d + 3], 0xFF, "X at ({x},{y})");
            }
        }
    }

    #[test]
    fn offset_subrect_picks_the_right_source_region() {
        // Combined scene 6x2; second output is a 2x2 region at global position (4,0) — the swizzle must pull source columns 4 and 5, not 0 and 1.
        let (cw, ch) = (6usize, 2usize);
        let src = ramp_rgba(cw, ch);
        let (ow, oh) = (2usize, 2usize);
        let pitch = ow * 4;
        let mut dst = vec![0u8; pitch * oh];
        swizzle_subrect_rgba_to_xrgb8888(
            SubrectSource {
                src_rgba: &src,
                src_w: cw,
                src_h: ch,
                src_x: 4,
                src_y: 0,
            },
            &mut dst,
            pitch,
            ow,
            oh,
        );
        // dst pixel (0,0) must be source pixel (4,0): R channel == 4.
        assert_eq!(dst[2], 4, "dst(0,0).R should be source column 4");
        // dst pixel (1,1) must be source pixel (5,1): R==5, G==1.
        let d = pitch + 4;
        assert_eq!(dst[d + 2], 5, "dst(1,1).R should be source column 5");
        assert_eq!(dst[d + 1], 1, "dst(1,1).G should be source row 1");
    }

    #[test]
    fn destination_pitch_padding_is_untouched() {
        // A DRM dumb buffer's pitch usually exceeds out_w*4; the trailing padding bytes must stay as they were.
        let (w, h) = (2usize, 2usize);
        let src = ramp_rgba(w, h);
        let pitch = 16;
        let mut dst = vec![0xEEu8; pitch * h];
        swizzle_subrect_rgba_to_xrgb8888(
            SubrectSource {
                src_rgba: &src,
                src_w: w,
                src_h: h,
                src_x: 0,
                src_y: 0,
            },
            &mut dst,
            pitch,
            w,
            h,
        );
        for row in 0..h {
            // Padding after the 2 written pixels (offset 8..) stays 0xEE.
            assert_eq!(dst[row * pitch + 8], 0xEE, "row {row} padding untouched");
        }
    }

    #[test]
    fn overhanging_output_is_clamped_not_panicking() {
        // Output wider/taller than the source region remaining: must clamp.
        let (w, h) = (3usize, 3usize);
        let src = ramp_rgba(w, h);
        let (ow, oh) = (4usize, 4usize);
        let pitch = ow * 4;
        let mut dst = vec![0u8; pitch * oh];
        // src_x=2 leaves only 1 valid source column; must not panic.
        swizzle_subrect_rgba_to_xrgb8888(
            SubrectSource {
                src_rgba: &src,
                src_w: w,
                src_h: h,
                src_x: 2,
                src_y: 0,
            },
            &mut dst,
            pitch,
            ow,
            oh,
        );
        // Column 0 maps to source column 2 (valid); R==2.
        assert_eq!(dst[2], 2);
    }

    #[test]
    fn rotate_90_maps_portrait_scene_to_landscape_scanout() {
        let src = ramp_rgba(2, 3);
        let mut dst = vec![0u8; 3 * 2 * 4];
        swizzle_subrect_rgba_to_xrgb8888_transformed(
            SubrectSource {
                src_rgba: &src,
                src_w: 2,
                src_h: 3,
                src_x: 0,
                src_y: 0,
            },
            &mut dst,
            3 * 4,
            3,
            2,
            settings_store::hardware::OutputTransform::Rotate90,
        );
        let top_right = 2 * 4;
        assert_eq!((dst[top_right + 2], dst[top_right + 1]), (0, 0));
        let bottom_left = 3 * 4;
        assert_eq!((dst[bottom_left + 2], dst[bottom_left + 1]), (1, 2));
    }

    #[test]
    fn profiled_path_copies_only_the_output_viewport() {
        let src = ramp_rgba(5, 2);
        let mut output = Vec::new();
        copy_rgba_subrect(
            SubrectSource {
                src_rgba: &src,
                src_w: 5,
                src_h: 2,
                src_x: 3,
                src_y: 0,
            },
            2,
            2,
            &mut output,
        );
        assert_eq!(output.len(), 16);
        assert_eq!(output[0], 3);
        assert_eq!(output[4], 4);
        assert_eq!(output[9], 1);
    }
}

#[cfg(test)]
mod tests {
    //! These tests need a working wgpu adapter (llvmpipe inside the distrobox); soft-skip on
    //! adapter-acquisition failure so a CI host without Vulkan doesn't flake. Run inside `my-distrobox`, where llvmpipe is available.
    use super::*;
    use vello::kurbo::Affine;
    use vello::peniko::Fill;

    fn bridge_or_skip(w: u32, h: u32) -> Option<VelloDrmBridge> {
        match VelloDrmBridge::new(w, h) {
            Ok(b) => Some(b),
            Err(BridgeError::NoAdapter(msg)) => {
                eprintln!("skipping paint test: {msg}");
                None
            }
            Err(other) => panic!("unexpected bridge error: {other:?}"),
        }
    }

    fn rect_scene(w: u32, h: u32, color: Color) -> Scene {
        let mut scene = Scene::new();
        let rect = vello::kurbo::Rect::new(0.0, 0.0, f64::from(w), f64::from(h));
        scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &rect);
        scene
    }

    #[test]
    fn bridge_paint_returns_non_empty_buffer_for_non_empty_scene() {
        let Some(mut bridge) = bridge_or_skip(64, 64) else {
            return;
        };
        // Fill with cyan so the buffer is definitely not the (dark) base color.
        let scene = rect_scene(64, 64, Color::from_rgb8(0, 255, 255));
        let bytes = bridge.paint(&scene).expect("paint");
        assert_eq!(bytes.len(), 64 * 64 * 4);
        // Find at least one non-background pixel (background is the bridge's default base color, ~0x1A,0x1B,0x26).
        let any_non_bg = bytes
            .chunks_exact(4)
            .any(|p| p[0] != 0x1A || p[1] != 0x1B || p[2] != 0x26);
        assert!(
            any_non_bg,
            "expected at least one non-background pixel after painting a cyan rect"
        );
    }

    #[test]
    fn bridge_resize_changes_buffer_size() {
        let Some(mut bridge) = bridge_or_skip(1600, 900) else {
            return;
        };
        let bytes = bridge.paint(&Scene::new()).expect("paint at initial size");
        assert_eq!(bytes.len(), 1600 * 900 * 4);

        // Resize to 1280x720 and verify the next paint returns the new buffer size.
        bridge.resize(1280, 720).expect("resize");
        assert_eq!(bridge.size(), (1280, 720));
        let bytes = bridge.paint(&Scene::new()).expect("paint after resize");
        assert_eq!(bytes.len(), 1280 * 720 * 4);
    }

    #[test]
    fn bridge_paint_uses_base_color_for_empty_scene() {
        let Some(mut bridge) = bridge_or_skip(32, 32) else {
            return;
        };
        // Pick a distinctive base color so the empty background is distinguishable from any other clear.
        bridge.set_base_color(Color::from_rgb8(0x12, 0x34, 0x56));
        let bytes = bridge.paint(&Scene::new()).expect("paint empty");
        assert_eq!(bytes.len(), 32 * 32 * 4);
        for (i, p) in bytes.chunks_exact(4).enumerate() {
            assert_eq!(
                (p[0], p[1], p[2]),
                (0x12, 0x34, 0x56),
                "pixel {i} expected base color, got ({:#x}, {:#x}, {:#x})",
                p[0],
                p[1],
                p[2]
            );
        }
    }

    #[test]
    fn paint_into_xrgb8888_matches_paint_swizzled() {
        // The fused path must produce the same pixels as tight-RGBA `paint()` output swizzled to XRGB8888 — proving copy-elimination is byte-exact, not just "close".
        let Some(mut bridge) = bridge_or_skip(40, 24) else {
            return;
        };
        let scene = rect_scene(40, 24, Color::from_rgb8(10, 200, 90));

        let rgba = bridge.paint(&scene).expect("paint").to_vec();
        let mut expected = vec![0u8; 40 * 24 * 4];
        for (i, p) in rgba.chunks_exact(4).enumerate() {
            let d = &mut expected[i * 4..i * 4 + 4];
            d[0] = p[2]; // B
            d[1] = p[1]; // G
            d[2] = p[0]; // R
            d[3] = 0xFF; // X
        }

        let pitch = 40 * 4;
        let mut dst = vec![0u8; pitch * 24];
        bridge
            .paint_into_xrgb8888(&scene, &mut dst, pitch)
            .expect("paint_into_xrgb8888");
        assert_eq!(
            dst, expected,
            "fused XRGB output must equal swizzled paint()"
        );
    }

    #[test]
    fn paint_into_xrgb8888_respects_destination_pitch() {
        // A destination whose pitch exceeds width*4 (the common DRM dumb-buffer case) must leave trailing padding untouched and place each row at `row * dst_pitch`.
        let Some(mut bridge) = bridge_or_skip(8, 4) else {
            return;
        };
        bridge.set_base_color(Color::from_rgb8(0x11, 0x22, 0x33));
        let pitch = 64;
        let mut dst = vec![0xEEu8; pitch * 4];
        bridge
            .paint_into_xrgb8888(&Scene::new(), &mut dst, pitch)
            .expect("paint into padded dst");
        for row in 0..4usize {
            for x in 0..8usize {
                let o = row * pitch + x * 4;
                assert_eq!(
                    (dst[o], dst[o + 1], dst[o + 2]),
                    (0x33, 0x22, 0x11),
                    "row {row} px {x} should be base color in BGR order"
                );
            }
            assert_eq!(
                dst[row * pitch + 8 * 4],
                0xEE,
                "row {row} padding untouched"
            );
        }
    }

    #[test]
    fn bridge_handles_non_aligned_width_via_padding() {
        // Width=33 → tight stride=132, padded to 256 — verifies the row-pitch stripping in `paint`.
        let Some(mut bridge) = bridge_or_skip(33, 16) else {
            return;
        };
        bridge.set_base_color(Color::from_rgb8(0xAA, 0xBB, 0xCC));
        let bytes = bridge.paint(&Scene::new()).expect("paint non-aligned");
        assert_eq!(bytes.len(), 33 * 16 * 4);
        for p in bytes.chunks_exact(4) {
            assert_eq!((p[0], p[1], p[2]), (0xAA, 0xBB, 0xCC));
        }
    }
}
