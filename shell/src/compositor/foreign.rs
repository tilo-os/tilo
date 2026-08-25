//! Compositing of foreign (external Wayland client) surfaces into tabs.
//!
//! An external client that binds our `wl_compositor` + `xdg_wm_base` gets
//! its toplevel adopted into the workspace tab tree by
//! [`crate::compositor::tab_binding::adopt_toplevel`]. This module is what
//! turns that bookkeeping entry into *pixels on screen*:
//!
//! ```text
//!  client commit
//!      │
//!      ├─ wl_shm  ──► surface_readback::buffer_to_rgba ──┐
//!      └─ dmabuf  ──► gpu_import (EGL → GL → readback) ──┤
//!                     └ or import_dmabuf_rgba (mmap) ────┤
//!                                                        ▼
//!                                            ForeignFrameStore (per surface)
//!                                                        │  snapshot on seq change
//!                                                        ▼
//!                              ForeignSurfaceWidget::draw → peniko::ImageBrush
//!                                                        │
//!                                            widget rect ─┴─► xdg_toplevel.configure
//! ```
//!
//! ## Why a widget rather than a post-pass blit
//!
//! The tab's on-screen rectangle is owned by the shell's widget tree
//! (`WindowGroup` → `TileHost`). Hosting the foreign surface *as* a
//! [`ui::widget::AppWidget`] means placement, clipping, rounded-corner
//! masking, spawn/close fades and "only the active tab paints" all come
//! from the same code path every other tile uses — instead of a second,
//! parallel geometry calculation that would drift out of sync. The widget
//! also publishes its own laid-out rect back into the store, which is what
//! the `xdg_toplevel.configure` sizing pass reads.
//!
//! ## Repaint gating
//!
//! [`ForeignSurfaceWidget::needs_animation_frame`] reports `true` exactly
//! when the store holds a frame the widget has not drawn yet. The native
//! render loop's existing `needs_animation_frame()` condition therefore
//! wakes the compositor for a new client frame and for nothing else — an
//! idle client costs one `Mutex` lock + one `HashMap` lookup per tick and
//! zero paints. `ui::widget::WindowGroup::needs_animation_frame` only
//! consults the *active* host, so a background tab whose client renders
//! continuously cannot pin the paint loop either.
//!
//! ## Buffer formats
//!
//! - `wl_shm` `Argb8888` / `Xrgb8888`: handled by
//!   [`crate::compositor::surface_readback::buffer_to_rgba`].
//! - dmabuf: two routes, tried in order by
//!   [`ForeignFrameStore::capture_dmabuf`].
//!   1. **GPU** — EGL wraps the client's buffer as a texture and the GPU
//!      reads it back ([`crate::compositor::gpu_import`]). This is the
//!      route that works for a Vulkan client, whose device-local
//!      swapchain images refuse `mmap` outright.
//!   2. **CPU mmap** — [`import_dmabuf_rgba`] maps the plane fd
//!      directly. Correct for the CPU-visible linear buffers a GBM/EGL
//!      client allocates, and the only route on a box with no usable
//!      EGL device.
//!
//!   The compositor only ever advertises single-plane RGBA under
//!   `Modifier::Linear` (see `State::new`), so both routes cover the
//!   whole advertised set. A buffer neither route can read is
//!   **rejected with a visible on-tile message** carrying both
//!   failures, never silently painted as a stale or blank frame.

use std::collections::HashMap;
use std::os::fd::AsRawFd;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::{Buffer as _, Fourcc, Modifier};
use smithay::reexports::wayland_server::Resource;
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

/// Wayland `wl_surface` protocol id, widened. Same key space the rest of
/// the compositor uses for foreign surfaces.
pub type SurfaceId = u64;

// ---------------------------------------------------------------------------
// dmabuf CPU import
// ---------------------------------------------------------------------------

/// `struct dma_buf_sync { __u64 flags; }` — `DMA_BUF_IOCTL_SYNC` is
/// `_IOW('b', 0, struct dma_buf_sync)`, i.e.
/// `(_IOC_WRITE << 30) | (8 << 16) | ('b' << 8) | 0`.
const DMA_BUF_IOCTL_SYNC: libc::c_ulong = 0x4008_6200;
const DMA_BUF_SYNC_READ: u64 = 1 << 0;
const DMA_BUF_SYNC_START: u64 = 0;
const DMA_BUF_SYNC_END: u64 = 1 << 2;

/// Why a dmabuf could not be imported. Every variant is reported to the
/// user on the tile itself rather than degrading to a blank surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DmabufImportError {
    /// The buffer carries a vendor tiling / compression modifier. We only
    /// advertise `Modifier::Linear`, so this means the client ignored the
    /// advertised set (or a future change widened it without teaching this
    /// importer about GPU-side import).
    NonLinearModifier(u64),
    /// Multi-planar (e.g. NV12). The RGBA path here is single-plane only.
    MultiPlanar(usize),
    /// FourCC we don't know how to swizzle.
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

/// Whether a buffer carrying `modifier` has a row-major layout the CPU
/// import can read.
///
/// `LINEAR` obviously qualifies. `INVALID` — the "implicit modifier"
/// escape hatch a client uses when it calls
/// `zwp_linux_buffer_params_v1.create` without `add`-ing a modifier — is
/// accepted too, because the compositor's dmabuf global advertises
/// `LINEAR` and nothing else: a conforming client that allocated against
/// our advertisement and then dropped the modifier on the wire still
/// allocated a linear buffer. Anything else is a vendor tiling or
/// compression layout whose bytes are not an image, and is rejected so
/// the tile reports it rather than painting noise.
fn modifier_is_cpu_readable(modifier: Modifier) -> bool {
    matches!(modifier, Modifier::Linear | Modifier::Invalid)
}

/// Byte layout of one pixel as it sits in memory for a DRM FourCC.
///
/// DRM format names read the *most significant* byte first of a
/// little-endian 32-bit word, so `ARGB8888` is `[B, G, R, A]` in memory
/// and `ABGR8888` is `[R, G, B, A]`.
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
/// appending to `out`.
///
/// Extracted as a free function so the unit tests can pin the channel
/// mapping without a GPU or a wayland connection — the dmabuf mmap path
/// is otherwise untestable in-process.
fn swizzle_row(order: PixelOrder, src: &[u8], width: usize, out: &mut Vec<u8>) {
    debug_assert_eq!(src.len(), width * 4);
    match order {
        PixelOrder::Bgra { opaque } => {
            for px in src.chunks_exact(4) {
                out.push(px[2]);
                out.push(px[1]);
                out.push(px[0]);
                out.push(if opaque { 0xFF } else { px[3] });
            }
        }
        PixelOrder::Rgba { opaque } => {
            for px in src.chunks_exact(4) {
                out.push(px[0]);
                out.push(px[1]);
                out.push(px[2]);
                out.push(if opaque { 0xFF } else { px[3] });
            }
        }
    }
}

