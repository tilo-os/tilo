//! Compositing of foreign (external Wayland client) surfaces into tabs.
//!
//! An external client that binds our `wl_compositor` + `xdg_wm_base` gets
//! its toplevel adopted into the workspace tab tree by
//! [`crate::compositor::tab_binding::adopt_toplevel`]; this module turns
//! that bookkeeping entry into pixels on screen:
//!
//! ```text
//!  client commit
//!      ├─ wl_shm  ──► surface_readback::buffer_to_rgba ──┐
//!      └─ dmabuf  ──► gpu_import / import_dmabuf_rgba ────┤
//!                                                        ▼
//!                                            ForeignFrameStore (per surface)
//!                                                        │  snapshot on seq change
//!                                                        ▼
//!                              ForeignSurfaceWidget::draw → peniko::ImageBrush
//!                                                        │
//!                                            widget rect ─┴─► xdg_toplevel.configure
//! ```
//!
//! Hosted as a [`ui::widget::AppWidget`] (not a post-pass blit), sharing
//! placement/clipping/masking/fades/active-tab-only-paints with every
//! other tile; [`ForeignSurfaceWidget::needs_animation_frame`] is `true`
//! only when the store holds an undrawn frame, so an idle client costs one lock + lookup per tick and zero paints.
//!
//! dmabuf tries two routes via [`ForeignFrameStore::capture_dmabuf`]: GPU
//! ([`crate::compositor::gpu_import`], the only route for a Vulkan
//! client whose swapchain images refuse `mmap`), then CPU mmap
//! ([`import_dmabuf_rgba`], for CPU-visible linear buffers or a box with
//! no EGL device) — together covering the whole advertised set
//! (single-plane RGBA under `Modifier::Linear`); anything else is **rejected with a visible on-tile message**, never silently blank.

use std::collections::HashMap;
use std::os::fd::AsRawFd;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::{Buffer as _, Fourcc, Modifier};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::wayland::shell::xdg::ToplevelSurface;

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{AppKind, AppWidget, IconDescriptor, Widget};

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{Blob, Extend, Fill, ImageAlphaType, ImageBrush, ImageData, ImageFormat};
use winit::event::{Modifiers, WindowEvent};

use crate::compositor::surface_readback::{ReadbackError, buffer_to_rgba};
use crate::shell::state::{TileContent as ShellTile, WorkspaceTree};

/// Wayland `wl_surface` protocol id, widened — same key space the rest of the compositor uses for foreign surfaces.
pub type SurfaceId = u64;

// dmabuf CPU import

/// `struct dma_buf_sync { __u64 flags; }` — `DMA_BUF_IOCTL_SYNC` is `_IOW('b', 0, struct dma_buf_sync)`, i.e. `(_IOC_WRITE << 30) | (8 << 16) | ('b' << 8) | 0`.
const DMA_BUF_IOCTL_SYNC: libc::c_ulong = 0x4008_6200;
const DMA_BUF_SYNC_READ: u64 = 1 << 0;
const DMA_BUF_SYNC_START: u64 = 0;
const DMA_BUF_SYNC_END: u64 = 1 << 2;

/// Why a dmabuf could not be imported — every variant is reported on the tile itself rather than degrading to a blank surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DmabufImportError {
    /// The buffer carries a vendor tiling/compression modifier — we only advertise `Modifier::Linear`, so the client ignored it.
    NonLinearModifier(u64),
    /// Multi-planar (e.g. NV12). The RGBA path here is single-plane only.
    MultiPlanar(usize),
    /// `FourCC` we don't know how to swizzle.
    UnsupportedFormat(Fourcc),
    /// Degenerate width / height / stride.
    InvalidDimensions {
        width: u32,
        height: u32,
        stride: u32,
    },
    /// `mmap` failed (errno attached).
    MapFailed(i32),
}

impl std::fmt::Display for DmabufImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DmabufImportError::NonLinearModifier(m) => {
                write!(f, "dmabuf modifier 0x{m:x} is not linear")
            }
            DmabufImportError::MultiPlanar(n) => {
                write!(
                    f,
                    "dmabuf has {n} planes; only single-plane RGBA is imported"
                )
            }
            DmabufImportError::UnsupportedFormat(c) => {
                write!(f, "dmabuf fourcc {c:?} is not an RGBA-family format")
            }
            DmabufImportError::InvalidDimensions {
                width,
                height,
                stride,
            } => write!(
                f,
                "dmabuf dimensions invalid: {width}x{height} stride {stride}"
            ),
            DmabufImportError::MapFailed(e) => write!(f, "dmabuf mmap failed: errno {e}"),
        }
    }
}

impl std::error::Error for DmabufImportError {}

/// Whether a buffer carrying `modifier` has a row-major layout the CPU import can read.
///
/// `LINEAR` obviously qualifies. `INVALID` (the "implicit modifier"
/// escape hatch for an `add`-less `zwp_linux_buffer_params_v1.create`) is
/// accepted too, since the compositor only advertises `LINEAR`, so a
/// conforming client dropping the modifier still allocated linear. Anything else is rejected so the tile reports it rather than painting noise.
fn modifier_is_cpu_readable(modifier: Modifier) -> bool {
    matches!(modifier, Modifier::Linear | Modifier::Invalid)
}

/// Byte layout of one pixel in memory for a DRM `FourCC` — names read the
/// *most significant* byte first of a little-endian word, so `ARGB8888` is `[B, G, R, A]` and `ABGR8888` is `[R, G, B, A]`.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum PixelOrder {
    /// `[B, G, R, A]` — needs an R/B swap to reach RGBA.
    Bgra { opaque: bool },
    /// `[R, G, B, A]` — already RGBA.
    Rgba { opaque: bool },
}

impl PixelOrder {
    fn for_fourcc(code: Fourcc) -> Option<Self> {
        match code {
            Fourcc::Argb8888 => Some(PixelOrder::Bgra { opaque: false }),
            Fourcc::Xrgb8888 => Some(PixelOrder::Bgra { opaque: true }),
            Fourcc::Abgr8888 => Some(PixelOrder::Rgba { opaque: false }),
            Fourcc::Xbgr8888 => Some(PixelOrder::Rgba { opaque: true }),
            _ => None,
        }
    }
}

/// Convert one row of `width` pixels laid out per `order` into RGBA,
/// appending to `out` — a free function so unit tests can pin the channel mapping without a GPU or wayland connection.
fn swizzle_row(order: PixelOrder, src: &[u8], width: usize, out: &mut Vec<u8>) {
    debug_assert_eq!(src.len(), width.saturating_mul(4));
    match order {
        PixelOrder::Bgra { opaque } => {
            for px in src.chunks_exact(4) {
                // `chunks_exact(4)` guarantees this length; the pattern
                // can't miss, but this stays panic-free even if that ever changes.
                let [b, g, r, a] = px else { continue };
                out.push(*r);
                out.push(*g);
                out.push(*b);
                out.push(if opaque { 0xFF } else { *a });
            }
        }
        PixelOrder::Rgba { opaque } => {
            for px in src.chunks_exact(4) {
                let [r, g, b, a] = px else { continue };
                out.push(*r);
                out.push(*g);
                out.push(*b);
                out.push(if opaque { 0xFF } else { *a });
            }
        }
    }
}

/// CPU-import a single-plane linear dmabuf into row-major RGBA.
///
/// `out` is cleared and refilled. Returns the buffer's `(width, height)`.
///
/// A **CPU mmap import**, not zero-copy GPU: the plane fd is mapped
/// read-only, `DMA_BUF_IOCTL_SYNC` brackets the read for cache handling,
/// rows swizzle into RGBA — correct for every advertised buffer (LINEAR only), at the cost of one full-frame copy.
/// Cheap, allocation-free "can this compositor turn that buffer into tile pixels?" check — format, modifier and plane count only.
///
/// Split out from [`import_dmabuf_rgba`] so the `zwp_linux_dmabuf_v1`
/// import gate can answer *at negotiation time* — rejecting makes `create` fail (renegotiate/fall back to `wl_shm`) instead of failing at commit forever.
///
/// # Errors
///
/// Returns [`DmabufImportError`] when `dmabuf`'s modifier isn't CPU-readable
/// (`NonLinearModifier`), it has more than one plane (`MultiPlanar`), or its
/// `FourCC` isn't one of the RGBA-family formats this compositor swizzles (`UnsupportedFormat`).
pub fn validate_importable(dmabuf: &Dmabuf) -> Result<(), DmabufImportError> {
    let format = dmabuf.format();
    if !modifier_is_cpu_readable(format.modifier) {
        return Err(DmabufImportError::NonLinearModifier(u64::from(
            format.modifier,
        )));
    }
    let planes = dmabuf.num_planes();
    if planes != 1 {
        return Err(DmabufImportError::MultiPlanar(planes));
    }
    PixelOrder::for_fourcc(format.code).ok_or(DmabufImportError::UnsupportedFormat(format.code))?;
    Ok(())
}

