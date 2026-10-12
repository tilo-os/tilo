//! Turning a client's GPU buffer into pixels the chrome can draw.
//!
//! [`crate::compositor::foreign::import_dmabuf_rgba`] CPU-maps a client's dma-buf, which works
//! for system-memory buffers (EGL/GBM clients like alacritty) but not a Vulkan client (every
//! `wgpu`-based Tilo app): swapchain images are device-local, and on amdgpu a device-local BO
//! carries `AMDGPU_GEM_CREATE_NO_CPU_ACCESS`, so `mmap` fails with `EPERM` regardless of layout.
//! The buffer is readable, just not by the CPU — so the GPU reads it instead:
//!
//! ```text
//!   client dmabuf ─► eglCreateImageKHR (EGL_LINUX_DMA_BUF_EXT)
//!                 ─► GL texture
//!                 ─► FBO + glReadPixels into a PBO   (GPU-side copy)
//!                 ─► RGBA8 bytes ─► ForeignFrameStore ─► vello ImageBrush
//! ```
//!
//! A dmabuf import (CPU never copies the client's memory; GPU does detiling/format
//! conversion; one readback per *commit*, not per paint). Zero-copy to vello isn't feasible —
//! `peniko::ImageBrush` takes a byte blob and the paint target's wgpu context differs from this
//! importer's GL context (possibly a different GPU) — so the readback keeps the seam at "bytes".
//!
//! One importer per process, lazily built on the first dmabuf commit and pinned to its creating
//! thread (`GlesRenderer` is `!Send`); construction failure is cached so a box with no usable EGL device doesn't re-probe every frame.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::{Buffer as _, Fourcc};
use smithay::backend::egl::{EGLContext, EGLDevice, EGLDisplay};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::{ExportMem, ImportDma};
use smithay::utils::{Point, Rectangle, Size};

/// Readbacks made through this importer since the process started.
static READBACKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// GL readbacks of client buffers to the CPU since the process started. A
/// frame kept on the GPU ([`crate::shell::gpu_frames`]) never adds to it.
pub fn readback_count() -> u64 {
    READBACKS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Why the GPU import path is unavailable or failed for one buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpuImportError {
    /// No EGL device / context could be created at all — the whole path is off for this process.
    Unavailable(String),
    /// EGL refused to wrap the client's dma-buf as an `EGLImage`.
    ImportFailed(String),
    /// The GPU-side readback (FBO bind + `glReadPixels`) failed.
    ReadbackFailed(String),
    /// Degenerate buffer dimensions.
    InvalidDimensions { width: u32, height: u32 },
}

impl std::fmt::Display for GpuImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GpuImportError::Unavailable(why) => write!(f, "no GPU import context: {why}"),
            GpuImportError::ImportFailed(why) => write!(f, "GPU dmabuf import failed: {why}"),
            GpuImportError::ReadbackFailed(why) => write!(f, "GPU readback failed: {why}"),
            GpuImportError::InvalidDimensions { width, height } => {
                write!(f, "dmabuf dimensions invalid: {width}x{height}")
            }
        }
    }
}

impl std::error::Error for GpuImportError {}

/// An EGL/GLES context used only to read client buffers back into RGBA.
pub struct GpuImporter {
    renderer: GlesRenderer,
    /// The DRM node the context was created on, for logging.
    device: PathBuf,
}

impl std::fmt::Debug for GpuImporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuImporter")
            .field("device", &self.device)
            .finish_non_exhaustive()
    }
}

/// Does `device` sit on the DRM node at `want`? EGL exposes both the primary (`card`) and
/// render (`renderD`) node names, so both are checked; comparison is on `st_rdev` rather than
/// path string, since `/dev/dri/renderD128` and a bind-mounted equivalent are the same device even though the strings differ.
fn device_matches(device: &EGLDevice, want_rdev: libc::dev_t) -> bool {
    let rdev_of = |p: PathBuf| -> Option<libc::dev_t> {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::metadata(p).ok().map(|m| m.rdev())
    };
    device
        .render_device_path()
        .ok()
        .and_then(rdev_of)
        .is_some_and(|d| d == want_rdev)
        || device
            .drm_device_path()
            .ok()
            .and_then(rdev_of)
            .is_some_and(|d| d == want_rdev)
}