/// CPU-import a single-plane linear dmabuf into row-major RGBA.
///
/// `out` is cleared and refilled. Returns the buffer's `(width, height)`.
///
/// This is a **CPU mmap import**, not a zero-copy GPU import: the plane fd
/// is mapped read-only, `DMA_BUF_IOCTL_SYNC` brackets the read so the
/// GPU's writes are visible and caches are handled, and the rows are
/// swizzled into RGBA. It is correct for every buffer the compositor
/// advertises (LINEAR only) and costs one full-frame copy.
/// Cheap, allocation-free "can this compositor turn that buffer into
/// tile pixels?" check — format, modifier and plane count only.
///
/// Split out from [`import_dmabuf_rgba`] so the
/// `zwp_linux_dmabuf_v1` import gate
/// ([`smithay::wayland::dmabuf::DmabufHandler::dmabuf_imported`]) can
/// answer the client *at negotiation time*. Rejecting there makes the
/// client's `create` fail, which is its cue to renegotiate or fall back
/// to `wl_shm`; accepting and then failing at commit would leave it
/// happily submitting frames into an error tile forever.
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

    // Bracket the CPU read with DMA_BUF_IOCTL_SYNC so the exporting driver
    // flushes/invalidates as needed. A driver that doesn't implement the
    // ioctl returns an error we deliberately ignore — the mapping is still
    // readable, we just lose the explicit cache hint.
    sync_dmabuf(raw_fd, DMA_BUF_SYNC_START | DMA_BUF_SYNC_READ);

    out.clear();
    out.reserve((width as usize) * (height as usize) * 4);
    let row_bytes = (width as usize) * 4;
    // SAFETY: the mapping covers `[0, map_len)`; every row slice below is
    // inside `[offset, offset + stride*height) ⊆ [0, map_len)`, checked
    // above. The slice is dropped before `munmap`.
    let mapped: &[u8] = unsafe { std::slice::from_raw_parts(ptr as *const u8, map_len) };
    for row in 0..height as usize {
        let start = offset as usize + row * stride as usize;
        swizzle_row(
            order,
            &mapped[start..start + row_bytes],
            width as usize,
            out,
        );
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
        libc::ioctl(fd, DMA_BUF_IOCTL_SYNC, &arg as *const DmaBufSync);
    }
}

// ---------------------------------------------------------------------------
// Frame store
// ---------------------------------------------------------------------------

/// Per-surface capture + placement state.
struct ForeignEntry {
    /// Most recent frame, row-major RGBA, `width * height * 4` bytes.
    /// Reused across commits so a steady-state client costs no
    /// reallocation.
    rgba: Vec<u8>,
    width: u32,
    height: u32,
    /// Monotonic frame counter. `0` means "no frame has ever landed".
    /// Bumped on every successful capture *and* on unmap, so a consumer
    /// comparing sequence numbers always notices a transition.
    seq: u64,
    /// Human-readable reason the last capture failed, drawn on the tile
    /// so an unsupported buffer is visible rather than silently blank.
    last_error: Option<String>,
    /// The tab's on-screen rectangle in physical pixels, published by the
    /// widget's `apply_layout`. `None` until the surface's tab has been
    /// laid out (a surface whose tab is in a background group or an
    /// inactive workspace never gets one — those tiles aren't built).
    placement: Option<Rect>,
    /// Size most recently sent to the client in an `xdg_toplevel`
    /// configure, in surface-local (logical) pixels.
    configured: Option<(i32, i32)>,
    /// When this surface was first heard of — the toplevel being adopted,
    /// in practice, since that is what lays its tab out and publishes a
    /// placement. Start of the window [`ForeignFrameStore::mark_presented`]
    /// waits before giving up on a well-sized frame.
    first_seen: Instant,
    /// Latched: this surface has produced something worth showing, so
    /// its window is on screen and stays there.
    ///
    /// One-way on purpose. The predicate behind it asks whether the
    /// current frame fits the tab it is in, and that briefly stops being
    /// true every time the user resizes a group — a window that could
    /// un-present would blink out mid-drag, which is a worse thing to
    /// watch than the one this exists to prevent.
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
/// A singleton rather than a field threaded through the widget tree
/// because `crate::shell::to_ui_tile` — the one place a
/// `TileContent::Foreign` becomes a widget — is a pure tree→widget
/// conversion reached from half a dozen call sites (initial build,
/// strip resync, the offscreen `shots` harness) that carry no compositor
/// context. Keying every entry on the wayland protocol id keeps the
/// shared state trivially partitioned per surface.
#[derive(Default)]
pub struct ForeignFrameStore {
    entries: Mutex<HashMap<SurfaceId, ForeignEntry>>,
    /// Total buffer imports performed, of any route.
    ///
    /// A dmabuf import costs a GPU readback (or a full-frame CPU map),
    /// so it must happen **once per client commit** and never once per
    /// paint — the compositor paints far more often than a client
    /// commits, and re-importing per frame would turn an idle GPU client
    /// into a permanent load. This counter is what
    /// `painting_does_not_reimport_the_buffer` asserts on.
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
    /// EGL imported the buffer as a GL texture and the GPU read it back.
    /// Works for every buffer the GPU can sample, including the
    /// device-local Vulkan swapchain images `wgpu` clients allocate.
    Gpu,
    /// The plane fd was CPU-mapped directly. Only possible for a linear
    /// buffer in CPU-visible memory, but needs no GL context.
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
    /// Neither path could produce pixels; the reason is recorded on the
    /// entry and drawn on the tile.
    Failed(String),
}

impl ForeignFrameStore {
    fn with_entry<R>(&self, id: SurfaceId, f: impl FnOnce(&mut ForeignEntry) -> R) -> R {
        let mut guard = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        f(guard.entry(id).or_insert_with(ForeignEntry::new))
    }

    fn note_import(&self) {
        self.imports
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// Buffer imports performed since process start, across every
    /// surface and route. Read by the repaint-cost test — see the
    /// field's doc for why the number must not track paints.
    pub fn import_count(&self) -> u64 {
        self.imports.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Capture the buffer a client just committed. Tries the `wl_shm`
    /// readback first; a buffer smithay reports as not-shm is routed to
    /// the dmabuf importer.
    ///
    /// The `wl_buffer → Dmabuf` lookup is done by the caller-visible
    /// [`smithay::wayland::dmabuf::get_dmabuf`], so this stays a single
    /// entry point for the commit hook.
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
                    // Leave the previous frame intact for now; the dmabuf
                    // branch below may still succeed. A genuine failure
                    // clears it via `note_failure`.
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
                match smithay::wayland::dmabuf::get_dmabuf(buffer) {
                    Ok(dmabuf) => self.capture_dmabuf(id, &dmabuf),
                    Err(_) => {
                        let msg = "buffer is neither wl_shm nor dmabuf".to_string();
                        self.note_failure(id, msg.clone());
                        CaptureOutcome::Failed(msg)
                    }
                }
            }
            Err(err) => {
                let msg = err.to_string();
                self.note_failure(id, msg.clone());
                CaptureOutcome::Failed(msg)
            }
        }
    }

    /// Import an already-resolved [`Dmabuf`] into the store.
    ///
    /// Two routes, tried in order:
    ///
    /// 1. **GPU** ([`crate::compositor::gpu_import`]) — EGL wraps the
    ///    client's buffer as a texture and the GPU reads it back. This
    ///    is the route that works for a Vulkan client: `wgpu` (and thus
    ///    every qOS app) allocates device-local swapchain images, which
    ///    on amdgpu carry `NO_CPU_ACCESS` and refuse `mmap` with
    ///    `EPERM` however linear they are.
    /// 2. **CPU mmap** ([`import_dmabuf_rgba`]) — no GL context needed,
    ///    and correct for the CPU-visible linear buffers a GBM/EGL
    ///    client (alacritty, GTK, …) allocates. This is the route on a
    ///    machine with no usable EGL device at all.
    ///
    /// Both failing is reported on the tile with the GPU route's reason
    /// *and* the CPU route's, because "which one would have worked" is
    /// the only useful thing to know at that point.
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
                    // The import itself failed for this buffer…
                    Ok(Err(e)) => Some(e.to_string()),
                    // …or there is no GPU import context at all.
                    Err(e) => Some(e.to_string()),
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