/// CPU-import `dmabuf` into `out` as row-major RGBA, returning its `(width, height)`.
///
/// # Errors
///
/// Returns [`DmabufImportError`] for the same reasons as
/// [`validate_importable`], plus degenerate width/height/stride
/// (`InvalidDimensions`) or a failed `mmap` (`MapFailed`).
pub fn import_dmabuf_rgba(
    dmabuf: &Dmabuf,
    out: &mut Vec<u8>,
) -> Result<(u32, u32), DmabufImportError> {
    validate_importable(dmabuf)?;
    let format = dmabuf.format();
    let order = PixelOrder::for_fourcc(format.code)
        .ok_or(DmabufImportError::UnsupportedFormat(format.code))?;

    let width = dmabuf.width();
    let height = dmabuf.height();
    let stride = dmabuf.strides().next().unwrap_or(0);
    let offset = dmabuf.offsets().next().unwrap_or(0);
    if width == 0 || height == 0 || stride < width.saturating_mul(4) {
        return Err(DmabufImportError::InvalidDimensions {
            width,
            height,
            stride,
        });
    }
    let map_len = (offset as usize)
        .checked_add((stride as usize).saturating_mul(height as usize))
        .ok_or(DmabufImportError::InvalidDimensions {
            width,
            height,
            stride,
        })?;

    let fd = dmabuf
        .handles()
        .next()
        .ok_or(DmabufImportError::MultiPlanar(0))?;
    let raw_fd = fd.as_raw_fd();

    // SAFETY: `raw_fd` is a live dma-buf fd owned by the `Dmabuf` (which
    // outlives this call). `mmap` with a null hint and `MAP_SHARED` is
    // the documented way to CPU-access a dma-buf; failure is reported
    // through `MAP_FAILED` and converted into an error below.
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            map_len,
            libc::PROT_READ,
            libc::MAP_SHARED,
            raw_fd,
            0,
        )
    };
    if ptr == libc::MAP_FAILED {
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        return Err(DmabufImportError::MapFailed(errno));
    }

    // Bracket the CPU read with DMA_BUF_IOCTL_SYNC; a driver without the ioctl returns an error we ignore (mapping stays readable, no cache hint).
    sync_dmabuf(raw_fd, DMA_BUF_SYNC_START | DMA_BUF_SYNC_READ);

    out.clear();
    out.reserve(
        (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(4),
    );
    let row_bytes = (width as usize).saturating_mul(4);
    // SAFETY: the mapping covers `[0, map_len)`; every row slice below is
    // inside `[offset, offset + stride*height) ⊆ [0, map_len)`, checked
    // above. The slice is dropped before `munmap`.
    let mapped: &[u8] = unsafe { std::slice::from_raw_parts(ptr as *const u8, map_len) };
    for row in 0..height as usize {
        let start = (offset as usize).saturating_add(row.saturating_mul(stride as usize));
        let end = start.saturating_add(row_bytes);
        // Proven in range by the `stride >= width*4` and `offset +
        // stride*height == map_len` checks above; `.get()` and a skip
        // keep this panic-free if that invariant is ever broken instead.
        let Some(row_slice) = mapped.get(start..end) else {
            continue;
        };
        swizzle_row(order, row_slice, width as usize, out);
    }

    sync_dmabuf(raw_fd, DMA_BUF_SYNC_END | DMA_BUF_SYNC_READ);
    // SAFETY: `ptr`/`map_len` are exactly what `mmap` returned above and
    // `mapped` is out of scope for any further use.
    unsafe {
        libc::munmap(ptr, map_len);
    }
    Ok((width, height))
}

fn sync_dmabuf(fd: libc::c_int, flags: u64) {
    #[repr(C)]
    struct DmaBufSync {
        flags: u64,
    }
    let arg = DmaBufSync { flags };
    // SAFETY: `DMA_BUF_IOCTL_SYNC` takes a pointer to a single `__u64`
    // flags word; `arg` matches that layout and outlives the call.
    unsafe {
        libc::ioctl(fd, DMA_BUF_IOCTL_SYNC, &raw const arg);
    }
}

// Frame store

/// Per-surface capture + placement state.
struct ForeignEntry {
    /// Most recent frame, row-major RGBA (`width*height*4` bytes) — reused across commits so a steady-state client costs no reallocation.
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    /// Monotonic frame counter (`0` = no frame yet), bumped on capture *and* unmap so a sequence comparison always notices a transition.
    seq: u64,
    /// Human-readable reason the last capture failed, drawn on the tile so an unsupported buffer is visible, not silently blank.
    last_error: Option<String>,
    /// The tab's on-screen rectangle in physical pixels, published by `apply_layout`; `None` until laid out.
    placement: Option<Rect>,
    /// Size most recently sent to the client in an `xdg_toplevel` configure, in surface-local (logical) pixels.
    configured: Option<(i32, i32)>,
    /// When this surface was first heard of (toplevel adoption) — start of the window [`ForeignFrameStore::mark_presented`] waits before giving up.
    first_seen: Instant,
    /// Latched: this surface has produced something worth showing, so its
    /// window is on screen and stays there. One-way on purpose: the
    /// predicate asks whether the current frame fits its tab, which
    /// briefly stops being true on every resize — un-presenting would blink the window out mid-drag, worse than what this prevents.
    presented: bool,
}

impl ForeignEntry {
    fn new() -> Self {
        Self {
            rgba: Vec::new(),
            width: 0,
            height: 0,
            seq: 0,
            last_error: None,
            placement: None,
            configured: None,
            first_seen: Instant::now(),
            presented: false,
        }
    }
}

/// A copy of a surface's most recent frame, handed to the render side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameSnapshot {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub seq: u64,
}

/// Process-wide capture + placement registry for foreign surfaces.
///
/// A singleton rather than a field threaded through the widget tree,
/// since `crate::shell::to_ui_tile` is reached from call sites with no
/// compositor context. Keying on the wayland protocol id keeps state trivially partitioned per surface.
#[derive(Default)]
pub struct ForeignFrameStore {
    entries: Mutex<HashMap<SurfaceId, ForeignEntry>>,
    /// Total buffer imports performed, of any route. A dmabuf import costs
    /// a GPU readback (or full-frame CPU map), so it must happen **once
    /// per client commit**, never once per paint. Asserted on by `painting_does_not_reimport_the_buffer`.
    imports: std::sync::atomic::AtomicU64,
}

static STORE: OnceLock<ForeignFrameStore> = OnceLock::new();

/// The process-wide [`ForeignFrameStore`].
pub fn store() -> &'static ForeignFrameStore {
    STORE.get_or_init(ForeignFrameStore::default)
}

/// How a dmabuf's bytes were obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmabufRoute {
    /// EGL imported the buffer as a GL texture and the GPU read it back — works for every GPU-samplable buffer, including `wgpu`'s swapchain images.
    Gpu,
    /// The plane fd was CPU-mapped directly — only linear, CPU-visible buffers, but needs no GL context.
    CpuMmap,
}

/// Which capture route a commit took, for logging + tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureOutcome {
    /// `wl_shm` pool readback.
    Shm { width: u32, height: u32 },
    /// dmabuf import — see [`DmabufRoute`] for how.
    Dmabuf {
        width: u32,
        height: u32,
        route: DmabufRoute,
    },
    /// Neither path could produce pixels; the reason is recorded on the entry and drawn on the tile.
    Failed(String),
}

impl ForeignFrameStore {
    fn with_entry<R>(&self, id: SurfaceId, f: impl FnOnce(&mut ForeignEntry) -> R) -> R {
        let mut guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(guard.entry(id).or_insert_with(ForeignEntry::new))
    }