impl GpuImporter {
    /// Build an importer on the DRM node at `node`, which should be the same render node the
    /// compositor advertises as the dmabuf `main_device` — a cross-device import would work only where the kernel can peer-map, and silently be a `PCIe` copy where it did.
    ///
    /// # Errors
    ///
    /// Returns [`GpuImportError::Unavailable`] if `node` cannot be `stat`'d, EGL reports no
    /// devices, no EGL device is bound to `node`, or the EGL display, context or renderer
    /// cannot be created on it.
    pub fn new(node: &Path) -> Result<Self, GpuImportError> {
        use std::os::unix::fs::MetadataExt as _;
        let want = std::fs::metadata(node)
            .map_err(|e| GpuImportError::Unavailable(format!("stat {}: {e}", node.display())))?
            .rdev();

        let devices: Vec<EGLDevice> = EGLDevice::enumerate()
            .map_err(|e| GpuImportError::Unavailable(format!("eglQueryDevicesEXT: {e}")))?
            .collect();
        if devices.is_empty() {
            return Err(GpuImportError::Unavailable(
                "EGL reported no devices".to_string(),
            ));
        }
        let device = devices
            .into_iter()
            .find(|d| device_matches(d, want))
            .ok_or_else(|| {
                GpuImportError::Unavailable(format!("no EGL device is bound to {}", node.display()))
            })?;

        // SAFETY: `device` is a live `EGLDeviceEXT` we just enumerated,
        // and the returned display is owned by the `EGLContext` below
        // for the rest of the process (the importer is never dropped
        // while a texture from it is alive — `import` finishes its
        // readback before returning).
        let display = unsafe { EGLDisplay::new(device) }
            .map_err(|e| GpuImportError::Unavailable(format!("eglGetPlatformDisplay: {e}")))?;
        let context = EGLContext::new(&display)
            .map_err(|e| GpuImportError::Unavailable(format!("eglCreateContext: {e}")))?;
        // SAFETY: `context` was created above and is not current on any
        // other thread; `GlesRenderer::new` makes it current on this one
        // and the importer is `!Send`, so it stays here.
        let renderer = unsafe { GlesRenderer::new(context) }
            .map_err(|e| GpuImportError::Unavailable(format!("GlesRenderer::new: {e}")))?;

        Ok(Self {
            renderer,
            device: node.to_path_buf(),
        })
    }

    pub fn device(&self) -> &Path {
        &self.device
    }

    /// Import `dmabuf` and read it back into `out` as row-major RGBA8.
    ///
    /// Returns the buffer's `(width, height)`. `out` is cleared and refilled; the caller's `Vec` is
    /// reused across commits so a steady-state client costs no reallocation. The GL scratch the readback allocates is released before this returns — see `Self::release_readback_scratch`.
    ///
    /// # Errors
    ///
    /// Returns [`GpuImportError::InvalidDimensions`] for a degenerate or unrepresentable
    /// buffer size, [`GpuImportError::ImportFailed`] if EGL cannot wrap the client's dma-buf,
    /// or [`GpuImportError::ReadbackFailed`] if the GPU-side copy or map fails.
    pub fn import(
        &mut self,
        dmabuf: &Dmabuf,
        out: &mut Vec<u8>,
    ) -> Result<(u32, u32), GpuImportError> {
        self.import_as(dmabuf, out, Fourcc::Abgr8888)
    }

    /// [`Self::import`] reading back in `readback`: `Abgr8888` (RGBA8, `X`
    /// formats forced opaque), or `Abgr2101010` / `Abgr16161616f` to keep a
    /// 10-bit or half-float buffer's precision, as-is.
    ///
    /// # Errors
    ///
    /// As [`Self::import`], and [`GpuImportError::ReadbackFailed`] when the
    /// driver cannot read back in `readback`.
    pub fn import_as(
        &mut self,
        dmabuf: &Dmabuf,
        out: &mut Vec<u8>,
        readback: Fourcc,
    ) -> Result<(u32, u32), GpuImportError> {
        let result = self.read_back(dmabuf, out, readback);
        self.release_readback_scratch();
        result
    }