    /// Publish an RGBA frame directly. Used by callers that already hold
    /// decoded pixels (the dmabuf importer's own tests, and any future
    /// capture route that doesn't go through a `wl_buffer`).
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
    /// Two things read it. The window stops waiting — a surface with a
    /// failure against it has something to say, so it is shown at once
    /// rather than held back for a frame that is not coming. And the words
    /// it shows are chosen by [`ForeignSurfaceWidget::unshowable_text`],
    /// which says that the window cannot be shown and leaves `reason` here
    /// for whoever is debugging rather than whoever is waiting.
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

    /// The client detached its buffer (`wl_surface.attach(NULL)`): the
    /// surface is unmapped, so drop the frame but keep the placement
    /// bookkeeping (the tab still exists until the toplevel is destroyed).
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

    /// Drop everything tracked for `id`. Called when the toplevel is
    /// destroyed — without this the frame `Vec` (a full framebuffer)
    /// would outlive the client.
    pub fn forget(&self, id: SurfaceId) {
        let mut guard = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        guard.remove(&id);
    }

    /// Current frame counter for `id`; `0` when nothing has landed.
    pub fn frame_seq(&self, id: SurfaceId) -> u64 {
        let guard = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        guard.get(&id).map(|e| e.seq).unwrap_or(0)
    }

    /// Copy out the current frame, or `None` when the surface has no
    /// pixels staged.
    pub fn snapshot(&self, id: SurfaceId) -> Option<FrameSnapshot> {
        let guard = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let e = guard.get(&id)?;
        if e.rgba.is_empty() || e.width == 0 || e.height == 0 {
            return None;
        }
        Some(FrameSnapshot {
            rgba: e.rgba.clone(),
            width: e.width,
            height: e.height,
            seq: e.seq,
        })
    }

    /// Dimensions of the frame currently staged for `id`, without
    /// copying it. `None` when nothing has landed.
    ///
    /// Exists so a harness can compare what the client drew against the
    /// tab it was drawn into. Those two disagreeing is what a window
    /// visibly resizing after it opens *is*, and a test that can only see
    /// the frame counter cannot tell that apart from a clean open.
    pub fn frame_size(&self, id: SurfaceId) -> Option<(u32, u32)> {
        let guard = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let e = guard.get(&id)?;
        (e.width > 0 && e.height > 0).then_some((e.width, e.height))
    }

    /// Whether `id`'s window has been shown — the latched answer from
    /// [`Self::mark_presented`], read without changing it.
    pub fn is_presented(&self, id: SurfaceId) -> bool {
        let guard = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        guard.get(&id).is_some_and(|e| e.presented)
    }

    /// Whether the tile standing in for `id` should say that its app is
    /// on its way: nothing has been presented, and it has been waiting
    /// longer than [`OPENING_DELAY`].
    ///
    /// Read by [`ForeignSurfaceWidget::opening`], which is what the
    /// window group consults before deciding to paint the card that holds
    /// the window's place. The latched `presented` wins, so this goes
    /// false on the frame the app has something to show and never comes
    /// back — a placeholder the person has watched go away must not
    /// return.
    pub fn is_opening(&self, id: SurfaceId) -> bool {
        let guard = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .get(&id)
            .is_some_and(|e| !e.presented && e.first_seen.elapsed() >= OPENING_DELAY)
    }

    /// Test-only: pretend this surface was first heard of `age` ago, so a
    /// test can reach the far side of a delay without sleeping through
    /// it. Creates the entry if it does not exist, which is what a
    /// freshly-adopted toplevel amounts to.
    #[cfg(test)]
    pub fn backdate_first_seen(&self, id: SurfaceId, age: Duration) {
        self.with_entry(id, |e| {
            e.first_seen = Instant::now()
                .checked_sub(age)
                .expect("age is a test constant, well inside the clock's range");
        });
    }

    /// Reason the last capture failed, if any.
    pub fn last_error(&self, id: SurfaceId) -> Option<String> {
        let guard = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        guard.get(&id).and_then(|e| e.last_error.clone())
    }

    /// Open this surface's entry at the moment its toplevel is adopted,
    /// so [`ForeignEntry::first_seen`] dates from the window being asked
    /// for rather than from its first buffer.
    ///
    /// Both waits this store bounds — the opening configure in
    /// [`flush_configures`] and the presentation grace in
    /// [`Self::mark_presented`] — run from that instant, and a client
    /// that has been given no configure has no reason to commit a buffer
    /// to open the entry itself. Without this call the clock on the
    /// opening configure would start when the client answered it.
    pub fn note_adopted(&self, id: SurfaceId) {
        self.with_entry(id, |e| {
            // Opening the entry is the whole effect; `first_seen` is
            // stamped by `ForeignEntry::new`. Re-adoption of a surface
            // already known must not restart either clock.
            let _ = &e.first_seen;
        });
    }

    /// Whether this surface has waited longer than
    /// [`OPENING_CONFIGURE_GRACE`] since it was adopted.
    ///
    /// Read only by [`flush_configures`], to decide when to stop waiting
    /// for a laid-out tab and configure the client anyway. A surface with
    /// no entry has not been adopted and is not waiting on anything.
    pub fn opening_wait_expired(&self, id: SurfaceId) -> bool {
        let guard = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        guard
            .get(&id)
            .is_some_and(|e| e.first_seen.elapsed() >= OPENING_CONFIGURE_GRACE)
    }

    /// Record the tab rectangle the surface is currently laid out into,
    /// in physical pixels. Called from the widget's `apply_layout` — the
    /// widget tree is the authority on where a tab lives, so it publishes
    /// rather than the compositor querying.
    pub fn note_placement(&self, id: SurfaceId, rect: Rect) {
        self.with_entry(id, |e| {
            e.placement = Some(rect);
        });
    }

    /// The tab rectangle last published by [`Self::note_placement`].
    pub fn placement(&self, id: SurfaceId) -> Option<Rect> {
        let guard = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        guard.get(&id).and_then(|e| e.placement)
    }

    /// Size of the tab rectangle in physical pixels.
    pub fn viewport(&self, id: SurfaceId) -> Option<(u32, u32)> {
        self.placement(id).map(|r| {
            (
                r.width().max(0.0).round() as u32,
                r.height().max(0.0).round() as u32,
            )
        })
    }

    /// The size the client should be configured to, in surface-local
    /// (logical) pixels, **iff** it differs from the last size we sent.
    /// Records the returned value as sent, so repeated calls with an
    /// unchanged layout return `None` and the client is not spammed with
    /// redundant configures.
    pub fn take_pending_configure(&self, id: SurfaceId, scale: f64) -> Option<(i32, i32)> {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        self.with_entry(id, |e| {
            let rect = e.placement?;
            let w_px = rect.width().max(0.0).round() as u32;
            let h_px = rect.height().max(0.0).round() as u32;
            if w_px == 0 || h_px == 0 {
                return None;
            }
            let logical = (
                ((w_px as f64) / scale).round().max(1.0) as i32,
                ((h_px as f64) / scale).round().max(1.0) as i32,
            );
            if e.configured == Some(logical) {
                return None;
            }
            e.configured = Some(logical);
            Some(logical)
        })
    }