    fn note_import(&self) {
        self.imports
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Buffer imports since process start, across every surface/route — read by the repaint-cost test (see the field's doc for why it must not track paints).
    pub fn import_count(&self) -> u64 {
        self.imports.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Capture the buffer a client just committed. Tries `wl_shm` readback
    /// first; a not-shm buffer routes to the dmabuf importer. The `wl_buffer → Dmabuf` lookup is done by the caller-visible [`smithay::wayland::dmabuf::get_dmabuf`], keeping this a single entry point.
    pub fn capture_commit(&self, id: SurfaceId, buffer: &WlBuffer) -> CaptureOutcome {
        let shm_result = self.with_entry(id, |e| {
            let r = buffer_to_rgba(buffer, &mut e.rgba);
            match r {
                Ok((w, h)) => {
                    e.width = w;
                    e.height = h;
                    e.seq = e.seq.wrapping_add(1);
                    e.last_error = None;
                    Ok((w, h))
                }
                Err(err) => {
                    // Leave the previous frame intact — the dmabuf branch below may still succeed; a genuine failure clears it via `note_failure`.
                    Err(err)
                }
            }
        });
        match shm_result {
            Ok((w, h)) => {
                self.note_import();
                CaptureOutcome::Shm {
                    width: w,
                    height: h,
                }
            }
            Err(ReadbackError::DmaBufUnsupported) => {
                if let Ok(dmabuf) = smithay::wayland::dmabuf::get_dmabuf(buffer) {
                    self.capture_dmabuf(id, dmabuf)
                } else {
                    let msg = "buffer is neither wl_shm nor dmabuf".to_string();
                    self.note_failure(id, msg.clone());
                    CaptureOutcome::Failed(msg)
                }
            }
            Err(err) => {
                let msg = err.to_string();
                self.note_failure(id, msg.clone());
                CaptureOutcome::Failed(msg)
            }
        }
    }

    /// Import an already-resolved [`Dmabuf`] into the store. Two routes, tried in order:
    ///
    /// 1. **GPU** ([`crate::compositor::gpu_import`]) — EGL wraps the
    ///    buffer as a texture; the only route for a Vulkan client, since
    ///    `wgpu`'s device-local swapchain images refuse `mmap` (`EPERM`) however linear.
    /// 2. **CPU mmap** ([`import_dmabuf_rgba`]) — no GL context needed;
    ///    correct for CPU-visible linear buffers, and the route with no usable EGL device.
    ///
    /// Both failing is reported on the tile with both routes' reasons.
    pub fn capture_dmabuf(&self, id: SurfaceId, dmabuf: &Dmabuf) -> CaptureOutcome {
        let gpu_err = match crate::compositor::render_node::advertised() {
            Some(node) => {
                let attempt = crate::compositor::gpu_import::with_importer(node, |imp| {
                    self.with_entry(id, |e| {
                        imp.import(dmabuf, &mut e.rgba).map(|(w, h)| {
                            e.width = w;
                            e.height = h;
                            e.seq = e.seq.wrapping_add(1);
                            e.last_error = None;
                            (w, h)
                        })
                    })
                });
                match attempt {
                    Ok(Ok((w, h))) => {
                        self.note_import();
                        return CaptureOutcome::Dmabuf {
                            width: w,
                            height: h,
                            route: DmabufRoute::Gpu,
                        };
                    }
                    // Either the import itself failed for this buffer, or there is no GPU import context at all.
                    Ok(Err(e)) | Err(e) => Some(e.to_string()),
                }
            }
            None => Some("no dmabuf main device was advertised".to_string()),
        };

        let result = self.with_entry(id, |e| {
            import_dmabuf_rgba(dmabuf, &mut e.rgba).map(|(w, h)| {
                e.width = w;
                e.height = h;
                e.seq = e.seq.wrapping_add(1);
                e.last_error = None;
                (w, h)
            })
        });
        match result {
            Ok((w, h)) => {
                self.note_import();
                CaptureOutcome::Dmabuf {
                    width: w,
                    height: h,
                    route: DmabufRoute::CpuMmap,
                }
            }
            Err(cpu_err) => {
                let msg = match gpu_err {
                    Some(g) => format!("{cpu_err}; GPU import also failed: {g}"),
                    None => cpu_err.to_string(),
                };
                self.note_failure(id, msg.clone());
                CaptureOutcome::Failed(msg)
            }
        }
    }

    /// Publish an RGBA frame directly — used by callers that already hold decoded pixels (tests, future non-`wl_buffer` capture routes).
    pub fn publish_rgba(&self, id: SurfaceId, rgba: &[u8], width: u32, height: u32) {
        self.with_entry(id, |e| {
            e.rgba.clear();
            e.rgba.extend_from_slice(rgba);
            e.width = width;
            e.height = height;
            e.seq = e.seq.wrapping_add(1);
            e.last_error = None;
        });
    }

    /// Record that the last capture failed and drop the stale frame.
    ///
    /// Two things read it: the window stops waiting (shown at once rather
    /// than held back for a frame that isn't coming), and [`ForeignSurfaceWidget::unshowable_text`] chooses the words shown, leaving `reason` here for debugging.
    pub fn note_failure(&self, id: SurfaceId, reason: String) {
        self.with_entry(id, |e| {
            e.rgba.clear();
            e.rgba.shrink_to_fit();
            e.width = 0;
            e.height = 0;
            e.seq = e.seq.wrapping_add(1);
            e.last_error = Some(reason);
        });
    }

    /// The client detached its buffer (`attach(NULL)`): drop the frame but keep placement bookkeeping (the tab exists until the toplevel is destroyed).
    pub fn clear_frame(&self, id: SurfaceId) {
        self.with_entry(id, |e| {
            e.rgba.clear();
            e.rgba.shrink_to_fit();
            e.width = 0;
            e.height = 0;
            e.seq = e.seq.wrapping_add(1);
            e.last_error = None;
        });
    }

    /// Drop everything tracked for `id` on toplevel destroy — without this the frame `Vec` (a full framebuffer) would outlive the client.
    pub fn forget(&self, id: SurfaceId) {
        let mut guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.remove(&id);
    }

    /// Current frame counter for `id`; `0` when nothing has landed.
    pub fn frame_seq(&self, id: SurfaceId) -> u64 {
        let guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.get(&id).map_or(0, |e| e.seq)
    }

    /// Copy out the current frame, or `None` when the surface has no pixels staged.
    pub fn snapshot(&self, id: SurfaceId) -> Option<FrameSnapshot> {
        let guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let e = guard.get(&id)?;
        let snapshot = if e.rgba.is_empty() || e.width == 0 || e.height == 0 {
            None
        } else {
            Some(FrameSnapshot {
                rgba: e.rgba.clone(),
                width: e.width,
                height: e.height,
                seq: e.seq,
            })
        };
        drop(guard);
        snapshot
    }

    /// Dimensions of the frame currently staged for `id`, without copying
    /// it; `None` when nothing has landed. Exists so a harness can compare
    /// what the client drew against the tab it was drawn into — disagreeing is what a visibly resizing window *is*, unlike a frame counter alone.
    pub fn frame_size(&self, id: SurfaceId) -> Option<(u32, u32)> {
        let guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let e = guard.get(&id)?;
        let size = (e.width > 0 && e.height > 0).then_some((e.width, e.height));
        drop(guard);
        size
    }

    /// Whether `id`'s window has been shown — the latched answer from [`Self::mark_presented`], read without changing it.
    pub fn is_presented(&self, id: SurfaceId) -> bool {
        let guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.get(&id).is_some_and(|e| e.presented)
    }

    /// Whether the tile standing in for `id` should say its app is on its
    /// way: nothing presented, waited longer than [`OPENING_DELAY`]. Read
    /// by [`ForeignSurfaceWidget::opening`] before painting the
    /// placeholder card; `presented` wins, so this goes false and stays false — a watched-away placeholder must not return.
    pub fn is_opening(&self, id: SurfaceId) -> bool {
        let guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard
            .get(&id)
            .is_some_and(|e| !e.presented && e.first_seen.elapsed() >= OPENING_DELAY)
    }

    /// Test-only: pretend this surface was first heard of `age` ago, reaching past a delay without sleeping; creates the entry if absent.
    ///
    /// # Panics
    ///
    /// Panics if `age` is larger than the time elapsed since the process's
    /// monotonic clock started — never true for the small, fixed durations tests pass.
    #[cfg(test)]
    pub fn backdate_first_seen(&self, id: SurfaceId, age: Duration) {
        self.with_entry(id, |e| {
            e.first_seen = Instant::now()
                .checked_sub(age)
                .expect("age is a test constant, well inside the clock's range");
        });
    }

    /// Open this surface's entry at the moment its toplevel is adopted, so
    /// [`ForeignEntry::first_seen`] dates from being asked for, not its first buffer.
    ///
    /// Both waits this store bounds — the opening configure in
    /// [`flush_configures`] and the presentation grace in
    /// [`Self::mark_presented`] — run from that instant.
    pub fn note_adopted(&self, id: SurfaceId) {
        self.with_entry(id, |_| {});
    }

    /// Whether this surface has waited longer than
    /// [`OPENING_CONFIGURE_GRACE`] since adopted. Read only by
    /// [`flush_configures`], to decide when to configure anyway; no entry means not adopted, not waiting on anything.
    pub fn opening_wait_expired(&self, id: SurfaceId) -> bool {
        let guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard
            .get(&id)
            .is_some_and(|e| e.first_seen.elapsed() >= OPENING_CONFIGURE_GRACE)
    }

    /// Reason the last capture failed, if any.
    pub fn last_error(&self, id: SurfaceId) -> Option<String> {
        let guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.get(&id).and_then(|e| e.last_error.clone())
    }

    /// Record the tab rectangle the surface is laid into, in physical pixels — called from `apply_layout`, the authority on where a tab lives.
    pub fn note_placement(&self, id: SurfaceId, rect: Rect) {
        self.with_entry(id, |e| {
            e.placement = Some(rect);
        });
    }

    /// The tab rectangle last published by [`Self::note_placement`].
    pub fn placement(&self, id: SurfaceId) -> Option<Rect> {
        let guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.get(&id).and_then(|e| e.placement)
    }

    /// Size of the tab rectangle in physical pixels.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to [0.0, u32::MAX] and .round()-ed first, so the casts to u32 below \
                  neither truncate a fraction nor lose a sign"
    )]
    pub fn viewport(&self, id: SurfaceId) -> Option<(u32, u32)> {
        self.placement(id).map(|r| {
            (
                r.width().round().clamp(0.0, f64::from(u32::MAX)) as u32,
                r.height().round().clamp(0.0, f64::from(u32::MAX)) as u32,
            )
        })
    }

    /// The size the client should be configured to, in surface-local (logical)
    /// pixels, **iff** it differs from the last sent — records the value as sent so an unchanged layout returns `None`.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "every cast to u32/i32 below is preceded by a .clamp(..) into that type's \
                  range and a .round(), so none truncates a fraction, loses a sign, or wraps \
                  out of range"
    )]
    pub fn take_pending_configure(&self, id: SurfaceId, scale: f64) -> Option<(i32, i32)> {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        self.with_entry(id, |e| {
            let rect = e.placement?;
            let w_px = rect.width().round().clamp(0.0, f64::from(u32::MAX)) as u32;
            let h_px = rect.height().round().clamp(0.0, f64::from(u32::MAX)) as u32;
            if w_px == 0 || h_px == 0 {
                return None;
            }
            let logical = (
                (f64::from(w_px) / scale)
                    .round()
                    .clamp(1.0, f64::from(i32::MAX)) as i32,
                (f64::from(h_px) / scale)
                    .round()
                    .clamp(1.0, f64::from(i32::MAX)) as i32,
            );
            if e.configured == Some(logical) {
                return None;
            }
            e.configured = Some(logical);
            Some(logical)
        })
    }

    /// Whether this surface has something worth putting on screen, and
    /// latch the answer once it does. Read by
    /// [`ForeignSurfaceWidget::presented`], the predicate deciding when a launched window turns up.
    ///
    /// Presentable when: a **capture failed** (must say so, not be
    /// invisible); a **frame answers the tab** per [`fills`] (else the
    /// unasked-for buffer size scales via `fit_rect`, snapping to 1:1
    /// later); or **[`PRESENT_GRACE`] passes** (backstop against an unclosable window). `tile` degenerate means not laid out, unjudgeable.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "on the enclosing function because #[allow] on the tracing::warn! invocation \
                  itself is silently ignored by rustc: the tab_w/tab_h fields it logs are each \
                  clamped to [0.0, u32::MAX] and .round()-ed first, so neither truncates a \
                  fraction nor loses a sign"
    )]
    pub fn mark_presented(&self, id: SurfaceId, tile: Rect) -> bool {
        self.with_entry(id, |e| {
            if e.presented {
                return true;
            }
            let waited = e.first_seen.elapsed();
            let backstop = waited >= PRESENT_GRACE;
            if e.last_error.is_none() && !fills(e.width, e.height, tile) && !backstop {
                return false;
            }
            e.presented = true;
            // Reaching the backstop is a defect every time: the window was a hole in the strip for the whole grace period.
            if backstop {
                tracing::warn!(
                    target: "shell::compositor::foreign",
                    surface_id = id,
                    waited_ms = u64::try_from(waited.as_millis()).unwrap_or(u64::MAX),
                    buffer_w = e.width,
                    buffer_h = e.height,
                    tab_w = tile.width().round().clamp(0.0, f64::from(u32::MAX)) as u32,
                    tab_h = tile.height().round().clamp(0.0, f64::from(u32::MAX)) as u32,
                    "no frame answered this tab; showing the window anyway",
                );
            }
            true
        })
    }
}

/// How long a window's place is held in silence before the card saying
/// what's coming is put there.
///
/// An app already on this machine can be on screen well inside a quarter
/// second, and a briefer flicker is noise, not information; past it, the wait is long enough to read as one.
pub(crate) const OPENING_DELAY: Duration = Duration::from_millis(250);

/// How long a surface may take to produce a frame that fits its tab
/// before its window is shown regardless.
///
/// Sized against real cost: a cold open runs 2-6s from invocation to
/// first buffer. Ten seconds is past every launch observed, and short enough that a never-painting app stops being a hole in the strip.
pub(crate) const PRESENT_GRACE: Duration = Duration::from_secs(10);

/// How long a newly adopted toplevel may go without its tab being laid
/// out before the compositor configures it anyway.
///
/// The ordinary case doesn't spend this: the tab splices in at adoption,
/// layout gives it a rect, and [`flush_configures`] answers next frame.
///
/// What this bounds is when no rect ever arrives — a never-configured client can't map, so waiting forever is worse than a wrong-sized frame.
const OPENING_CONFIGURE_GRACE: Duration = Duration::from_millis(250);