    /// Hand back every GL object the last readback finished with.
    ///
    /// `copy_texture` allocates a pixel-pack buffer the size of the whole client image (~2.8 MB of
    /// GTT for a full-tile window); its `Drop` can't free it, since deleting a GL object needs the
    /// context current, which `Drop` has no way to arrange — it posts the id to the renderer's destruction queue instead, which something has to drain.
    ///
    /// Smithay drains it from `Frame::finish`/`Unbind` (a renderer that draws); this one only reads
    /// buffers back, so nothing drained it — every commit leaked a whole-window pixel-pack buffer,
    /// hundreds of MB after a minute of animation. Draining also prunes the dmabuf→texture cache of buffers the client has since destroyed; a failure here is reported and swallowed since the pixels are already copied out.
    fn release_readback_scratch(&mut self) {
        use smithay::backend::renderer::Renderer as _;
        if let Err(e) = self.renderer.cleanup_texture_cache() {
            tracing::warn!(
                target: "shell::compositor::dmabuf",
                error = %e,
                "could not release GPU readback scratch; GTT will grow"
            );
        }
    }

    fn read_back(
        &mut self,
        dmabuf: &Dmabuf,
        out: &mut Vec<u8>,
        readback: Fourcc,
    ) -> Result<(u32, u32), GpuImportError> {
        let width = dmabuf.width();
        let height = dmabuf.height();
        if width == 0 || height == 0 {
            return Err(GpuImportError::InvalidDimensions { width, height });
        }
        READBACKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let texture = self
            .renderer
            .import_dmabuf(dmabuf, None)
            .map_err(|e| GpuImportError::ImportFailed(e.to_string()))?;

        // `width`/`height` are client-controlled; a GL region needs `i32` coordinates, so a
        // buffer claiming a size that does not fit one is rejected rather than wrapped.
        let region_width = i32::try_from(width)
            .map_err(|_| GpuImportError::InvalidDimensions { width, height })?;
        let region_height = i32::try_from(height)
            .map_err(|_| GpuImportError::InvalidDimensions { width, height })?;

        // `Abgr8888` is `[R, G, B, A]` in memory — exactly the `ImageFormat::Rgba8` blob vello wants, so the readback lands in its final layout with no swizzle pass.
        let region = Rectangle::new(
            Point::from((0, 0)),
            Size::from((region_width, region_height)),
        );
        let mapping = self
            .renderer
            .copy_texture(&texture, region, readback)
            .map_err(|e| GpuImportError::ReadbackFailed(e.to_string()))?;
        let bytes = self
            .renderer
            .map_texture(&mapping)
            .map_err(|e| GpuImportError::ReadbackFailed(e.to_string()))?;

        // Also client-controlled: a buffer whose reported area overflows `usize` is rejected
        // rather than silently wrapping into a too-small allocation.
        let bytes_per_pixel = if readback == Fourcc::Abgr16161616f {
            8
        } else {
            4
        };
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|area| area.checked_mul(bytes_per_pixel))
            .ok_or(GpuImportError::InvalidDimensions { width, height })?;
        let Some(head) = bytes.get(..expected) else {
            return Err(GpuImportError::ReadbackFailed(format!(
                "readback returned {} bytes, expected {expected}",
                bytes.len()
            )));
        };
        out.clear();
        out.reserve(expected);
        out.extend_from_slice(head);

        // The `X` FourCCs have no alpha channel — the fourth byte is undefined padding that would otherwise show up as a see-through tile. Force it opaque, same as the CPU importer's swizzle.
        if readback == Fourcc::Abgr8888 && is_opaque_fourcc(dmabuf.format().code) {
            for px in out.chunks_exact_mut(4) {
                if let Some(alpha) = px.last_mut() {
                    *alpha = 0xFF;
                }
            }
        }
        Ok((width, height))
    }
}

/// Whether a `FourCC`'s fourth channel is padding rather than alpha. Free function so the alpha
/// fixup is testable without a GL context — getting it wrong makes every GPU client's tile translucent, subtle enough to slip through a glance at a screenshot.
pub fn is_opaque_fourcc(code: Fourcc) -> bool {
    matches!(code, Fourcc::Xrgb8888 | Fourcc::Xbgr8888)
}