    /// Whether this surface has something worth putting on screen, and
    /// latch the answer once it does.
    ///
    /// Read by [`ForeignSurfaceWidget::presented`], which the shell's
    /// window group consults before painting anything at all — so this
    /// is the predicate deciding when a launched app's window turns up.
    ///
    /// Three ways to become presentable, in the order they are checked:
    ///
    /// - **A capture failed.** Then there is something to say, and the
    ///   tile says it. An app that cannot be shown must not be silently
    ///   invisible.
    /// - **A frame has landed that answers the tab.** Not merely a frame:
    ///   the first buffer a client commits is the size *it* chose, because
    ///   the compositor's opening configure carries no size and the real
    ///   one is only sent once the tab has been laid out. Painting that
    ///   first buffer means [`ForeignSurfaceWidget::fit_rect`] scales it
    ///   to fit — the app appears magnified, then snaps down to 1:1 when
    ///   the correctly-sized buffer arrives a moment later. Waiting for a
    ///   frame sized to the tab is what makes an opened window keep the
    ///   size it opened at. What counts as sized to the tab is [`fills`],
    ///   which also admits a client that kept a strip of that size for
    ///   decorations it draws itself — a client the decoration protocol
    ///   could not talk out of it — because otherwise the answer never
    ///   comes and the window waits out the grace below.
    /// - **[`PRESENT_GRACE`] has passed.** The backstop, and the reason
    ///   this cannot strand a window: an app that never presents, or one
    ///   whose buffers never match its tab, becomes visible anyway and
    ///   shows whatever it has — including the placeholder naming it.
    ///   A window nobody can see is a window nobody can close.
    ///
    /// `tile` is the rect the tab occupies, from the widget that is about
    /// to draw. A degenerate one means the tab has not been laid out yet,
    /// which is not something a frame can be judged against.
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
            let (tab_w, tab_h) = (
                tile.width().round() as u32,
                tile.height().round() as u32,
            );
            // Reaching the backstop is a defect every time: the window
            // was a hole in the strip for ten seconds, and the buffer
            // and tab printed beside it are what has to be reconciled
            // to close it. Said out loud, because the symptom on screen
            // is indistinguishable from an app that was simply slow —
            // which is how a client short by its own title bar went
            // unnoticed while it delayed every launch on this machine.
            if backstop {
                tracing::warn!(
                    target: "shell::compositor::foreign",
                    surface_id = id,
                    waited_ms = waited.as_millis() as u64,
                    buffer_w = e.width,
                    buffer_h = e.height,
                    tab_w,
                    tab_h,
                    "no frame answered this tab; showing the window anyway",
                );
            } else {
                tracing::debug!(
                    target: "shell::compositor::foreign",
                    surface_id = id,
                    waited_ms = waited.as_millis() as u64,
                    buffer_w = e.width,
                    buffer_h = e.height,
                    tab_w,
                    tab_h,
                    "window shown",
                );
            }
            true
        })
    }
}

/// How long a window's place is held in silence before the card saying
/// what is coming to it is put there.
///
/// An app already on this machine can be on screen well inside a quarter
/// of a second, and an indicator that appears and disappears in that time
/// is a flicker rather than information — nobody has begun to wonder yet.
/// Past it the wait is long enough to be a wait, and an empty space where
/// a window was asked for reads as nothing having happened.
pub(crate) const OPENING_DELAY: Duration = Duration::from_millis(250);

/// How long a surface may take to produce a frame that fits its tab
/// before its window is shown regardless.
///
/// Sized against what a launch actually costs: a cold open measured
/// here runs 2-6 s from the invocation to the first buffer, and the
/// notification the launch raised is on screen for all of it. Ten
/// seconds is past every launch observed and still short enough that an
/// app which is never going to paint stops being a hole in the strip.
pub(crate) const PRESENT_GRACE: Duration = Duration::from_secs(10);

/// How long a newly adopted toplevel may go without its tab being laid
/// out before the compositor configures it anyway.
///
/// The ordinary case does not spend this: the tab is spliced into the
/// tree as the toplevel is adopted, the next layout pass gives it a rect,
/// and [`flush_configures`] answers the client on the following frame —
/// one frame, not a quarter of a second.
///
/// What this bounds is the case where no rect ever arrives, which is not
/// hypothetical: a tab collapsed under a maximized sibling, or one in a
/// workspace that is not on screen, is laid out to nothing and publishes
/// no placement. A client that has never been configured cannot map, so
/// waiting on that rect for ever would be a window that never opens at
/// all — strictly worse than the wrong-sized frame this whole path exists
/// to prevent. Long enough that a busy frame or two cannot trip it,
/// short enough that a window nobody laid out still opens promptly.
///
/// The grace can always be reached: adopting a toplevel bumps the chrome
/// revision, which forces the very next paint, and the run loop's
/// one-second keepalive paints an idle desktop regardless. So this is
/// bounded by the clock and not by whether anything else happens to
/// happen.
const OPENING_CONFIGURE_GRACE: Duration = Duration::from_millis(250);