/// Whether a `w`x`h` buffer is this client's answer to the tab it was
/// configured into, rather than the size it picked before being asked.
///
/// Two ways to be an answer: it **covers the tab** ([`fit_rect`] scales
/// uniformly by the smaller ratio, so aspect match means exact cover),
/// or it **fits inside and matches one axis** (a client keeping a title
/// bar out of its given size answers one axis and falls short forever,
/// e.g. configured 670x668, commits 670x633 — covering alone would wait
/// the full [`PRESENT_GRACE`]). Backstops undecorated/self-decorating clients; overflowing either axis still disqualifies.
///
/// The half-percent tolerance absorbs logical-size rounding.
fn fills(w: u32, h: u32, tile: Rect) -> bool {
    if w == 0 || h == 0 || tile.width() <= 0.0 || tile.height() <= 0.0 {
        return false;
    }
    let (bw, bh) = (f64::from(w), f64::from(h));
    let scale = (tile.width() / bw).min(tile.height() / bh);
    let at_least = |a: f64, b: f64| a >= b * (1.0 - FIT_TOLERANCE);
    let at_most = |a: f64, b: f64| a <= b * (1.0 + FIT_TOLERANCE);
    if at_least(bw * scale, tile.width()) && at_least(bh * scale, tile.height()) {
        return true;
    }
    at_most(bw, tile.width())
        && at_most(bh, tile.height())
        && (at_least(bw, tile.width()) || at_least(bh, tile.height()))
}

/// How far a buffer may disagree with its tab and still be the same size. See [`fills`].
const FIT_TOLERANCE: f64 = 0.005;

// The widget

/// Hosts one foreign wayland surface inside a tab. Holds no pixel state
/// beyond a cached [`ImageData`] built from the store's most recent
/// frame, rebuilt only when the sequence number moves — a static client costs nothing per frame beyond the fill.
pub struct ForeignSurfaceWidget {
    surface_id: SurfaceId,
    app_id: String,
    rect: Rect,
    /// GPU-uploadable copy of the last frame we drew, plus the store sequence it came from.
    cached: Option<ImageData>,
    cached_seq: u64,
    /// Stable, human-addressable name so `FrameBuilder::widget_registry` (and the shell's click/rect resolver) can find the tile.
    test_id: String,
    /// The app's own name, resolved on the frame that first needs it.
    ///
    /// Only the placeholder below reads it — the rare path where an app
    /// never showed a usable frame. The lookup consults launches then installed manifests, so it's not repeated on every rebuild.
    name: std::cell::OnceCell<String>,
}

impl ForeignSurfaceWidget {
    pub fn new(surface_id: SurfaceId, app_id: String) -> Self {
        Self {
            surface_id,
            app_id,
            rect: Rect::ZERO,
            cached: None,
            cached_seq: 0,
            test_id: format!("foreign-surface-{surface_id}"),
            name: std::cell::OnceCell::new(),
        }
    }

    /// The app's name, as the tab above this surface says it.
    fn display_name(&self) -> &str {
        self.name
            .get_or_init(|| crate::compositor::tab_binding::tab_label_for_foreign(&self.app_id))
    }

    /// What a window says when it has gone up with nothing in it.
    ///
    /// Reached two ways, both the end of somebody's wait: a capture
    /// failed, or the app got every second it could and still never fit
    /// its tab — either way it must be a visible end, not a window that quietly stays blank or disappears.
    ///
    /// So it names the app and says what happened, in plain words; why a
    /// *buffer* couldn't be shown stays in [`ForeignFrameStore::last_error`] for whoever is debugging, not on this card.
    fn unshowable_text(&self) -> (String, String) {
        let detail = match store().last_error(self.surface_id) {
            Some(_) => "This window can't be shown.",
            None => "This app isn't responding.",
        };
        (self.display_name().to_string(), detail.to_string())
    }

    pub fn surface_id(&self) -> SurfaceId {
        self.surface_id
    }

    /// Place a compositor-owned floating surface without putting it through
    /// the workspace's Taffy tree. The same placement store still drives the
    /// xdg configure handshake, so the client renders at this exact size.
    pub fn place_floating(&mut self, rect: Rect) {
        self.rect = rect;
        if rect.width() > 0.0 && rect.height() > 0.0 {
            store().note_placement(self.surface_id, rect);
        }
    }

    /// Whether the client currently has pixels mapped. Unlike ordinary tabs,
    /// an inactive OSK must disappear completely rather than show an
    /// "unresponsive" placeholder while its toplevel is intentionally
    /// unmapped.
    pub fn is_mapped(&self) -> bool {
        store().frame_size(self.surface_id).is_some()
    }

    /// Pull a fresh frame out of the store if one has landed since the last call; returns `true` when the cache changed.
    fn refresh_cache(&mut self) -> bool {
        let seq = store().frame_seq(self.surface_id);
        if seq == self.cached_seq {
            return false;
        }
        self.cached_seq = seq;
        match store().snapshot(self.surface_id) {
            Some(frame) => {
                self.cached = Some(ImageData {
                    data: Blob::new(std::sync::Arc::new(frame.rgba)),
                    format: ImageFormat::Rgba8,
                    // Wayland `ARGB8888` carries premultiplied alpha; `X` variants are forced opaque during the swizzle, where premultiplied and straight coincide.
                    alpha_type: ImageAlphaType::AlphaPremultiplied,
                    width: frame.width,
                    height: frame.height,
                });
            }
            None => self.cached = None,
        }
        true
    }

    /// Whether this surface's window is still being held back, and so the
    /// shell has to keep asking.
    ///
    /// The frame counter alone can't carry this: an uncommitted client
    /// leaves it at zero, matching the cache, so the strip is judged
    /// unchanged; the ten-second backstop is only read from a draw — without this the window sits invisible until an unrelated repaint.
    fn awaiting_first_paint(&self) -> bool {
        !store().is_presented(self.surface_id)
    }

    /// Destination rect for the client image inside `self.rect`, preserving
    /// aspect ratio and anchoring at the tile's top-left corner. A
    /// correctly-configured client renders at exactly the tab size (identity fit); a letterbox band only shows briefly between a resize configure and the client's answering commit.
    ///
    /// The anchor is load-bearing: pointer input reaches the client by
    /// subtracting the tile origin from the global cursor
    /// (`client_input::surface_local`), so a *centred* draw instead would slide every pixel while input kept addressing the un-slid position.
    fn fit_rect(&self, img_w: u32, img_h: u32) -> Option<Rect> {
        if img_w == 0 || img_h == 0 || self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return None;
        }
        let iw = f64::from(img_w);
        let ih = f64::from(img_h);
        let scale = (self.rect.width() / iw).min(self.rect.height() / ih);
        let dw = iw * scale;
        let dh = ih * scale;
        let dx = self.rect.x0;
        let dy = self.rect.y0;
        Some(Rect::new(dx, dy, dx + dw, dy + dh))
    }
}

/// `base` composed with a translation to `(dx, dy)`. Wrapped so the one
/// `arithmetic_side_effects` allow lives in a single place: `Affine`'s
/// `Mul` is pure floating-point matrix composition and cannot panic or
/// overflow.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine multiplication is floating-point matrix composition; it cannot panic or overflow"
)]
fn translated(base: Affine, dx: f64, dy: f64) -> Affine {
    base * Affine::translate((dx, dy))
}

/// `base` composed with a translation to `(dx, dy)` and then a non-uniform
/// scale by `(sx, sy)`. See [`translated`] for why the allow is warranted.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "Affine multiplication is floating-point matrix composition; it cannot panic or overflow"
)]
fn translated_and_scaled(base: Affine, dx: f64, dy: f64, sx: f64, sy: f64) -> Affine {
    base * Affine::translate((dx, dy)) * Affine::scale_non_uniform(sx, sy)
}

impl Widget for ForeignSurfaceWidget {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // `new_leaf` can't fail in practice for a single-node insert; a
        // dummy id here just means the fallback is unreachable by real
        // traversal instead of panicking.
        let node = taffy
            .new_leaf(taffy::Style {
                size: taffy::Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap_or(NodeId::new(0));
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(l) = taffy.layout(node) else {
            return;
        };
        let x = origin.x + f64::from(l.location.x);
        let y = origin.y + f64::from(l.location.y);
        self.rect = Rect::new(
            x,
            y,
            x + f64::from(l.size.width),
            y + f64::from(l.size.height),
        );
        // Publish the laid-out rect so the configure pass can tell the client what resolution to render at — the shell-to-wayland seam.
        if self.rect.width() > 0.0 && self.rect.height() > 0.0 {
            store().note_placement(self.surface_id, self.rect);
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.refresh_cache();
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return;
        }
        let radius = f64::from(theme.tokens.radii.xl);
        let clip = RoundedRect::from_rect(self.rect, radius);

        if let Some(image) = self.cached.clone()
            && let Some(dst) = self.fit_rect(image.width, image.height)
        {
            // Neutral backdrop so letterbox bands (and client transparency) read as compositor surface, not whatever the tile card painted.
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.base,
                None,
                &clip,
            );
            let sx = dst.width() / f64::from(image.width);
            let sy = dst.height() / f64::from(image.height);
            let brush_transform = translated_and_scaled(transform, dst.x0, dst.y0, sx, sy);
            let brush = ImageBrush::new(image.clone()).with_extend(Extend::Pad);
            scene.push_layer(
                Fill::NonZero,
                vello::peniko::BlendMode::default(),
                1.0,
                transform,
                &clip,
            );
            scene.fill(
                Fill::NonZero,
                brush_transform,
                &brush,
                None,
                &Rect::new(0.0, 0.0, f64::from(image.width), f64::from(image.height)),
            );
            scene.pop_layer();
            return;
        }

        let (headline, detail) = self.unshowable_text();
        // Fixed ramp indices were a guess about which way round the card
        // is, not an answer: `neutral[1]` is near-black on the dark ramp
        // and the card is `#16171b`, so the app's own name measured 1.03:1 and was invisible. `on_card` derives both tiers from the card's actual luminance instead.
        let tiers = theme.tokens.colors.on_card();
        let on_card = tiers.ink;
        let on_card_dim = tiers.ink_dim;
        let size = theme.tokens.typography.scale.xl;
        let (tw, _) = text_renderer.measure(&headline, size, None);
        let cx = self.rect.center().x;
        let y = self.rect.center().y - f64::from(size) / 2.0;
        text_renderer.draw_text(
            scene,
            &headline,
            size,
            on_card,
            translated(transform, cx - f64::from(tw) / 2.0, y),
            None,
        );
        let sm = theme.tokens.typography.scale.sm;
        let (dw, _) = text_renderer.measure(&detail, sm, None);
        text_renderer.draw_text(
            scene,
            &detail,
            sm,
            on_card_dim,
            translated(
                transform,
                cx - f64::from(dw) / 2.0,
                y + f64::from(size) + 8.0,
            ),
            None,
        );
    }