thread_local! {
    /// Lazily-built process importer.
    static IMPORTER: RefCell<Option<Result<GpuImporter, GpuImportError>>> =
        const { RefCell::new(None) };
}

/// Run `f` with the thread's GPU importer, building it on first use. `node` is the render node
/// to build on. Returns the construction error — cached, so a machine without EGL pays the probe once — when no importer exists. Callers must not re-enter: `f` runs with the thread-local borrowed.
///
/// # Errors
///
/// Returns the cached [`GpuImportError`] from [`GpuImporter::new`] when this thread has no
/// working GPU importer; `f` does not run in that case.
pub fn with_importer<R>(
    node: &Path,
    f: impl FnOnce(&mut GpuImporter) -> R,
) -> Result<R, GpuImportError> {
    IMPORTER.with(|cell| {
        let mut slot = cell.borrow_mut();
        let built = slot.get_or_insert_with(|| {
            let built = GpuImporter::new(node);
            match &built {
                Ok(imp) => tracing::info!(
                    target: "shell::compositor::dmabuf",
                    device = %imp.device().display(),
                    "GPU dmabuf import ready (EGL + GLES readback)"
                ),
                Err(e) => tracing::warn!(
                    target: "shell::compositor::dmabuf",
                    error = %e,
                    "GPU dmabuf import unavailable; falling back to CPU mmap import"
                ),
            }
            built
        });
        match built {
            Ok(importer) => Ok(f(importer)),
            Err(e) => Err(e.clone()),
        }
    })
}