/// Whether a `w`x`h` buffer is this client's answer to the tab it was
/// configured into, rather than the size it picked before being asked.
///
/// Two ways to be an answer, because there are two kinds of client.
///
/// **It covers the tab.** `fit_rect` scales uniformly by the smaller of
/// the two ratios, so a buffer covers the tile exactly when its aspect
/// ratio matches. Sizes are not compared directly because they need not
/// be equal to look right: a buffer twice the tile in both axes is drawn
/// at exactly the tile, sharper and in the same place.
///
/// **It fits inside the tab and matches one of its axes.** A client that
/// draws its own frame keeps the title bar out of the size it was given
/// and commits the remainder, so it answers one axis exactly and comes up
/// short on the other — for ever, because there is no later buffer in
/// which it gives that strip back. Configured 670x668 such a client
/// commits 670x633, which under the covering test alone is not an answer
/// at all, and its window waits out [`PRESENT_GRACE`] — ten seconds of
/// empty tab over an app that painted in two.
///
/// That was every winit client on this compositor until it began
/// advertising `zxdg_decoration_manager_v1` and answering `ServerSide`
/// (see `State`'s `XdgDecorationHandler`), which is the real fix: a
/// client told the compositor decorates keeps nothing back, commits
/// 670x668, and is admitted by the covering arm above with no band left
/// under it. This arm remains as the backstop for the clients that
/// negotiation cannot reach — one that never binds the global, or a
/// toolkit that decorates itself regardless of the answer. They still
/// get shown promptly rather than stranded; what they do not get is a
/// tab whose bottom strip is empty, and no arm of this predicate can
/// give them that. Only the client can, by rendering the size it was
/// asked for.
///
/// Overflowing either axis is what still disqualifies the opening buffer:
/// a client's own idea of its size is not derived from the tab and does
/// not fit inside it. That is the frame this gate exists to keep off
/// screen, and it is kept off screen by the same arm that admits the
/// decorated one.
///
/// The half-percent tolerance absorbs the rounding in the logical-size
/// configure (a 1-px disagreement at 1400 px is 0.07%) without admitting
/// a band anyone could see.
fn fills(w: u32, h: u32, tile: Rect) -> bool {
    if w == 0 || h == 0 || tile.width() <= 0.0 || tile.height() <= 0.0 {
        return false;
    }
    let (bw, bh) = (w as f64, h as f64);
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

/// How far a buffer may disagree with its tab and still be the same
/// size. See [`fills`].
const FIT_TOLERANCE: f64 = 0.005;

// ---------------------------------------------------------------------------
// The widget
// ---------------------------------------------------------------------------

/// Hosts one foreign wayland surface inside a tab.
///
/// Holds no pixel state of its own beyond a cached [`ImageData`] built
/// from the store's most recent frame; the cache is rebuilt only when the
/// store's sequence number moves, so a static client costs nothing per
/// frame beyond the `ImageBrush` fill.
pub struct ForeignSurfaceWidget {
    surface_id: SurfaceId,
    app_id: String,
    rect: Rect,
    /// GPU-uploadable copy of the last frame we drew, plus the store
    /// sequence it came from.
    cached: Option<ImageData>,
    cached_seq: u64,
    /// Stable, human-addressable name so `FrameBuilder::widget_registry`
    /// (and therefore the shell's click/rect resolver) can find the tile.
    test_id: String,
    /// The app's own name, resolved on the frame that first needs it.
    ///
    /// Only the placeholder below reads it, and the placeholder is the
    /// rare path — an app that never showed a frame anyone could use. The
    /// lookup consults this session's launches and then the installed
    /// manifests, so it is not something to repeat on every rebuild of
    /// the widget tree, and not something to pay for at all on the tiles
    /// that never fail.
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
    /// Reached two ways, and both are the end of somebody's wait: a
    /// capture failed, or the app was given every second it could have and
    /// still never produced a frame that fits its tab. It has to be a
    /// visible end either way — a window that quietly stays blank, or that
    /// quietly disappears, leaves a person watching for something that is
    /// not coming.
    ///
    /// So it says what happened, in the words that person would use, under
    /// the app's own name. Why a particular buffer could not be shown is a
    /// fact about buffers: it is kept in [`ForeignFrameStore::last_error`]
    /// for whoever needs it, and it is not put on a card somebody is
    /// reading to find out whether their app opened.
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

    /// Pull a fresh frame out of the store if one has landed since the
    /// last call. Returns `true` when the cache changed.
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
                    // Wayland `ARGB8888` carries premultiplied alpha; the
                    // `X` variants are forced opaque during the swizzle,
                    // for which premultiplied and straight coincide.
                    alpha_type: ImageAlphaType::AlphaPremultiplied,
                    width: frame.width,
                    height: frame.height,
                });
            }
            None => self.cached = None,
        }
        true
    }

    /// Destination rect for the client image inside `self.rect`,
    /// preserving aspect ratio and anchoring the result at the tile's
    /// top-left corner.
    ///
    /// A correctly-configured client renders at exactly the tab size, in
    /// which case this is the identity fit. A letterbox band only shows
    /// during the frame or two between a resize configure and the
    /// client's answering commit — better than a stretched image.
    ///
    /// The anchor is the load-bearing part. Pointer input reaches the
    /// client by subtracting the tile origin from the global cursor
    /// (`client_input::surface_local`), so the client's pixels have to be
    /// painted from that same origin or the two disagree: a frame that is
    /// momentarily the wrong size would otherwise be *centred*, sliding
    /// every pixel the user is looking at by half the mismatch while
    /// input carried on addressing the un-slid position. Anchoring costs
    /// nothing when the sizes match (the centring term is zero) and keeps
    /// paint and input on one origin when they don't, so a press always
    /// lands on the row it appears to land on.
    /// Whether this surface's window is still being held back, and so
    /// the shell has to keep asking.
    ///
    /// The frame counter alone cannot carry this. A client that has
    /// committed nothing leaves it at zero, matching the cache, so the
    /// strip is judged unchanged and never re-recorded — and the ten
    /// second backstop in [`ForeignFrameStore::mark_presented`] is only
    /// ever read from a draw. The window would sit invisible until some
    /// unrelated repaint happened to come along, which is not something
    /// to leave to chance for the case whose whole purpose is to make
    /// sure a window cannot be stranded.
    fn awaiting_first_paint(&self) -> bool {
        !store().is_presented(self.surface_id)
    }

    fn fit_rect(&self, img_w: u32, img_h: u32) -> Option<Rect> {
        if img_w == 0 || img_h == 0 || self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return None;
        }
        let iw = img_w as f64;
        let ih = img_h as f64;
        let scale = (self.rect.width() / iw).min(self.rect.height() / ih);
        let dw = iw * scale;
        let dh = ih * scale;
        let dx = self.rect.x0;
        let dy = self.rect.y0;
        Some(Rect::new(dx, dy, dx + dw, dy + dh))
    }
}