    fn handle_event(
        &mut self,
        _event: &WindowEvent,
        _scale_factor: f64,
        _modifiers: &Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        _handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        // Input routing to client surfaces goes through the seat, not the widget tree.
    }

    fn name(&self) -> &'static str {
        "ForeignSurfaceWidget"
    }

    /// Whether the client has drawn something that belongs on screen — the
    /// judgement lives on [`ForeignFrameStore::mark_presented`] since it must latch per surface, and this widget rebuilds whenever the tab tree does.
    fn presented(&self) -> bool {
        store().mark_presented(self.surface_id, self.rect)
    }

    /// Whether this is a window's place being held for an app asked for but not yet drawn. See [`Widget::opening`], [`ForeignFrameStore::is_opening`].
    fn opening(&self) -> bool {
        store().is_opening(self.surface_id)
    }

    fn test_id(&self) -> Option<&str> {
        Some(&self.test_id)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// A pending frame in the store is the only reason this widget needs a repaint — cheap to poll, `false` when idle, so paint-gating is unaffected.
    fn needs_animation_frame(&self) -> bool {
        self.awaiting_first_paint() || store().frame_seq(self.surface_id) != self.cached_seq
    }

    fn needs_redraw(&self) -> bool {
        self.awaiting_first_paint() || store().frame_seq(self.surface_id) != self.cached_seq
    }
}

impl AppWidget for ForeignSurfaceWidget {
    fn app_kind(&self) -> AppKind {
        // No `AppKind` describes "arbitrary external client"; `Settings` is the catch-all `to_ui_kind` already uses for unknown kinds.
        AppKind::Settings
    }
    fn app_title(&self) -> &str {
        // The app's name, not its address — this handed back the raw wayland app-id while `Self::display_name` already resolves it via the manifest.
        self.display_name()
    }
    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("󰖯")
    }
    fn permission_hint(&self) -> Option<AppKind> {
        None
    }
}

// Tree mirroring: protocol-side adoptions → rendered chrome tree

/// One foreign tile as seen in a workspace tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignTab {
    pub surface_id: SurfaceId,
    pub app_id: String,
    pub tab_id: u64,
    pub title: String,
}

/// Every `TileContent::Foreign` tab in `tree`'s active workspace, in
/// traversal order — scoped since callers ask what's on screen; use [`foreign_tabs_anywhere`] to ask whether a surface *has* a tab.
pub fn foreign_tabs(tree: &WorkspaceTree) -> Vec<ForeignTab> {
    let mut out = Vec::new();
    let Some(ws) = tree.active_workspace() else {
        return out;
    };
    collect_foreign_tabs(ws, &mut out);
    out
}

/// Every `TileContent::Foreign` tab in `tree`, across all its workspaces.
///
/// A window keeps its tab when the user walks to another workspace, and a
/// launch can place one in a workspace never made active — asking only the active workspace would report an existing window as missing.
pub fn foreign_tabs_anywhere(tree: &WorkspaceTree) -> Vec<ForeignTab> {
    let mut out = Vec::new();
    for ws in &tree.workspaces {
        collect_foreign_tabs(ws, &mut out);
    }
    out
}

fn collect_foreign_tabs(ws: &crate::shell::state::Workspace, out: &mut Vec<ForeignTab>) {
    for strip in &ws.strips {
        for group in &strip.window_groups {
            for tab in &group.tabs {
                if let ShellTile::Foreign { app_id, surface_id } = &tab.tile {
                    out.push(ForeignTab {
                        surface_id: *surface_id,
                        app_id: app_id.clone(),
                        tab_id: tab.id,
                        title: tab.title.clone(),
                    });
                }
            }
        }
    }
}

/// Whether `surface_id` is the active tab of its group in the active workspace — i.e. whether its pixels are on screen.
pub fn is_visible_tab(tree: &WorkspaceTree, surface_id: SurfaceId) -> bool {
    let Some(ws) = tree.active_workspace() else {
        return false;
    };
    for strip in &ws.strips {
        for group in &strip.window_groups {
            for tab in &group.tabs {
                if matches!(
                    &tab.tile,
                    ShellTile::Foreign { surface_id: s, .. } if *s == surface_id
                ) {
                    return group.active_tab == tab.id;
                }
            }
        }
    }
    false
}

/// Reconcile the set of foreign tabs in `rendered` against `source`.
///
/// The compositor's protocol state and the chrome's rendered tree are
/// separate — the chrome owns its own `WorkspaceTreeHandle` so widget
/// state survives protocol churn. This copies foreign adoptions and teardowns across, the only direction data flows.
///
/// Returns `true` when `rendered` was mutated, signalling the caller to bump the chrome revision for strip resync.
pub fn reconcile_foreign_tabs(source: &WorkspaceTree, rendered: &mut WorkspaceTree) -> bool {
    // Both sides are read across every workspace — the two trees move
    // their active pointers independently, so comparing only active workspaces made the mirror re-adopt a window the user walked away from.
    let wanted = foreign_tabs_anywhere(source);
    let present = foreign_tabs_anywhere(rendered);
    let mut changed = false;

    // Remove foreign tabs the protocol side no longer has.
    let wanted_ids: std::collections::HashSet<SurfaceId> =
        wanted.iter().map(|t| t.surface_id).collect();
    for gone in present
        .iter()
        .filter(|t| !wanted_ids.contains(&t.surface_id))
    {
        if crate::compositor::tab_binding::close_tab_for_destroyed_surface(
            rendered,
            gone.surface_id,
        ) {
            changed = true;
        }
    }

    // Collapse any surface with more than one tab: a session saved before
    // this mirror compared whole trees carries duplicates on disk, so preventing new ones isn't enough — reopening must come back with one per window.
    if drop_duplicate_foreign_tabs(rendered) {
        changed = true;
    }

    // Add foreign tabs the rendered tree is missing via `adopt_toplevel`, the same production splice the protocol side uses.
    let present_ids: std::collections::HashSet<SurfaceId> = foreign_tabs_anywhere(rendered)
        .iter()
        .map(|t| t.surface_id)
        .collect();
    for add in wanted
        .iter()
        .filter(|t| !present_ids.contains(&t.surface_id))
    {
        crate::compositor::tab_binding::adopt_toplevel(
            rendered,
            add.surface_id,
            Some(add.app_id.clone()),
        );
        changed = true;
    }

    // Keep titles/app-ids in step for tabs on both sides (a client can call `set_app_id` after mapping).
    for want in &wanted {
        for ws in &mut rendered.workspaces {
            for strip in &mut ws.strips {
                for group in &mut strip.window_groups {
                    for tab in &mut group.tabs {
                        if let ShellTile::Foreign { app_id, surface_id } = &mut tab.tile
                            && *surface_id == want.surface_id
                            && *app_id != want.app_id
                        {
                            want.app_id.clone_into(app_id);
                            want.title.clone_into(&mut tab.title);
                            changed = true;
                        }
                    }
                }
            }
        }
    }

    changed
}

/// Keep the first tab showing each surface and remove the rest.
///
/// One window is one tab. A second tab onto the same surface draws the
/// client twice, offers it two sets of configures, and outlives it — a destroyed surface closes one tab, leaving the other pointing at nothing.
///
/// A session saved while the mirror still compared active workspaces
/// carries duplicates on disk, so this runs every reconcile, not only at adoption. Returns whether anything was removed.
fn drop_duplicate_foreign_tabs(tree: &mut WorkspaceTree) -> bool {
    let mut seen: std::collections::HashSet<SurfaceId> = std::collections::HashSet::new();
    let mut removed = false;
    for ws in &mut tree.workspaces {
        for strip in &mut ws.strips {
            for group in &mut strip.window_groups {
                let active = group.active_tab;
                let mut dropped_active = false;
                group.tabs.retain(|tab| {
                    let ShellTile::Foreign { surface_id, .. } = &tab.tile else {
                        return true;
                    };
                    if seen.insert(*surface_id) {
                        return true;
                    }
                    dropped_active |= active == tab.id;
                    removed = true;
                    false
                });
                // The duplicate may have been the tab the group was showing — fall back the way a closed tab does.
                if dropped_active {
                    group.active_tab = group.tabs.last().map_or(0, |t| t.id);
                }
            }
        }
    }
    removed
}

// Per-frame compositor passes

/// Tell every foreign client what pixel size to render at.
///
/// Reads the tab rects published during layout and sends
/// `xdg_toplevel.configure` to any client whose size changed, converted
/// to surface-local logical pixels at the compositor's current scale (the same value pushed via `wp_fractional_scale_v1`).
///
/// Also where a toplevel gets its **opening** configure — run here, not
/// at `get_toplevel`, because a tab's size isn't known until layout; an
/// earlier configure carries no size, letting the client draw whatever it likes before being corrected.
///
/// Returns the number of configures sent — used by the wiring test.
pub fn flush_configures(state: &mut crate::compositor::state::State) -> usize {
    let scale = state.scale;
    let fallback = opening_fallback_size(state);
    // `toplevel_surfaces` borrows the shell state; a cheap clone releases the borrow before the configure calls below, which don't touch `State`.
    let toplevels: Vec<_> = state.xdg_shell_state.toplevel_surfaces().to_vec();
    let mut sent: usize = 0;
    for toplevel in toplevels {
        let id = crate::compositor::state::surface_id(toplevel.wl_surface());
        if let Some((w, h)) = store().take_pending_configure(id, scale) {
            configure_to(&toplevel, w, h);
            sent = sent.saturating_add(1);
            tracing::debug!(
                target: "shell::compositor::foreign",
                surface_id = id,
                width = w,
                height = h,
                "configured foreign toplevel to its tab size"
            );
            continue;
        }
        // No laid-out size. Ordinary for an already-configured toplevel
        // (an unchanged layout mustn't re-configure every frame); for a
        // never-configured one, waiting is right only while the rect might still come — it can't map without ever configuring, so it can't wait forever.
        if toplevel.is_initial_configure_sent() || !store().opening_wait_expired(id) {
            continue;
        }
        let (w, h) = fallback;
        configure_to(&toplevel, w, h);
        sent = sent.saturating_add(1);
        tracing::warn!(
            target: "shell::compositor::foreign",
            surface_id = id,
            width = w,
            height = h,
            grace_ms = u64::try_from(OPENING_CONFIGURE_GRACE.as_millis()).unwrap_or(u64::MAX),
            "no tab rect for an opening toplevel; configuring it to the \
             strip's full tile so it can map at all",
        );
    }
    sent
}

