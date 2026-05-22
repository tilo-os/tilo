//! CPU-fallback vello → RGBA bridge for the DRM backend.
//!
//! ## Why CPU-fallback (Phase 5.2 v1)
//!
//! Vello renders into a wgpu texture; smithay's DRM path drives GLES.
//! The zero-copy path is `wgpu::Texture` → DMA-BUF fd → smithay
//! `GlesRenderer::import_dmabuf` → `GlesTexture` → page-flip. That
//! interop is doable on the Vulkan wgpu backend (`hal_texture` →
//! `vulkan::Image` → external memory fd) but it's a multi-week build
//! that adds Vulkan-only constraints. Phase 5.2's mandate is "ship the
//! path; perf optimization is a follow-up": this module renders to an
//! offscreen wgpu texture, copies bytes back to a CPU buffer, and
//! exposes them as a tight-packed `&[u8]` so the DRM upload side can
//! glTexImage2D / GBM-buffer-blit them.
//!
//! Loudly logs `"DRM paint: CPU fallback active"` on first call so the
//! operator knows the slow path is engaged. Replace with the DMA-BUF
//! interop path in a Phase 5.3 follow-up.
//!
//! ## Row-pitch alignment
//!
//! `wgpu::copy_texture_to_buffer` requires `bytes_per_row` to be a
//! multiple of `COPY_BYTES_PER_ROW_ALIGNMENT` (256). For widths whose
//! tight stride isn't aligned we allocate a padded readback buffer and
//! strip the padding while copying into the public `readback`.
//!
//! ## Test strategy
//!
//! Unit tests run inside `#[cfg(test)]` and need a wgpu device, which
//! requires `llvmpipe` (`vulkan_lvp`) inside the distrobox. The
//! existing `tests/support/offscreen.rs` harness establishes that
//! llvmpipe is available; the same prerequisite applies here. The
//! tests check
//!
//! * rendering a non-empty scene yields at least one non-background
//!   pixel,
//! * rendering an empty scene yields a buffer of the base color,
//! * resizing reallocates the readback buffer to the right size.
//!
//! No real DRM device is touched; the bridge is purely "vello + wgpu
//! → CPU bytes".

use std::sync::atomic::{AtomicBool, Ordering};

use pollster::FutureExt;
use vello::peniko::Color;
use vello::util::RenderContext;
use vello::{wgpu, AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, Scene};

/// Errors the bridge can return. Each variant maps to an operator-
/// visible message; the DRM backend logs and skips the frame.
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("failed to acquire a wgpu adapter for DRM CPU-fallback paint (is llvmpipe installed?): {0}")]
    NoAdapter(String),

    #[error("failed to construct vello renderer: {0}")]
    Renderer(String),

    #[error("vello render_to_texture failed: {0}")]
    Render(String),

    #[error("wgpu buffer mapping failed: {0}")]
    Map(String),
}

/// One-shot latch so the "CPU fallback active" warning only fires once
/// per process. Shared across all bridges since the message is
/// process-global.
static WARNED_CPU_FALLBACK: AtomicBool = AtomicBool::new(false);

/// CPU-fallback bridge between a vello `Scene` and an RGBA byte
/// buffer. Owns its wgpu device + offscreen texture + readback buffer
/// so callers can reuse the bridge across frames without re-allocation.
///
/// Single-output by design — multi-output is one bridge per output.
pub struct VelloDrmBridge {
    context: RenderContext,
    dev_id: usize,
    /// The vello renderer parented to the same wgpu device as
    /// `texture`. Cached across frames so we don't pay the
    /// pipeline-compile cost on every paint.
    renderer: Renderer,
    /// Offscreen render target. Recreated on resize.
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// Readback buffer (padded to the wgpu row alignment requirement).
    readback_buf: wgpu::Buffer,
    /// Bytes-per-row in the padded readback buffer. May exceed
    /// `width * 4` when the tight stride doesn't divide 256.
    padded_bytes_per_row: u32,
    /// Tight (no padding) RGBA buffer the public `paint` returns.
    readback: Vec<u8>,
    /// Current size; resize triggers reallocation of `texture` +
    /// `readback_buf`.
    size: (u32, u32),
    /// Base color used to clear when the caller's scene is empty.
    /// Matches the winit backend's fallback so the host window doesn't
    /// flash white.
    base_color: Color,
}