impl Widget for ForeignSurfaceWidget {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(taffy::Style {
                size: taffy::Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .expect("foreign surface leaf node");
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    /// The addresses this window is showing, all at the window's own rect.
    ///
    /// A packaged app is a separate process, so nothing here walked a widget
    /// tree to find the photograph inside it — and the socket that app holds
    /// has no field for saying where it drew one. What *is* known, and known
    /// from the app's own publish rather than guessed, is that these addresses
    /// belong to this connection and this connection is this window. So the
    /// window is the answer, and it is a true one: a mark drawn here is drawn
    /// on the thing that is showing the object being acted on.
    ///
    /// Coarser than an in-process app's answer, which is the photograph's own
    /// rect with the letterbox bands excluded. Closing that gap means letting
    /// an application say where inside its window an address is drawn, which
    /// is a field on the wire rather than anything here — and until it exists,
    /// answering at window granularity is better than not answering, because
    /// an address that resolves to nothing draws nothing and reads exactly
    /// like a desktop where the agent did not act.
    fn resource_rects(&self) -> Vec<(String, Rect)> {
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return Vec::new();
        }
        crate::shell::action_touch::resources_of(&self.app_id)
            .into_iter()
            .map(|id| (id, self.rect))
            .collect()
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(l) = taffy.layout(node) else {
            return;
        };
        let x = origin.x + l.location.x as f64;
        let y = origin.y + l.location.y as f64;
        self.rect = Rect::new(x, y, x + l.size.width as f64, y + l.size.height as f64);
        // Publish the laid-out rect so the compositor's configure pass can
        // tell the client what resolution to render at. This is the
        // production seam between "where the shell put the tab" and "what
        // size the wayland client is asked for".
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
        let radius = theme.tokens.radii.xl as f64;
        let clip = RoundedRect::from_rect(self.rect, radius);

        if let Some(image) = self.cached.clone() {
            if let Some(dst) = self.fit_rect(image.width, image.height) {
                // Neutral backdrop so the letterbox bands (and any client
                // transparency) read as compositor surface rather than
                // whatever the tile card happened to paint.
                scene.fill(
                    Fill::NonZero,
                    transform,
                    theme.tokens.colors.surfaces.base,
                    None,
                    &clip,
                );
                let sx = dst.width() / image.width as f64;
                let sy = dst.height() / image.height as f64;
                let brush_transform = transform
                    * Affine::translate((dst.x0, dst.y0))
                    * Affine::scale_non_uniform(sx, sy);
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
                    &Rect::new(0.0, 0.0, image.width as f64, image.height as f64),
                );
                scene.pop_layer();
                return;
            }
        }

        let (headline, detail) = self.unshowable_text();
        // These were fixed ramp indices, which is a guess about which way
        // round the card is rather than an answer. `neutral[1]` is near-black
        // on the dark ramp and the card under it is `#16171b`, so the app's
        // own name — the one thing on screen saying *what* failed to open —
        // measured 1.03:1 and was invisible; under high contrast the same
        // near-black sat on pure black. `on_card` derives both tiers from the
        // card's actual luminance, which is what this text needed all along.
        let tiers = theme.tokens.colors.on_card();
        let on_card = tiers.ink;
        let on_card_dim = tiers.ink_dim;
        let size = theme.tokens.typography.scale.xl;
        let (tw, _) = text_renderer.measure(&headline, size, None);
        let cx = self.rect.center().x;
        let y = self.rect.center().y - size as f64 / 2.0;
        text_renderer.draw_text(
            scene,
            &headline,
            size,
            on_card,
            transform * Affine::translate((cx - tw as f64 / 2.0, y)),
            None,
        );
        let sm = theme.tokens.typography.scale.sm;
        let (dw, _) = text_renderer.measure(&detail, sm, None);
        text_renderer.draw_text(
            scene,
            &detail,
            sm,
            on_card_dim,
            transform * Affine::translate((cx - dw as f64 / 2.0, y + size as f64 + 8.0)),
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
        // Input routing to client surfaces goes through the seat, not the
        // widget tree.
    }

    fn name(&self) -> &str {
        "ForeignSurfaceWidget"
    }

    /// Whether the client has drawn something that belongs on screen.
    ///
    /// The whole judgement is [`ForeignFrameStore::mark_presented`]; it
    /// lives on the store because the answer has to latch per surface,
    /// and this widget is rebuilt whenever the tab tree is.
    fn presented(&self) -> bool {
        store().mark_presented(self.surface_id, self.rect)
    }

    /// Whether this is a window's place being held for an app that has
    /// been asked for and has not drawn. See [`Widget::opening`] and
    /// [`ForeignFrameStore::is_opening`].
    fn opening(&self) -> bool {
        store().is_opening(self.surface_id)
    }

    fn test_id(&self) -> Option<&str> {
        Some(&self.test_id)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// A pending frame in the store is the only reason this widget needs
    /// the compositor to repaint. Cheap enough to poll every tick and
    /// `false` whenever the client is idle, so the paint-gating budget is
    /// unaffected by a foreign tab that isn't rendering.
    fn needs_animation_frame(&self) -> bool {
        self.awaiting_first_paint() || store().frame_seq(self.surface_id) != self.cached_seq
    }

    fn needs_redraw(&self) -> bool {
        self.awaiting_first_paint() || store().frame_seq(self.surface_id) != self.cached_seq
    }
}

impl AppWidget for ForeignSurfaceWidget {
    fn app_kind(&self) -> AppKind {
        // No `AppKind` describes "arbitrary external client"; `Settings`
        // is the catch-all `to_ui_kind` already uses for unknown kinds.
        AppKind::Settings
    }
    fn app_title(&self) -> &str {
        // The app's name, not its address. This handed back the raw
        // wayland app-id — `dev.qos.chat` — while
        // [`Self::display_name`] sitting directly above it already
        // resolves that to "Chat" through the manifest (and humanises it
        // when no manifest names it). Anything asking a tile what it is
        // called got the reverse-DNS id, which is a thing the system
        // dispatches on and not a thing to say to a person.
        self.display_name()
    }
    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("󰖯")
    }
    fn permission_hint(&self) -> Option<AppKind> {
        None
    }
}

// ---------------------------------------------------------------------------
// Tree mirroring: protocol-side adoptions → rendered chrome tree
// ---------------------------------------------------------------------------

/// One foreign tile as seen in a workspace tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignTab {
    pub surface_id: SurfaceId,
    pub app_id: String,
    pub tab_id: u64,
    pub title: String,
}

/// Every `TileContent::Foreign` tab in `tree`'s active workspace, in
/// traversal order.
pub fn foreign_tabs(tree: &WorkspaceTree) -> Vec<ForeignTab> {
    let mut out = Vec::new();
    let Some(ws) = tree.active_workspace() else {
        return out;
    };
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
    out
}

/// Which window group in the active workspace holds `tab_id`.
///
/// The inverse of the walk [`foreign_tabs`] does: that flattens the
/// tree down to surfaces and loses which group each one was in, and a
/// caller holding a surface id needs the group back to say what a
/// window should be merged *into*.
pub fn group_of_tab(tree: &WorkspaceTree, tab_id: u64) -> Option<u64> {
    let ws = tree.active_workspace()?;
    for strip in &ws.strips {
        for group in &strip.window_groups {
            if group.tabs.iter().any(|t| t.id == tab_id) {
                return Some(group.id);
            }
        }
    }
    None
}

/// Whether `surface_id` is the active tab of its group in the active
/// workspace — i.e. whether its pixels are on screen.
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
/// The compositor's protocol state (`State::workspace_tree`, mutated by
/// `XdgShellHandler`) and the chrome's rendered tree are separate trees —
/// the chrome owns its own `WorkspaceTreeHandle` so widget state survives
/// protocol churn. This copies foreign adoptions and teardowns across, and
/// is the only direction data flows (the chrome never invents foreign
/// tabs).
///
/// Returns `true` when `rendered` was mutated, which is the caller's
/// signal to bump the chrome revision so the strip resync splices the tab
/// into the live widget tree.
pub fn reconcile_foreign_tabs(source: &WorkspaceTree, rendered: &mut WorkspaceTree) -> bool {
    let wanted = foreign_tabs(source);
    let present = foreign_tabs(rendered);
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

    // Add foreign tabs the rendered tree is missing. `adopt_toplevel` is
    // the same production splice the protocol side uses, so placement
    // rules (empty-workspace bootstrap, first strip / first group) stay in
    // exactly one place.
    let present_ids: std::collections::HashSet<SurfaceId> = foreign_tabs(rendered)
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

    // Keep titles / app-ids in step for tabs that exist on both sides
    // (a client can call `set_app_id` after mapping).
    for want in &wanted {
        for ws in rendered.workspaces.iter_mut() {
            for strip in ws.strips.iter_mut() {
                for group in strip.window_groups.iter_mut() {
                    for tab in group.tabs.iter_mut() {
                        if let ShellTile::Foreign { app_id, surface_id } = &mut tab.tile {
                            if *surface_id == want.surface_id && *app_id != want.app_id {
                                *app_id = want.app_id.clone();
                                tab.title = want.title.clone();
                                changed = true;
                            }
                        }
                    }
                }
            }
        }
    }

    changed
}

// ---------------------------------------------------------------------------
// Per-frame compositor passes
// ---------------------------------------------------------------------------