/// Put `size` in the toplevel's pending state and send it — smithay
/// writes the pending state into the configure, so the size must be
/// staged before the send, letting any other pending change (decoration mode) ride the same event.
fn configure_to(toplevel: &ToplevelSurface, w: i32, h: i32) {
    toplevel.with_pending_state(|pending| {
        pending.size = Some((w, h).into());
    });
    toplevel.send_configure();
}

/// The size to configure an opening toplevel to when its tab was never
/// laid out, in surface-local (logical) pixels.
///
/// An approximation of a full-width window's tile: the output, less the
/// chrome around a strip. Not what layout would have given — it declined — but a size the window can honestly render at, close enough that a later correction is small.
fn opening_fallback_size(state: &crate::compositor::state::State) -> (i32, i32) {
    let scale = if state.scale.is_finite() && state.scale > 0.0 {
        state.scale
    } else {
        1.0
    };
    let (out_w, out_h) = state.output_size;
    #[allow(
        clippy::cast_possible_truncation,
        reason = "clamped to [1.0, i32::MAX] and .round()-ed first, so the cast to i32 never \
                  truncates a fraction or wraps out of range"
    )]
    let logical = |px: f64| (px / scale).round().clamp(1.0, f64::from(i32::MAX)) as i32;
    let chrome_w = f64::from(crate::shell::sidebar_wheel_chrome::SIDEBAR_WIDTH_PX);
    let chrome_h =
        f64::from(crate::shell::topbar_chrome::TOPBAR_HEIGHT_PX) + ui::widget::tabs::TAB_BAR_HEIGHT;
    (
        logical((f64::from(out_w) - chrome_w).max(1.0)),
        logical((f64::from(out_h) - chrome_h).max(1.0)),
    )
}

/// Release the frame throttle for foreign surfaces that are actually on screen.
///
/// Wayland clients that draw on demand wait for `wl_surface.frame` before
/// their next buffer; without this pass a client renders one frame and
/// freezes forever. Callbacks fire only for the *visible* tab of each group, also keeping a background client from burning CPU unseen.
///
/// Returns the number of surfaces signalled.
pub fn release_frame_throttle(state: &mut crate::compositor::state::State) -> usize {
    let time_ms = u32::try_from(state.start_time.elapsed().as_millis()).unwrap_or(u32::MAX);
    let toplevels: Vec<_> = state.xdg_shell_state.toplevel_surfaces().to_vec();
    let mut signalled: usize = 0;
    for toplevel in toplevels {
        let surface = toplevel.wl_surface();
        let id = crate::compositor::state::surface_id(surface);
        if !is_visible_tab(&state.workspace_tree, id) && state.osk_surface != Some(id) {
            continue;
        }
        let mut fired = false;
        smithay::wayland::compositor::with_states(surface, |states| {
            let mut attrs = states
                .cached_state
                .get::<smithay::wayland::compositor::SurfaceAttributes>();
            for callback in attrs.current().frame_callbacks.drain(..) {
                callback.done(time_ms);
                fired = true;
            }
        });
        if fired {
            signalled = signalled.saturating_add(1);
        }
    }
    signalled
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "deterministic geometry fixtures use exactly representable values"
)]
mod tests {
    use super::*;
    use crate::shell::state::WorkspaceTree;

    /// Distinct ids per test so the process-wide store can't leak state between parallel test threads.
    fn sid(n: u64) -> SurfaceId {
        900_000u64.saturating_add(n)
    }

    #[test]
    fn swizzle_row_bgra_swaps_red_and_blue() {
        // Memory bytes for DRM_FORMAT_ARGB8888 are [B, G, R, A].
        let src = [10u8, 20, 30, 200, 1, 2, 3, 4];
        let mut out = Vec::new();
        swizzle_row(PixelOrder::Bgra { opaque: false }, &src, 2, &mut out);
        assert_eq!(out, vec![30, 20, 10, 200, 3, 2, 1, 4]);
    }

    #[test]
    fn swizzle_row_opaque_variant_forces_alpha() {
        let src = [10u8, 20, 30, 0x00];
        let mut out = Vec::new();
        swizzle_row(PixelOrder::Bgra { opaque: true }, &src, 1, &mut out);
        assert_eq!(out, vec![30, 20, 10, 0xFF]);
    }

    #[test]
    fn swizzle_row_rgba_passes_channels_through() {
        // DRM_FORMAT_ABGR8888 is already [R, G, B, A] in memory.
        let src = [10u8, 20, 30, 40];
        let mut out = Vec::new();
        swizzle_row(PixelOrder::Rgba { opaque: false }, &src, 1, &mut out);
        assert_eq!(out, vec![10, 20, 30, 40]);
    }

    #[test]
    fn pixel_order_rejects_non_rgba_fourcc() {
        assert!(PixelOrder::for_fourcc(Fourcc::Nv12).is_none());
        assert!(PixelOrder::for_fourcc(Fourcc::Argb8888).is_some());
    }

    /// A tab is 1200x800. These are the sizes a client hands us.
    const TILE: Rect = Rect::new(0.0, 0.0, 1200.0, 800.0);

    /// An app already on this machine can be on screen almost at once, so saying "on its way" that fast would flicker before anyone reads it.
    #[test]
    fn a_surface_just_heard_of_is_not_yet_worth_saying_anything_about() {
        let id = sid(50);
        store().forget(id);
        store().note_placement(id, TILE);
        assert!(
            !store().is_opening(id),
            "an app that may still open in a blink must not flash an \
             indicator on its way past",
        );
        store().forget(id);
    }

    /// Past the delay the wait is a wait, and an empty space where a window was asked for reads as nothing having happened.
    #[test]
    fn a_surface_still_starting_after_the_delay_says_so() {
        let id = sid(51);
        store().forget(id);
        store().backdate_first_seen(id, OPENING_DELAY + Duration::from_millis(50));
        assert!(
            store().is_opening(id),
            "the person is watching an empty desktop for a window they \
             asked for, and nothing on screen accounts for it",
        );
        store().forget(id);
    }

    /// And it stops the instant there is a window, without coming back —
    /// a placeholder someone has watched go away must not return, worse to see than the wait it was covering.
    #[test]
    fn a_surface_that_has_been_shown_is_never_opening_again() {
        let id = sid(52);
        store().forget(id);
        store().backdate_first_seen(id, OPENING_DELAY + Duration::from_millis(50));
        assert!(
            store().is_opening(id),
            "precondition: it was still starting"
        );
        store().publish_rgba(id, &vec![0; 1200 * 800 * 4], 1200, 800);
        assert!(
            store().mark_presented(id, TILE),
            "precondition: it presented"
        );
        assert!(
            !store().is_opening(id),
            "the window is on screen; nothing may still be saying it is on \
             its way",
        );
        store().forget(id);
    }

    /// What a person reads on a window's own card is the app's name, never the wayland app-id — a machine identifier the user never chose.
    #[test]
    fn a_card_in_a_windows_place_names_the_app_not_its_id() {
        let w = ForeignSurfaceWidget::new(sid(53), "dev.qos.never-launched-fixture".into());
        let name = w.display_name();
        assert_eq!(
            name, "Never Launched Fixture",
            "a window's card must carry the app's name; got {name:?}",
        );
        assert!(
            !name.contains("dev.qos."),
            "a reverse-DNS package id reached a card somebody is reading",
        );
    }

    /// A window that has gone up with nothing in it says so, in words a
    /// person would use.
    ///
    /// This is the last thing between somebody and a window that will
    /// never fill in, so it names the app and says what happened —
    /// without the vocabulary of what went wrong. "waiting for first frame" over a package id taught nobody which app it was, or whether to keep waiting.
    #[test]
    fn a_window_that_cannot_be_shown_says_so_in_plain_words() {
        let id = sid(54);
        store().forget(id);
        let w = ForeignSurfaceWidget::new(id, "dev.qos.never-launched-fixture".into());

        let (headline, detail) = w.unshowable_text();
        assert_eq!(headline, "Never Launched Fixture", "the app's own name");
        assert_eq!(detail, "This app isn't responding.");

        // A capture that failed is a different thing to say, and the reason it failed is still not the person's problem to read.
        store().note_failure(id, "unsupported dmabuf modifier 0x0100000000000001".into());
        let (headline, detail) = w.unshowable_text();
        assert_eq!(headline, "Never Launched Fixture");
        assert_eq!(detail, "This window can't be shown.");

        for (field, text) in [("headline", &headline), ("detail", &detail)] {
            let lower = text.to_lowercase();
            for jargon in [
                "frame", "buffer", "surface", "dmabuf", "modifier", "wayland", "toplevel",
                "dev.qos", "install",
            ] {
                assert!(
                    !lower.contains(jargon),
                    "the {field} on a window somebody is reading says {jargon:?}: \
                     {text:?}",
                );
            }
        }
        store().forget(id);
    }

    /// The window does not go up until there is a window to look at.
    #[test]
    fn a_surface_that_has_not_drawn_is_not_shown() {
        let id = sid(40);
        store().forget(id);
        store().note_placement(id, TILE);
        assert!(
            !store().mark_presented(id, TILE),
            "nothing has been committed, so there is nothing to show",
        );
        store().forget(id);
    }

    /// The frame the compositor's opening configure produces: the client picked its own size, so painting it means scaling to fit until the right-sized buffer lands and snaps.
    #[test]
    fn a_frame_that_does_not_fit_the_tab_is_not_shown() {
        let id = sid(41);
        store().forget(id);
        // 4:3 into a 3:2 tab — fitted, it leaves a band down one side.
        store().publish_rgba(id, &vec![0; 800 * 600 * 4], 800, 600);
        assert!(
            !store().mark_presented(id, TILE),
            "an 800x600 buffer in a 1200x800 tab is drawn magnified, then \
             snaps down when the real one arrives — which is the resize",
        );
        store().forget(id);
    }

    /// Any buffer whose aspect matches is drawn at exactly the tab, so the one that follows it changes nothing the user can see.
    #[test]
    fn a_frame_that_fits_the_tab_is_shown() {
        for (w, h) in [(1200u32, 800u32), (2400, 1600), (600, 400)] {
            let id = sid(42);
            store().forget(id);
            store().publish_rgba(id, &vec![0; (w * h * 4) as usize], w, h);
            assert!(
                store().mark_presented(id, TILE),
                "{w}x{h} has the tab's aspect, so it fills it",
            );
            store().forget(id);
        }
    }