impl VelloDrmBridge {
    /// Build a bridge sized at `width × height`.
    ///
    /// `width` and `height` must both be > 0 (the bridge panics
    /// otherwise; DRM connector enumeration guarantees this).
    pub fn new(width: u32, height: u32) -> Result<Self, BridgeError> {
        assert!(width > 0 && height > 0, "VelloDrmBridge: width/height must be > 0");

        let mut context = RenderContext::new();
        let dev_id = context.device(None).block_on().ok_or_else(|| {
            BridgeError::NoAdapter(
                "no wgpu adapter (install vulkan/llvmpipe drivers in the runtime)".to_owned(),
            )
        })?;
        let device = &context.devices[dev_id].device;

        let (texture, view, readback_buf, padded_bytes_per_row) =
            Self::allocate_targets(device, width, height);

        let renderer = Renderer::new(
            device,
            RendererOptions {
                surface_format: None,
                use_cpu: false,
                antialiasing_support: AaSupport::all(),
                num_init_threads: None,
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
            base_color: Color::rgb8(0x1A, 0x1B, 0x26),
        })
    }

    /// Override the per-frame clear color. Matches the winit backend
    /// API so callers can swap themes uniformly.
    pub fn set_base_color(&mut self, color: Color) {
        self.base_color = color;
    }

    /// Render `scene` into the offscreen texture and return a slice of
    /// the tight-packed RGBA bytes (`width * height * 4`).
    ///
    /// The slice's lifetime is tied to `&mut self`; callers that need
    /// to keep the bytes across frames must clone them out before the
    /// next `paint` invocation.
    pub fn paint(&mut self, scene: &Scene) -> Result<&[u8], BridgeError> {
        if !WARNED_CPU_FALLBACK.swap(true, Ordering::AcqRel) {
            tracing::warn!(
                "DRM paint: CPU fallback active — vello → wgpu readback → RGBA bytes; \
                 zero-copy DMA-BUF interop is the Phase 5.3 follow-up. \
                 Expect ~10ms+ per frame for a 1080p output."
            );
        }

        let (width, height) = self.size;
        let device = &self.context.devices[self.dev_id].device;
        let queue = &self.context.devices[self.dev_id].queue;

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

        // Stage the copy texture → padded readback buffer.
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

        // Block-map the readback buffer and copy into the tight
        // `self.readback`, stripping row padding.
        let slice = self.readback_buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            tx.send(r).ok();
        });
        device.poll(wgpu::Maintain::Wait);
        rx.recv()
            .map_err(|e| BridgeError::Map(format!("map_async channel: {e}")))?
            .map_err(|e| BridgeError::Map(format!("{e:?}")))?;

        let mapped = slice.get_mapped_range();
        let tight_row = (width * 4) as usize;
        let needed = tight_row * height as usize;
        self.readback.clear();
        self.readback.reserve(needed);
        for row in 0..height as usize {
            let start = row * self.padded_bytes_per_row as usize;
            self.readback
                .extend_from_slice(&mapped[start..start + tight_row]);
        }
        drop(mapped);
        self.readback_buf.unmap();

        Ok(&self.readback)
    }

    /// Resize the offscreen target. Reallocates the texture + readback
    /// buffer to fit the new dimensions; returns `Ok(())` even when
    /// the new size matches the current (no-op).
    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), BridgeError> {
        assert!(width > 0 && height > 0, "VelloDrmBridge::resize: width/height must be > 0");
        if (width, height) == self.size {
            return Ok(());
        }
        let device = &self.context.devices[self.dev_id].device;
        let (texture, view, readback_buf, padded_bytes_per_row) =
            Self::allocate_targets(device, width, height);
        self.texture = texture;
        self.view = view;
        self.readback_buf = readback_buf;
        self.padded_bytes_per_row = padded_bytes_per_row;
        self.size = (width, height);
        // Reset the tight buffer; it'll be repopulated on the next
        // `paint`. We could keep capacity, but the new size may be
        // larger so it's cleaner to clear.
        self.readback.clear();
        Ok(())
    }

    /// Current (width, height). Useful in tests and for callers that
    /// want to compare against the connector's mode size before
    /// resizing.
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Allocate (texture, view, readback_buf, padded_bytes_per_row)
    /// for the given size. Pulled out so `new` and `resize` share the
    /// allocation logic.
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
        let unpadded = width * bytes_per_pixel;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = unpadded.div_ceil(align) * align;
        let readback_size = (padded_bytes_per_row as u64) * (height as u64);
        let readback_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("quick-desktop-drm-paint-readback"),
            size: readback_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        (texture, view, readback_buf, padded_bytes_per_row)
    }
}