/// Tell every foreign client what pixel size to render at.
///
/// Reads the tab rects the widgets published during the layout pass and
/// sends an `xdg_toplevel.configure` to any client whose size changed
/// (group resize, tab switch, output resize, fullscreen promotion). The
/// size is converted from physical pixels to surface-local logical pixels
/// with the compositor's current display scale, which is the same value
/// pushed to clients through `wp_fractional_scale_v1` on commit.
///
/// This is also where a toplevel gets its **opening** configure, and that
/// is the point of running it here rather than at `get_toplevel`. A tab's
/// size is a fact about the strip, and the strip does not know it until it
/// has laid the tab out; a configure sent any earlier carries no size,
/// which tells the client to choose one, which is how an app came to draw
/// a whole frame at a size the strip never asked for before being
/// corrected. Sending the opening configure from the pass that runs
/// *after* layout means the first size a client is ever given is the size
/// its tab actually has.
///
/// Returns the number of configures sent — used by the wiring test.
pub fn flush_configures(state: &mut crate::compositor::state::State) -> usize {
    let scale = state.scale;
    let fallback = opening_fallback_size(state);
    // `toplevel_surfaces` borrows the shell state; the configure calls
    // below don't touch `State`, so a cheap clone of the handles releases
    // the borrow for the loop.
    let toplevels: Vec<_> = state.xdg_shell_state.toplevel_surfaces().to_vec();
    let mut sent = 0;
    for toplevel in toplevels {
        let id = crate::compositor::state::surface_id(toplevel.wl_surface());
        if let Some((w, h)) = store().take_pending_configure(id, scale) {
            configure_to(&toplevel, w, h);
            sent += 1;
            tracing::debug!(
                target: "shell::compositor::foreign",
                surface_id = id,
                width = w,
                height = h,
                "configured foreign toplevel to its tab size"
            );
            continue;
        }
        // No laid-out size. For a toplevel that has already been
        // configured that is the ordinary case — an unchanged layout must
        // not re-configure the client every frame. For one that has never
        // been configured it means the tab has not been laid out yet, and
        // waiting is right only for as long as the rect might still be
        // coming: a client with no configure at all cannot map, so it
        // cannot be left waiting for ever.
        if toplevel.is_initial_configure_sent() || !store().opening_wait_expired(id) {
            continue;
        }
        let (w, h) = fallback;
        configure_to(&toplevel, w, h);
        sent += 1;
        tracing::warn!(
            target: "shell::compositor::foreign",
            surface_id = id,
            width = w,
            height = h,
            grace_ms = OPENING_CONFIGURE_GRACE.as_millis() as u64,
            "no tab rect for an opening toplevel; configuring it to the \
             strip's full tile so it can map at all",
        );
    }
    sent
}

/// Put `size` in the toplevel's pending state and send it.
///
/// The pending state is what smithay writes into the configure, so the
/// size has to be staged before the send rather than passed to it — and
/// staging it also means any other pending change the client is owed
/// (the decoration mode, notably) rides out in the same event.
fn configure_to(toplevel: &ToplevelSurface, w: i32, h: i32) {
    toplevel.with_pending_state(|pending| {
        pending.size = Some((w, h).into());
    });
    toplevel.send_configure();
}

/// The size to configure an opening toplevel to when its tab was never
/// laid out, in surface-local (logical) pixels.
///
/// An approximation of the tile a full-width window gets: the output,
/// less the chrome around a strip — the top bar above it, the tab bar
/// over the tile, and the wheel disc's slot beside it. It is not the
/// answer the layout would have given, because the layout declined to
/// give one; it is a size the window can honestly render at, close enough
/// to its eventual tile that the correction when a rect does arrive is
/// small, and far better than the client's own idea of how big it is.
fn opening_fallback_size(state: &crate::compositor::state::State) -> (i32, i32) {
    let scale = if state.scale.is_finite() && state.scale > 0.0 {
        state.scale
    } else {
        1.0
    };
    let (out_w, out_h) = state.output_size;
    let logical = |px: f64| ((px / scale).round() as i32).max(1);
    let chrome_w = crate::shell::sidebar_wheel_chrome::SIDEBAR_WIDTH_PX as f64;
    let chrome_h =
        crate::shell::topbar_chrome::TOPBAR_HEIGHT_PX as f64 + ui::widget::tabs::TAB_BAR_HEIGHT;
    (
        logical((out_w as f64 - chrome_w).max(1.0)),
        logical((out_h as f64 - chrome_h).max(1.0)),
    )
}