    /// Resizing a group makes the current frame stop fitting for a frame or two — disappearing in response would be worse than the defect this gate fixes.
    #[test]
    fn a_shown_window_stays_shown_when_its_frame_stops_fitting() {
        let id = sid(43);
        store().forget(id);
        store().publish_rgba(id, &vec![0; 1200 * 800 * 4], 1200, 800);
        assert!(store().mark_presented(id, TILE));

        let dragged = Rect::new(0.0, 0.0, 400.0, 800.0);
        assert!(
            store().mark_presented(id, dragged),
            "the window is on screen; a resize does not take it away",
        );
        store().forget(id);
    }

    /// An app that cannot be shown must say so rather than be an invisible hole in the strip.
    #[test]
    fn a_surface_whose_capture_failed_is_shown_so_it_can_say_why() {
        let id = sid(44);
        store().forget(id);
        store().note_failure(id, "unsupported buffer".into());
        assert!(store().mark_presented(id, TILE));
        store().forget(id);
    }

    /// A client that answered the size it was configured to, less the
    /// strip its own toolkit kept for decorations, has arrived.
    ///
    /// Not hypothetical: before `zxdg_decoration_manager_v1`, every winit
    /// client reserved a title bar out of its size — configured 670x668,
    /// committed 670x633, forever. Judged on aspect alone that never fits, so the window waited out [`PRESENT_GRACE`] over an app that had painted in two seconds.
    ///
    /// A client that never binds the global, or decorates itself regardless, still produces exactly this buffer and must still be shown.
    #[test]
    fn a_frame_short_by_the_clients_own_decorations_is_shown() {
        let id = sid(45);
        store().forget(id);
        store().note_placement(id, TILE);
        // The tab less a title bar the client kept for itself.
        let (w, h) = (1200u32, 765u32);
        store().publish_rgba(id, &vec![0; (w * h * 4) as usize], w, h);
        assert!(
            store().mark_presented(id, TILE),
            "{w}x{h} is the tab's width and answers its height as closely \
             as a client that draws its own frame ever will — waiting for \
             it to give that strip back is waiting for the grace period",
        );
        store().forget(id);
    }

    /// `fills` is what the gate is made of; the grace period is what
    /// stops it stranding a window — both asserted directly since the ten-second wait isn't something a test can sit through.
    #[test]
    fn fills_accepts_a_buffer_sized_to_the_tab_and_nothing_else() {
        assert!(fills(1200, 800, TILE));
        assert!(fills(300, 200, TILE), "same aspect, quarter size");
        assert!(!fills(800, 600, TILE));
        assert!(!fills(1200, 900, TILE));
        assert!(!fills(0, 800, TILE), "no buffer is not a fitting buffer");
        assert!(
            !fills(1200, 800, Rect::ZERO),
            "a tab with no area cannot be judged against",
        );

        // A client that kept a title bar out of the size it was given: one axis answered exactly, the other short, nothing overflowing.
        assert!(
            fills(1200, 765, TILE),
            "short by a title bar it drew itself"
        );
        assert!(fills(1160, 800, TILE), "short by a frame down one side");
        assert!(
            !fills(1210, 765, TILE),
            "wider than the tab is not an answer to it, however close",
        );
        assert!(
            !fills(600, 500, TILE),
            "inside the tab but answering neither axis is the client's own \
             idea of its size, which is the frame this gate keeps back",
        );
    }