/// Whether this thread has a working GPU importer *without* building one — used by tests and diagnostics that must not trigger the probe.
pub fn is_ready() -> bool {
    IMPORTER.with(|cell| matches!(cell.borrow().as_ref(), Some(Ok(_))))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GPU memory this process is holding, in KiB. `drm-memory-<region>:` is reported once per open
    /// DRM file and once per region, so every line has to be summed — one fd or one region
    /// undercounts. `None` means this kernel exposes no per-file DRM accounting, which callers must treat as "cannot observe", never "nothing allocated".
    fn own_gpu_memory_kib() -> Option<u64> {
        let mut total = 0_u64;
        let mut saw_any = false;
        let Ok(entries) = std::fs::read_dir("/proc/self/fdinfo") else {
            return None;
        };
        for entry in entries.flatten() {
            let Ok(text) = std::fs::read_to_string(entry.path()) else {
                continue;
            };
            for line in text.lines() {
                let Some(value) = line.strip_prefix("drm-memory-") else {
                    continue;
                };
                let Some((_region, amount)) = value.split_once(':') else {
                    continue;
                };
                let digits: String = amount.chars().filter(char::is_ascii_digit).collect();
                if let Ok(kib) = digits.parse::<u64>() {
                    total = total.saturating_add(kib);
                    saw_any = true;
                }
            }
        }
        saw_any.then_some(total)
    }

    #[test]
    fn importer_builds_on_the_render_node_and_does_not_leak_gpu_memory() {
        // Phase `importer_builds_on_the_advertised_render_node`.
        // On a machine with a usable GPU the importer must actually build — the gate that separates "GPU import is wired up" from "everything silently fell back to the CPU path".
        fn importer_builds_on_the_advertised_render_node() {
            let Ok(node) = crate::compositor::render_node::probe() else {
                eprintln!("skipping: no advertisable render node on this machine");
                return;
            };
            match GpuImporter::new(&node.path) {
                Ok(imp) => assert_eq!(imp.device(), node.path.as_path()),
                Err(e) => panic!(
                    "the compositor advertises {} as the dmabuf main device but cannot \
                     build a GL import context on it: {e}",
                    node.path.display()
                ),
            }
        }
        // Phase `repeated_readbacks_do_not_accumulate_gpu_memory`.
        // Every readback allocates a pixel-pack buffer the size of the whole client image, handed back
        // only when someone drains the renderer's destruction queue. The invariant is about
        // *repetition*: a hundred readbacks must not cost more than a few — this drives the same
        // `copy_texture` → `map_texture` → drop → release sequence [`GpuImporter::read_back`] runs per commit, from a texture the test can make without a client.
        fn repeated_readbacks_do_not_accumulate_gpu_memory() {
            use smithay::backend::renderer::ImportMem;

            // Big enough that a leaked buffer cannot hide inside a driver's small-allocation slab: one readback is 4 MiB.
            const SIDE: i32 = 1024;
            const ROUNDS: usize = 24;

            let Ok(node) = crate::compositor::render_node::probe() else {
                eprintln!("skipping: no advertisable render node on this machine");
                return;
            };
            let mut importer = match GpuImporter::new(&node.path) {
                Ok(importer) => importer,
                Err(e) => panic!(
                    "the compositor advertises {} as the dmabuf main device but cannot \
                     build a GL import context on it: {e}",
                    node.path.display()
                ),
            };

            let pixels = vec![0x40_u8; (SIDE * SIDE * 4) as usize];
            let size = Size::from((SIDE, SIDE));
            let texture = importer
                .renderer
                .import_memory(&pixels, Fourcc::Abgr8888, size, false)
                .expect("importing a linear RGBA texture from memory");
            let region = Rectangle::new(Point::from((0, 0)), size);

            let read_back_once = |importer: &mut GpuImporter| {
                let mapping = importer
                    .renderer
                    .copy_texture(&texture, region, Fourcc::Abgr8888)
                    .expect("GPU readback into a pixel-pack buffer");
                let bytes = importer
                    .renderer
                    .map_texture(&mapping)
                    .expect("mapping the pixel-pack buffer");
                assert!(!bytes.is_empty(), "a readback must return pixels");
                drop(mapping);
                importer.release_readback_scratch();
            };

            // One warm-up: the first readback also pays for whatever the driver allocates lazily, a one-off, not a leak.
            read_back_once(&mut importer);

            let Some(before) = own_gpu_memory_kib() else {
                panic!(
                    "GL import works on {} but /proc/self/fdinfo reports no \
                     drm-memory-* accounting, so this test cannot see whether \
                     readback buffers are freed. An unobservable invariant is \
                     not a passing one.",
                    node.path.display()
                )
            };
            for _ in 0..ROUNDS {
                read_back_once(&mut importer);
            }
            let after = own_gpu_memory_kib().expect("accounting was present a moment ago");

            let grew_kib = after.saturating_sub(before);
            let leaked_all_kib = (ROUNDS as u64) * (SIDE as u64) * (SIDE as u64) * 4 / 1024;
            assert!(
                grew_kib < leaked_all_kib / 4,
                "{ROUNDS} readbacks grew this process's GPU memory by {grew_kib} KiB; \
                 holding on to every pixel-pack buffer would cost {leaked_all_kib} KiB, \
                 so the renderer's destruction queue is not being drained"
            );
        }
        // Phase `importer_builds_on_the_advertised_render_node` (defined above).
        importer_builds_on_the_advertised_render_node();
        // Phase `repeated_readbacks_do_not_accumulate_gpu_memory` (defined above).
        repeated_readbacks_do_not_accumulate_gpu_memory();
    }

    #[test]
    fn fourcc_opacity_and_error_stages_are_exact() {
        // Phase `opaque_fourccs_are_exactly_the_x_variants`.
        assert!(is_opaque_fourcc(Fourcc::Xrgb8888));
        assert!(is_opaque_fourcc(Fourcc::Xbgr8888));
        assert!(
            !is_opaque_fourcc(Fourcc::Argb8888),
            "ARGB carries real alpha; forcing it opaque would break client transparency"
        );
        assert!(!is_opaque_fourcc(Fourcc::Abgr8888));
        // Phase `errors_name_the_failing_stage`.
        assert!(
            GpuImportError::Unavailable("eglCreateContext: nope".into())
                .to_string()
                .contains("eglCreateContext"),
            "a construction failure must carry the EGL call that failed"
        );
        assert!(
            GpuImportError::ReadbackFailed("glReadPixels".into())
                .to_string()
                .contains("readback"),
            "readback failures must be distinguishable from import failures"
        );
    }
}