/// Release the frame throttle for foreign surfaces that are actually on
/// screen.
///
/// Wayland clients that draw on demand (every toolkit worth the name) wait
/// for `wl_surface.frame` before producing their next buffer. Without this
/// pass a client renders exactly one frame and then freezes forever.
/// Callbacks are fired only for the *visible* tab of each group, which is
/// also what keeps a background client from burning CPU rendering frames
/// nobody can see.
///
/// Returns the number of surfaces signalled.
pub fn release_frame_throttle(state: &mut crate::compositor::state::State) -> usize {
    let time_ms = state.start_time.elapsed().as_millis() as u32;
    let toplevels: Vec<_> = state.xdg_shell_state.toplevel_surfaces().to_vec();
    let mut signalled = 0;
    for toplevel in toplevels {
        let surface = toplevel.wl_surface();
        let id = crate::compositor::state::surface_id(surface);
        if !is_visible_tab(&state.workspace_tree, id) {
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
            signalled += 1;
        }
    }
    signalled
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::WorkspaceTree;

    /// Distinct ids per test so the process-wide store can't leak state
    /// between parallel test threads.
    fn sid(n: u64) -> SurfaceId {
        900_000 + n
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

    /// An app already on this machine can be on screen almost at once,
    /// and for that first fraction of a second saying "on its way" would
    /// be a card that appears and disappears before anyone has read it.
    /// Nothing is the right thing to show while the wait is that short.
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

    /// Past the delay the wait is a wait, and an empty space where a
    /// window was asked for reads as nothing having happened.
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

    /// And it stops the instant there is a window, without coming back.
    ///
    /// A placeholder someone has already watched go away must not return
    /// — that is a window blinking into a card and out again, which is
    /// worse to watch than the wait it was covering.
    #[test]
    fn a_surface_that_has_been_shown_is_never_opening_again() {
        let id = sid(52);
        store().forget(id);
        store().backdate_first_seen(id, OPENING_DELAY + Duration::from_millis(50));
        assert!(store().is_opening(id), "precondition: it was still starting");
        store().publish_rgba(id, &vec![0; 1200 * 800 * 4], 1200, 800);
        assert!(store().mark_presented(id, TILE), "precondition: it presented");
        assert!(
            !store().is_opening(id),
            "the window is on screen; nothing may still be saying it is on \
             its way",
        );
        store().forget(id);
    }

    /// What a person reads on a window's own card is the app's name.
    ///
    /// Never the wayland app-id: `dev.qos.file-explorer` is a machine
    /// identifier the user did not choose and does not recognise, and it
    /// is what used to be on screen for the whole of a cold open.
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
    /// This is the last thing standing between somebody and a window that
    /// is never going to fill in, so it has to name the app they asked for
    /// and tell them what happened — and it has to do that without any of
    /// the vocabulary of the thing that went wrong. "waiting for first
    /// frame" over a package id was on screen for the whole of a cold open
    /// once; a person reading it learns neither which app it is nor
    /// whether to keep waiting.
    #[test]
    fn a_window_that_cannot_be_shown_says_so_in_plain_words() {
        let id = sid(54);
        store().forget(id);
        let w = ForeignSurfaceWidget::new(id, "dev.qos.never-launched-fixture".into());

        let (headline, detail) = w.unshowable_text();
        assert_eq!(headline, "Never Launched Fixture", "the app's own name");
        assert_eq!(detail, "This app isn't responding.");

        // A capture that failed is a different thing to say, and the
        // reason it failed is still not the person's problem to read.
        store().note_failure(id, "unsupported dmabuf modifier 0x0100000000000001".into());
        let (headline, detail) = w.unshowable_text();
        assert_eq!(headline, "Never Launched Fixture");
        assert_eq!(detail, "This window can't be shown.");

        for (field, text) in [("headline", &headline), ("detail", &detail)] {
            let lower = text.to_lowercase();
            for jargon in [
                "frame", "buffer", "surface", "dmabuf", "modifier", "wayland",
                "toplevel", "dev.qos", "install",
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

    /// The frame the compositor's opening configure produces: the client
    /// picked its own size, so painting it means scaling it to fit, and
    /// the app appears magnified until the right-sized buffer lands.
    /// That snap is the window "resizing itself" a second after opening.
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

    /// A client that answered the size it was configured to, less the
    /// strip its own toolkit kept for decorations, has arrived.
    ///
    /// This is not a hypothetical. Before the compositor advertised
    /// `zxdg_decoration_manager_v1`, every winit client fell back to
    /// drawing its own frame and reserved the title bar out of the size
    /// it was given: configured 670x668, it committed 670x633 and never
    /// committed anything else. Measured on the launch path with all
    /// four of Files, the word processor, the calculator and Notes.
    ///
    /// Judged on aspect alone that buffer never fits, so the window was
    /// held back until [`PRESENT_GRACE`] ran out — ten seconds of empty
    /// tab over an app that had painted in two.
    ///
    /// Negotiation now stops qOS's own apps ever committing it, but this
    /// case is not retired with them: a client that never binds the
    /// global, or one whose toolkit decorates itself whatever it is
    /// told, produces exactly this buffer and must still be shown when
    /// it does.
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

    /// Any buffer whose aspect matches is drawn at exactly the tab, so
    /// the one that follows it changes nothing the user can see.
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

    /// Resizing a group makes the current frame stop fitting for a frame
    /// or two. A window that answered this by disappearing would be a
    /// worse defect than the one the gate exists to fix.
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

    /// An app that cannot be shown must say so rather than be an
    /// invisible hole in the strip.
    #[test]
    fn a_surface_whose_capture_failed_is_shown_so_it_can_say_why() {
        let id = sid(44);
        store().forget(id);
        store().note_failure(id, "unsupported buffer".into());
        assert!(store().mark_presented(id, TILE));
        store().forget(id);
    }

    /// `fills` is what the gate is made of, and the grace period is what
    /// stops it stranding a window. Both are asserted on the predicate
    /// directly — the ten-second wait itself is not something a test can
    /// sit through, so what is proved here is that a mismatch is the
    /// only thing keeping the window back.
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

        // A client that kept a title bar out of the size it was given:
        // one axis answered exactly, the other short, nothing overflowing.
        assert!(fills(1200, 765, TILE), "short by a title bar it drew itself");
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

    /// A surface asks for frames until it has drawn, and after that only
    /// when a newer one is waiting.
    ///
    /// The first half is the rule `0b3e2451c` ("Keep asking about a
    /// window that has not drawn, so it cannot be stranded") set out to
    /// establish, and this test predates it — it used to assert the
    /// opposite. A client whose first buffer has not arrived is the one
    /// case where nothing else will wake the paint loop, so a widget
    /// that fell silent there left the window blank for good.
    ///
    /// The second half is why the rule is not simply "always": an app
    /// that has something on screen and nothing newer to show must not
    /// hold the paint loop awake.
    #[test]
    fn widget_asks_for_frames_until_it_has_drawn_then_only_for_a_pending_one() {
        let id = sid(7);
        store().forget(id);
        let mut w = ForeignSurfaceWidget::new(id, "org.example.app".into());
        // A 1×1 tab, so the 1×1 frame below is a frame that *fits* it —
        // which is what being presented waits for, rather than merely a
        // frame having landed.
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
        // A square image inside a 2:1 tile: height-limited, anchored at
        // the tile's top-left with the spare width to the right.
        let fit = w.fit_rect(100, 100).expect("fit");
        assert_eq!((fit.width(), fit.height()), (200.0, 200.0));
        assert_eq!(fit.x0, 0.0);
        assert_eq!(fit.y0, 0.0);
        // An exactly-matching client fills the tile with no letterbox.
        let exact = w.fit_rect(400, 200).expect("fit");
        assert_eq!(exact, w.rect);
    }

    /// A press has to land on what the user can see, so the origin the
    /// client's pixels are painted from must be the origin pointer input
    /// is translated by — including while a resize is in flight and the
    /// client's last frame is still the old size.
    #[test]
    fn painted_frame_origin_matches_the_origin_input_is_mapped_with() {
        let tile = Rect::new(288.0, 124.0, 288.0 + 856.0, 124.0 + 868.0);
        let mut w = ForeignSurfaceWidget::new(sid(9), "x".into());
        w.rect = tile;

        // Every size a lagging client might still be presenting: exactly
        // right, short, tall, narrow and wide.
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
        // The identifier propagates; the *title* is humanised from it.
        // `a50d47498` ("Put the app's own name on its tab") made that so,
        // because Files was appearing as `app` and the word processor as
        // `word-processor` — machine identifiers nobody chose, on camera.
        // This test predates it and expected the raw trailing component,
        // which is what a revert would restore and what this fails on.
        assert_eq!(
            t.title, "Late",
            "a tab wears a name, not the last segment of an app id"
        );
    }

    /// The window itself has to answer with the same name its tab does.
    ///
    /// The tab title was fixed once already; the widget standing for the
    /// surface kept handing back the raw wayland app-id from
    /// `app_title()`, so anything that asked the tile what it was called
    /// — rather than reading the tab's stored title — got
    /// `dev.qos.chat`, an address the user never chose. The resolution
    /// was one method away the whole time.
    #[test]
    fn a_foreign_window_answers_with_its_name_not_its_app_id() {
        let w = ForeignSurfaceWidget::new(3, "dev.qos.chat".into());
        assert_eq!(
            w.app_title(),
            "Chat",
            "a window says what it is called, not how it is addressed",
        );

        // A dotted id with a multi-word tail humanises the same way the
        // package registry does, so an app reaches one name by either
        // route.
        let w = ForeignSurfaceWidget::new(4, "dev.qos.word-processor".into());
        assert_eq!(w.app_title(), "Word Processor");
    }

    #[test]
    fn visibility_tracks_the_groups_active_tab() {
        use crate::compositor::tab_binding::{Placement, adopt_toplevel, adopt_toplevel_at};
        crate::shell::launcher::clear_pending_placements();
        let mut tree = WorkspaceTree::bootstrap_demo();
        // A lone selected window, so each adoption opens its own column
        // rather than joining a tabbed group — which is the arrangement
        // this test is about.
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

        // A second launch opens its own group beside the first, so both
        // windows are on screen — that is the point of the strip.
        let _second = adopt_toplevel(&mut tree, 12, None);
        assert!(
            is_visible_tab(&tree, 11),
            "a launch into its own group must not hide the previous window"
        );
        assert!(is_visible_tab(&tree, 12));

        // Stacking into the *same* group is what puts a window behind
        // another — the tab dimension, not the strip one.
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