    #[test]
    fn publish_then_snapshot_round_trips_and_bumps_seq() {
        let id = sid(1);
        store().forget(id);
        assert_eq!(store().frame_seq(id), 0);
        assert!(store().snapshot(id).is_none());

        store().publish_rgba(id, &[1, 2, 3, 4, 5, 6, 7, 8], 2, 1);
        let seq = store().frame_seq(id);
        assert_eq!(seq, 1, "first publish bumps the sequence to 1");
        let snap = store().snapshot(id).expect("frame present");
        assert_eq!(snap.rgba, vec![1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!((snap.width, snap.height), (2, 1));

        // A non-draining read: a second snapshot still sees the frame.
        assert!(store().snapshot(id).is_some());
        store().forget(id);
    }

    #[test]
    fn note_failure_drops_the_stale_frame_and_records_the_reason() {
        let id = sid(2);
        store().forget(id);
        store().publish_rgba(id, &[9; 16], 2, 2);
        assert!(store().snapshot(id).is_some());

        store().note_failure(id, "test failure".into());
        assert!(
            store().snapshot(id).is_none(),
            "a failed capture must not leave the previous frame visible"
        );
        assert_eq!(store().last_error(id).as_deref(), Some("test failure"));
        store().forget(id);
    }

    #[test]
    fn clear_frame_unmaps_without_forgetting_placement() {
        let id = sid(3);
        store().forget(id);
        store().note_placement(id, Rect::new(0.0, 0.0, 800.0, 600.0));
        store().publish_rgba(id, &[7; 16], 2, 2);
        store().clear_frame(id);
        assert!(store().snapshot(id).is_none());
        assert_eq!(
            store().viewport(id),
            Some((800, 600)),
            "unmap keeps the tab's placement; only destroy forgets it"
        );
        store().forget(id);
        assert_eq!(store().viewport(id), None);
    }

    #[test]
    fn pending_configure_fires_once_per_size_change() {
        let id = sid(4);
        store().forget(id);
        assert_eq!(store().take_pending_configure(id, 1.0), None);

        store().note_placement(id, Rect::new(0.0, 0.0, 1200.0, 800.0));
        assert_eq!(store().take_pending_configure(id, 1.0), Some((1200, 800)));
        assert_eq!(
            store().take_pending_configure(id, 1.0),
            None,
            "an unchanged layout must not re-send a configure every frame"
        );

        store().note_placement(id, Rect::new(0.0, 0.0, 600.0, 400.0));
        assert_eq!(store().take_pending_configure(id, 1.0), Some((600, 400)));
        store().forget(id);
    }

    #[test]
    fn pending_configure_divides_physical_pixels_by_scale() {
        let id = sid(5);
        store().forget(id);
        store().note_placement(id, Rect::new(0.0, 0.0, 2400.0, 1600.0));
        assert_eq!(
            store().take_pending_configure(id, 2.0),
            Some((1200, 800)),
            "the configure carries surface-local (logical) size, not physical px"
        );
        store().forget(id);
    }

    #[test]
    fn frame_seq_forget_resets_to_zero() {
        let id = sid(6);
        store().forget(id);
        store().publish_rgba(id, &[0; 4], 1, 1);
        assert_ne!(store().frame_seq(id), 0);
        store().forget(id);
        assert_eq!(
            store().frame_seq(id),
            0,
            "destroying a toplevel must release its cached frame"
        );
    }

    /// A surface asks for frames until it has drawn, and after that only when
    /// a newer one is waiting. The first half matters because an
    /// undrawn-first-buffer client is the one case nothing else wakes the
    /// paint loop for; the second: an app with nothing newer must not hold the loop awake.
    #[test]
    fn widget_asks_for_frames_until_it_has_drawn_then_only_for_a_pending_one() {
        let id = sid(7);
        store().forget(id);
        let mut w = ForeignSurfaceWidget::new(id, "org.example.app".into());
        // A 1×1 tab, so the 1×1 frame below *fits* it — what being presented waits for, not merely a frame having landed.
        w.rect = Rect::new(0.0, 0.0, 1.0, 1.0);
        assert!(
            w.needs_animation_frame(),
            "nothing presented yet: a widget that stopped asking here would \
             never be woken to paint the window's first frame"
        );
        store().publish_rgba(id, &[255, 0, 0, 255], 1, 1);
        assert!(
            w.needs_animation_frame(),
            "a fresh frame requests a repaint"
        );
        assert!(w.refresh_cache(), "cache picks up the new frame");
        assert!(w.presented(), "a frame that fits the tab is on screen");
        assert!(
            !w.needs_animation_frame(),
            "once drawn, an idle client must not keep the paint loop awake"
        );
        store().forget(id);
    }

    #[test]
    fn widget_fit_rect_letterboxes_preserving_aspect() {
        let mut w = ForeignSurfaceWidget::new(sid(8), "x".into());
        w.rect = Rect::new(0.0, 0.0, 400.0, 200.0);
        // A square image inside a 2:1 tile: height-limited, anchored at the tile's top-left with the spare width to the right.
        let fit = w.fit_rect(100, 100).expect("fit");
        assert_eq!((fit.width(), fit.height()), (200.0, 200.0));
        assert_eq!(fit.x0, 0.0);
        assert_eq!(fit.y0, 0.0);
        // An exactly-matching client fills the tile with no letterbox.
        let exact = w.fit_rect(400, 200).expect("fit");
        assert_eq!(exact, w.rect);
    }

    /// A press has to land on what the user can see, so client pixels and pointer input must share one origin — including mid-resize, before the client's frame catches up.
    #[test]
    fn painted_frame_origin_matches_the_origin_input_is_mapped_with() {
        let tile = Rect::new(288.0, 124.0, 288.0 + 856.0, 124.0 + 868.0);
        let mut w = ForeignSurfaceWidget::new(sid(9), "x".into());
        w.rect = tile;

        // Every size a lagging client might still be presenting: exactly right, short, tall, narrow and wide.
        for (iw, ih) in [(856, 868), (856, 834), (856, 900), (800, 868), (900, 868)] {
            let dst = w.fit_rect(iw, ih).expect("fit");
            let (local_x, local_y) =
                crate::compositor::client_input::surface_local((dst.x0, dst.y0), tile);
            assert_eq!(
                (local_x, local_y),
                (0.0, 0.0),
                "a {iw}x{ih} frame in an {}x{} tile paints from {:?}, but input maps that \
                 point to ({local_x}, {local_y}) — a press would miss by that much",
                tile.width(),
                tile.height(),
                (dst.x0, dst.y0),
            );
        }
    }

    #[test]
    fn reconcile_copies_adoptions_and_teardowns_across_trees() {
        let mut source = WorkspaceTree::bootstrap_demo();
        let mut rendered = WorkspaceTree::bootstrap_demo();
        assert!(
            !reconcile_foreign_tabs(&source, &mut rendered),
            "two pristine trees need no mirroring"
        );

        crate::compositor::tab_binding::adopt_toplevel(
            &mut source,
            4242,
            Some("org.example.client".into()),
        );
        assert!(reconcile_foreign_tabs(&source, &mut rendered));
        let mirrored = foreign_tabs(&rendered);
        assert_eq!(mirrored.len(), 1);
        assert_eq!(mirrored[0].surface_id, 4242);
        assert_eq!(mirrored[0].app_id, "org.example.client");
        assert!(
            !reconcile_foreign_tabs(&source, &mut rendered),
            "mirroring is idempotent"
        );

        crate::compositor::tab_binding::close_tab_for_destroyed_surface(&mut source, 4242);
        assert!(reconcile_foreign_tabs(&source, &mut rendered));
        assert!(foreign_tabs(&rendered).is_empty());
    }

    /// One window, one tab — wherever in the tree that tab sits, including a workspace other than the one the user is looking at.
    #[test]
    fn a_window_in_another_workspace_is_not_adopted_a_second_time() {
        let mut source = WorkspaceTree::bootstrap_demo();
        let mut rendered = WorkspaceTree::bootstrap_demo();
        let surface = 31;

        crate::compositor::tab_binding::adopt_toplevel(
            &mut source,
            surface,
            Some("dev.qos.word-processor".into()),
        );
        assert!(reconcile_foreign_tabs(&source, &mut rendered));
        assert_eq!(
            tabs_for_surface(&rendered, surface),
            1,
            "the mirror should adopt the window exactly once",
        );

        // The user goes to another workspace — nothing about the window changed, it's simply no longer the one on screen.
        let elsewhere = rendered
            .workspaces
            .iter()
            .map(|w| w.id)
            .find(|id| *id != rendered.active)
            .expect("the demo tree has more than one workspace");
        rendered.active = elsewhere;

        reconcile_foreign_tabs(&source, &mut rendered);
        assert_eq!(
            tabs_for_surface(&rendered, surface),
            1,
            "switching workspaces opened a second window onto the same surface",
        );

        // And back again, which is the sequence a person actually performs.
        rendered.active = source.active;
        reconcile_foreign_tabs(&source, &mut rendered);
        assert_eq!(
            tabs_for_surface(&rendered, surface),
            1,
            "returning to the window's own workspace duplicated it",
        );
    }

    /// A window the launcher placed in a workspace other than the active one
    /// is already adopted, must not be adopted again — the launch path, not the switch path: a launch records where asked, and the toplevel can map later.
    #[test]
    fn a_window_placed_outside_the_active_workspace_is_adopted_once() {
        let mut source = WorkspaceTree::bootstrap_demo();
        let mut rendered = WorkspaceTree::bootstrap_demo();
        let surface = 32;

        crate::compositor::tab_binding::adopt_toplevel(
            &mut source,
            surface,
            Some("dev.qos.photos".into()),
        );
        assert!(reconcile_foreign_tabs(&source, &mut rendered));

        // Move the adopted tab into a workspace that isn't the active one, the way a recorded placement or a peer's room does.
        let elsewhere = rendered
            .workspaces
            .iter()
            .map(|w| w.id)
            .find(|id| *id != rendered.active)
            .expect("the demo tree has more than one workspace");
        let moved = take_foreign_tab(&mut rendered, surface).expect("the adopted tab");
        let ws = rendered
            .workspaces
            .iter_mut()
            .find(|w| w.id == elsewhere)
            .expect("the other workspace");
        let strip = ws.strips.first_mut().expect("the other workspace's strip");
        strip.window_groups.push(crate::shell::state::WindowGroup {
            id: crate::shell::state::next_id(),
            active_tab: moved.id,
            tabs: vec![moved],
            width: crate::shell::state::WindowWidth::Normal,
            maximized: false,
        });

        reconcile_foreign_tabs(&source, &mut rendered);
        assert_eq!(
            tabs_for_surface(&rendered, surface),
            1,
            "the window was adopted again because its tab was in another workspace",
        );
    }

    /// A desktop that already has the duplicate (a session saved before the mirror compared whole trees) comes back with one window, not two.
    #[test]
    fn a_duplicate_already_in_the_tree_is_collapsed() {
        let mut source = WorkspaceTree::bootstrap_demo();
        let mut rendered = WorkspaceTree::bootstrap_demo();
        let surface = 33;

        crate::compositor::tab_binding::adopt_toplevel(
            &mut source,
            surface,
            Some("dev.qos.photos".into()),
        );
        assert!(reconcile_foreign_tabs(&source, &mut rendered));

        // Stage the state a pre-fix session left behind: a second tab onto the same surface, in the same tab bar, showing the same name.
        let twin = {
            let one = foreign_tabs_anywhere(&rendered)
                .into_iter()
                .find(|t| t.surface_id == surface)
                .expect("the adopted tab");
            crate::shell::state::Tab {
                id: one.tab_id + 1_000_000,
                title: one.title.clone(),
                tile: ShellTile::Foreign {
                    app_id: one.app_id,
                    surface_id: surface,
                },
            }
        };
        let group = rendered
            .active_workspace_mut()
            .and_then(|w| w.strips.first_mut())
            .and_then(|s| s.window_groups.first_mut())
            .expect("the group the window was adopted into");
        group.tabs.push(twin);
        assert_eq!(
            tabs_for_surface(&rendered, surface),
            2,
            "the fixture is staged"
        );

        assert!(
            reconcile_foreign_tabs(&source, &mut rendered),
            "collapsing the duplicate is a change the chrome has to be told about",
        );
        assert_eq!(
            tabs_for_surface(&rendered, surface),
            1,
            "the restored desktop still shows the application twice",
        );

        // And the group is still showing something that exists.
        let group = rendered
            .active_workspace()
            .and_then(|w| w.strips.first())
            .and_then(|s| s.window_groups.first())
            .expect("the group");
        assert!(
            group.tabs.iter().any(|t| t.id == group.active_tab),
            "the group is pointing at a tab that is no longer there",
        );
    }

    /// How many tabs anywhere in `tree` show `surface` — walks the tree the chrome renders from, so a duplicate is counted where it'd draw.
    fn tabs_for_surface(tree: &WorkspaceTree, surface: SurfaceId) -> usize {
        tree.workspaces
            .iter()
            .flat_map(|w| w.strips.iter())
            .flat_map(|s| s.window_groups.iter())
            .flat_map(|g| g.tabs.iter())
            .filter(|t| {
                matches!(&t.tile, ShellTile::Foreign { surface_id, .. } if *surface_id == surface)
            })
            .count()
    }

    /// Detach the tab showing `surface` from wherever it is, so a test can put it somewhere else.
    fn take_foreign_tab(
        tree: &mut WorkspaceTree,
        surface: SurfaceId,
    ) -> Option<crate::shell::state::Tab> {
        for ws in &mut tree.workspaces {
            for strip in &mut ws.strips {
                for group in &mut strip.window_groups {
                    if let Some(i) = group.tabs.iter().position(|t| {
                        matches!(
                            &t.tile,
                            ShellTile::Foreign { surface_id, .. } if *surface_id == surface
                        )
                    }) {
                        return Some(group.tabs.remove(i));
                    }
                }
            }
        }
        None
    }

    #[test]
    fn reconcile_propagates_a_late_set_app_id() {
        let mut source = WorkspaceTree::bootstrap_demo();
        let mut rendered = WorkspaceTree::bootstrap_demo();
        crate::compositor::tab_binding::adopt_toplevel(&mut source, 77, None);
        assert!(reconcile_foreign_tabs(&source, &mut rendered));
        assert_eq!(foreign_tabs(&rendered)[0].app_id, "anon-77");

        crate::compositor::tab_binding::rename_tab_for_app_id(
            &mut source,
            77,
            "io.example.late".into(),
        );
        assert!(reconcile_foreign_tabs(&source, &mut rendered));
        let t = &foreign_tabs(&rendered)[0];
        assert_eq!(t.app_id, "io.example.late");
        // The identifier propagates; the *title* is humanised from it rather than shown as a machine identifier nobody chose.
        assert_eq!(
            t.title, "Late",
            "a tab wears a name, not the last segment of an app id"
        );
    }

    /// The window itself has to answer with the same name its tab does.
    ///
    /// The tab title was fixed once already; the widget kept handing back
    /// the raw wayland app-id from `app_title()`, so asking the tile directly (not the tab's stored title) got `dev.qos.chat`, an address nobody chose.
    #[test]
    fn a_foreign_window_answers_with_its_name_not_its_app_id() {
        let w = ForeignSurfaceWidget::new(3, "dev.qos.chat".into());
        assert_eq!(
            w.app_title(),
            "Chat",
            "a window says what it is called, not how it is addressed",
        );

        // A dotted id with a multi-word tail humanises the same way the package registry does, so an app reaches one name by either route.
        let w = ForeignSurfaceWidget::new(4, "dev.qos.word-processor".into());
        assert_eq!(w.app_title(), "Word Processor");
    }

    #[test]
    fn visibility_tracks_the_groups_active_tab() {
        use crate::compositor::tab_binding::{Placement, adopt_toplevel, adopt_toplevel_at};
        crate::shell::launcher::clear_pending_placements();
        let mut tree = WorkspaceTree::bootstrap_demo();
        // A lone selected window, so each adoption opens its own column rather than joining a tabbed group — the arrangement this test is about.
        {
            let strip_id = tree.workspaces[0].strips[0].id;
            let g = &mut tree.workspaces[0].strips[0].window_groups[0];
            g.tabs.truncate(1);
            g.active_tab = g.tabs[0].id;
            let gid = g.id;
            tree.active_strip.insert(tree.active, strip_id);
            tree.set_active_group(strip_id, gid);
        }
        let first = adopt_toplevel(&mut tree, 11, None);
        assert!(is_visible_tab(&tree, 11), "a fresh adoption becomes active");

        // A second launch opens its own group beside the first, so both windows are on screen — that is the point of the strip.
        let _second = adopt_toplevel(&mut tree, 12, None);
        assert!(
            is_visible_tab(&tree, 11),
            "a launch into its own group must not hide the previous window"
        );
        assert!(is_visible_tab(&tree, 12));

        // Stacking into the *same* group is what puts a window behind another — the tab dimension, not the strip one.
        let owner = tree
            .active_workspace()
            .unwrap()
            .strips
            .iter()
            .flat_map(|s| s.window_groups.iter())
            .find(|g| g.tabs.iter().any(|t| t.id == first))
            .expect("the first window's group")
            .id;
        let _third = adopt_toplevel_at(&mut tree, 13, None, Placement::TabIn(owner));
        assert!(
            !is_visible_tab(&tree, 11),
            "a tab added to the same group covers the one that was showing"
        );
        assert!(is_visible_tab(&tree, 13));
        assert!(
            !is_visible_tab(&tree, 999),
            "unknown surfaces are not visible"
        );
    }
}