#[cfg(test)]
mod tests {
    //! These tests need a working wgpu adapter (llvmpipe inside the
    //! distrobox). We soft-skip when adapter acquisition fails so a
    //! CI run on a host without Vulkan doesn't flake. The verification
    //! recipe runs them inside `my-distrobox`, where llvmpipe is
    //! available.
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
        let rect = vello::kurbo::Rect::new(0.0, 0.0, w as f64, h as f64);
        scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &rect);
        scene
    }

    #[test]
    fn bridge_paint_returns_non_empty_buffer_for_non_empty_scene() {
        let Some(mut bridge) = bridge_or_skip(64, 64) else {
            return;
        };
        // Fill with cyan so the buffer is definitely not the (dark) base color.
        let scene = rect_scene(64, 64, Color::rgb8(0, 255, 255));
        let bytes = bridge.paint(&scene).expect("paint");
        assert_eq!(bytes.len(), 64 * 64 * 4);
        // Find at least one non-background pixel. Background is the
        // bridge's default base color (~0x1A, 0x1B, 0x26).
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
        // First render at 1600x900.
        let bytes = bridge
            .paint(&Scene::new())
            .expect("paint at initial size");
        assert_eq!(bytes.len(), 1600 * 900 * 4);

        // Resize to 1280x720 and verify the next paint returns the
        // new buffer size.
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
        // Pick a distinctive base color so we can tell the empty
        // background apart from any other clear.
        bridge.set_base_color(Color::rgb8(0x12, 0x34, 0x56));
        let bytes = bridge.paint(&Scene::new()).expect("paint empty");
        assert_eq!(bytes.len(), 32 * 32 * 4);
        // Every pixel must be exactly the base color.
        for (i, p) in bytes.chunks_exact(4).enumerate() {
            assert_eq!(
                (p[0], p[1], p[2]),
                (0x12, 0x34, 0x56),
                "pixel {i} expected base color, got ({:#x}, {:#x}, {:#x})",
                p[0], p[1], p[2]
            );
        }
    }

    #[test]
    fn bridge_handles_non_aligned_width_via_padding() {
        // Width=33 → tight stride=132, padded to 256. Verifies the
        // row-pitch stripping in `paint`.
        let Some(mut bridge) = bridge_or_skip(33, 16) else {
            return;
        };
        bridge.set_base_color(Color::rgb8(0xAA, 0xBB, 0xCC));
        let bytes = bridge.paint(&Scene::new()).expect("paint non-aligned");
        assert_eq!(bytes.len(), 33 * 16 * 4);
        for p in bytes.chunks_exact(4) {
            assert_eq!((p[0], p[1], p[2]), (0xAA, 0xBB, 0xCC));
        }
    }
}
