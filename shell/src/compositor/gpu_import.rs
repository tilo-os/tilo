//! Turning a client's GPU buffer into pixels the chrome can draw.
//!
//! # Why the CPU mmap path is not enough
//!
//! [`crate::compositor::foreign::import_dmabuf_rgba`] maps a client's
//! dma-buf plane and reads it with the CPU. That works for a client
//! whose buffer lives in system memory — an EGL/GBM client such as
//! alacritty allocates exactly that. It does **not** work for a Vulkan
//! client (which every `wgpu`-based qOS app is): its swapchain images
//! are device-local, and on amdgpu a device-local BO carries
//! `AMDGPU_GEM_CREATE_NO_CPU_ACCESS`, so `mmap` on the exported fd fails
//! with `EPERM` no matter how linear the layout is.
//!
//! The buffer is perfectly readable — just not by the CPU. So we ask the
//! GPU to read it:
//!
//! ```text
//!   client dmabuf ─► eglCreateImageKHR (EGL_LINUX_DMA_BUF_EXT)
//!                 ─► GL texture
//!                 ─► FBO + glReadPixels into a PBO   (GPU-side copy)
//!                 ─► RGBA8 bytes ─► ForeignFrameStore ─► vello ImageBrush
//! ```
//!
//! This is a real dmabuf import: the client's memory is never copied by
//! the CPU on the producing side, the GPU does the detiling / format
//! conversion, and we pay one readback of the visible tile per *commit*
//! (not per paint — see the store's sequence-number gating).
//!
//! # Why not hand the texture to vello
//!
//! Zero-copy all the way to the screen would mean giving vello an
//! externally-owned GPU texture. `peniko::ImageBrush` takes a byte blob,
//! and the compositor's paint target is a wgpu device that is not the GL
//! context this importer owns (it can even be a different GPU). Bridging
//! those is a wgpu-hal-level change well beyond the buffer path; the
//! readback keeps the seam at "bytes", which every existing consumer
//! (tiles, share-session encoder, screenshots) already speaks.
//!
//! # Lifetime
//!
//! One importer per process, created lazily on the first dmabuf commit
//! and pinned to the thread that created it (GL contexts are not `Send`
//! and `GlesRenderer` is `!Send`). Construction failure is cached: a box
//! with no usable EGL device must not re-probe on every frame.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::{Buffer as _, Fourcc};
use smithay::backend::egl::{EGLContext, EGLDevice, EGLDisplay};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::{ExportMem, ImportDma};
use smithay::utils::{Point, Rectangle, Size};

/// Why the GPU import path is unavailable or failed for one buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpuImportError {
    /// No EGL device / context could be created at all — the whole path
    /// is off for this process.
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

/// Does `device` sit on the DRM node at `want`?
///
/// EGL exposes both the primary (`card`) and render (`renderD`) node
/// names, and which one is populated depends on the driver, so both are
/// checked. Comparison is on `st_rdev` rather than on the path string —
/// `/dev/dri/renderD128` and a bind-mounted equivalent are the same
/// device even though the strings differ.
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
    /// Build an importer on the DRM node at `node`.
    ///
    /// `node` should be the same render node the compositor advertises
    /// as the dmabuf `main_device`, so the buffers a client hands us were
    /// allocated on the very GPU that reads them back — a cross-device
    /// import would work only where the kernel can peer-map, and would
    /// silently be a PCIe copy where it did.
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
    /// Returns the buffer's `(width, height)`. `out` is cleared and
    /// refilled; the caller's `Vec` is reused across commits so a
    /// steady-state client costs no reallocation.
    ///
    /// The GL scratch the readback allocates is released before this
    /// returns — see [`Self::release_readback_scratch`].
    pub fn import(
        &mut self,
        dmabuf: &Dmabuf,
        out: &mut Vec<u8>,
    ) -> Result<(u32, u32), GpuImportError> {
        let result = self.read_back(dmabuf, out);
        self.release_readback_scratch();
        result
    }

    /// Hand back every GL object the last readback finished with.
    ///
    /// `copy_texture` allocates a pixel-pack buffer the size of the
    /// whole client image — for a full-tile window, ~2.8 MB of GTT per
    /// call — and its `Drop` cannot free it: deleting a GL object needs
    /// the context current, and `Drop` has no way to make it so. It
    /// posts the id to the renderer's destruction queue instead, and
    /// something has to drain that queue.
    ///
    /// Smithay drains it from `Frame::finish` and from `Unbind`, which
    /// assume a renderer is a thing that draws. This one never draws —
    /// it only reads client buffers back — so nothing drained the
    /// queue, and every client commit left one whole-window pixel-pack
    /// buffer behind for the compositor's lifetime: hundreds of
    /// megabytes after a minute of an animating window, proportional to
    /// frames drawn rather than to windows open.
    ///
    /// Draining also prunes the dmabuf→texture cache of entries whose
    /// buffer the client has since destroyed.
    ///
    /// A failure here is reported and swallowed: the pixels are already
    /// copied out, and refusing the frame would trade a visible window
    /// for a memory saving.
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
    ) -> Result<(u32, u32), GpuImportError> {
        let width = dmabuf.width();
        let height = dmabuf.height();
        if width == 0 || height == 0 {
            return Err(GpuImportError::InvalidDimensions { width, height });
        }

        let texture = self
            .renderer
            .import_dmabuf(dmabuf, None)
            .map_err(|e| GpuImportError::ImportFailed(e.to_string()))?;

        // `Abgr8888` is `[R, G, B, A]` in memory — i.e. exactly the
        // `ImageFormat::Rgba8` blob vello wants, so the readback lands
        // in its final layout with no swizzle pass.
        let region = Rectangle::new(
            Point::from((0, 0)),
            Size::from((width as i32, height as i32)),
        );
        let mapping = self
            .renderer
            .copy_texture(&texture, region, Fourcc::Abgr8888)
            .map_err(|e| GpuImportError::ReadbackFailed(e.to_string()))?;
        let bytes = self
            .renderer
            .map_texture(&mapping)
            .map_err(|e| GpuImportError::ReadbackFailed(e.to_string()))?;

        let expected = (width as usize) * (height as usize) * 4;
        if bytes.len() < expected {
            return Err(GpuImportError::ReadbackFailed(format!(
                "readback returned {} bytes, expected {expected}",
                bytes.len()
            )));
        }
        out.clear();
        out.reserve(expected);
        out.extend_from_slice(&bytes[..expected]);

        // The `X` FourCCs have no alpha channel — the fourth byte is
        // undefined padding, and whatever the driver left there would
        // otherwise show up as a see-through tile. Force it opaque, the
        // same way the CPU importer's swizzle does.
        if is_opaque_fourcc(dmabuf.format().code) {
            for px in out.chunks_exact_mut(4) {
                px[3] = 0xFF;
            }
        }
        Ok((width, height))
    }
}

/// Whether a FourCC's fourth channel is padding rather than alpha.
///
/// Free function so the alpha fixup is testable without a GL context —
/// getting this wrong makes every GPU client's tile translucent, which
/// is subtle enough to slip through a glance at a screenshot.
pub fn is_opaque_fourcc(code: Fourcc) -> bool {
    matches!(code, Fourcc::Xrgb8888 | Fourcc::Xbgr8888)
}

thread_local! {
    /// Lazily-built process importer.
    static IMPORTER: RefCell<Option<Result<GpuImporter, GpuImportError>>> =
        const { RefCell::new(None) };
}

/// Run `f` with the thread's GPU importer, building it on first use.
///
/// `node` is the render node to build on (the advertised dmabuf main
/// device). Returns the construction error — cached, so a machine
/// without EGL pays the probe exactly once — when no importer exists.
///
/// Callers must not re-enter: `f` runs with the thread-local borrowed.
pub fn with_importer<R>(
    node: &Path,
    f: impl FnOnce(&mut GpuImporter) -> R,
) -> Result<R, GpuImportError> {
    IMPORTER.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.is_none() {
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
            *slot = Some(built);
        }
        match slot.as_mut().expect("just initialised") {
            Ok(importer) => Ok(f(importer)),
            Err(e) => Err(e.clone()),
        }
    })
}

/// Whether this thread has a working GPU importer *without* building
/// one. Used by tests and diagnostics that must not trigger the probe.
pub fn is_ready() -> bool {
    IMPORTER.with(|cell| matches!(cell.borrow().as_ref(), Some(Ok(_))))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GPU memory this process is holding, in KiB.
    ///
    /// `drm-memory-<region>:` is reported once per open DRM file and
    /// once per memory region, so every line has to be summed — reading
    /// one fd, or one region, undercounts. `None` means this kernel
    /// exposes no per-file DRM accounting at all, which callers must
    /// treat as "cannot observe", never as "nothing allocated".
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
                let digits: String = amount.chars().filter(|c| c.is_ascii_digit()).collect();
                if let Ok(kib) = digits.parse::<u64>() {
                    total += kib;
                    saw_any = true;
                }
            }
        }
        saw_any.then_some(total)
    }

    /// Every readback allocates a pixel-pack buffer the size of the
    /// whole client image, and hands it back only when someone drains
    /// the renderer's destruction queue.
    ///
    /// So the invariant is about *repetition*: a hundred readbacks must
    /// not cost more than a few. This drives the same
    /// `copy_texture` → `map_texture` → drop → release sequence
    /// [`GpuImporter::read_back`] runs per client commit, from a
    /// texture the test can make without a client.
    #[test]
    fn repeated_readbacks_do_not_accumulate_gpu_memory() {
        use smithay::backend::renderer::ImportMem;

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

        // Big enough that a leaked buffer cannot hide inside a driver's
        // small-allocation slab: one readback is 4 MiB.
        const SIDE: i32 = 1024;
        const ROUNDS: usize = 24;
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

        // One warm-up: the first readback also pays for whatever the
        // driver allocates lazily, which is a one-off, not a leak.
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

    #[test]
    fn opaque_fourccs_are_exactly_the_x_variants() {
        assert!(is_opaque_fourcc(Fourcc::Xrgb8888));
        assert!(is_opaque_fourcc(Fourcc::Xbgr8888));
        assert!(
            !is_opaque_fourcc(Fourcc::Argb8888),
            "ARGB carries real alpha; forcing it opaque would break client transparency"
        );
        assert!(!is_opaque_fourcc(Fourcc::Abgr8888));
    }

    #[test]
    fn errors_name_the_failing_stage() {
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

    /// On a machine with a usable GPU the importer must actually build —
    /// this is the gate that separates "GPU import is wired up" from
    /// "everything silently fell back to the CPU path".
    #[test]
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
}
