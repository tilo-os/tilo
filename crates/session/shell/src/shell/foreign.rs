//! Compositing of foreign (external Wayland client) surfaces into tabs.
//!
//! An external client that binds our `wl_compositor` + `xdg_wm_base` gets
//! its toplevel adopted into the workspace tab tree by
//! [`crate::shell::tab_binding::adopt_toplevel`]; this module turns
//! that bookkeeping entry into pixels on screen:
//!
//! ```text
//!  client commit
//!      ├─ wl_shm  ──► surface_readback::buffer_to_rgba ──┐
//!      └─ dmabuf  ──► commit Barrier ─► fence (loop) ─► capture worker
//!           shared: drawn from the held buffer itself ───┤ or GPU / CPU import
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
//! only until the first buffer is presented. A later client commit is
//! `needs_redraw` (one shot), so an idle hosted app costs zero cadence.
//!
//! dmabuf capture is coalesced per surface and runs outside the compositor
//! thread. A Smithay commit [`Barrier`] keeps each submitted buffer owned by
//! the compositor until its read completes; the pixels are published only
//! when that exact commit is subsequently applied. No thread waits on a
//! client's GPU: a buffer still being rendered is parked until the compositor
//! loop sees its implicit fence signal ([`dmabuf_fence_signalled`]), so a slow
//! client delays only its own commits. On the shared route the worker imports
//! the buffer (once per buffer) and copies nothing: the frame is drawn from the
//! buffer itself, so once its commit applies the compositor holds the buffer
//! ([`ForeignFrameStore::take_unheld_frame`]) and gives it back only after
//! the last frame that drew it finished (`gpu_frames`). The worker tries GPU
//! ([`crate::compositor::gpu_import`], the only route for a Vulkan
//! client whose swapchain images refuse `mmap`), then CPU mmap
//! ([`import_dmabuf_rgba`], for CPU-visible linear buffers or a box with
//! no EGL device) — together covering the whole advertised set
//! (single-plane RGBA under `Modifier::Linear`); anything else is **rejected with a visible on-tile message**, never silently blank.

use std::os::fd::AsRawFd;
use std::sync::Arc;
#[cfg(target_os = "linux")]
use ui::widget::TaffyTreeExt;

use std::collections::HashMap;
#[cfg(any(test, target_os = "linux"))]
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(target_os = "linux")]
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use smithay::backend::allocator::dmabuf::Dmabuf;
#[cfg(target_os = "linux")]
use smithay::backend::allocator::{Buffer as _, Fourcc, Modifier};
#[cfg(target_os = "linux")]
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
#[cfg(target_os = "linux")]
use smithay::reexports::wayland_server::{Client, backend::ClientId};
#[cfg(target_os = "linux")]
use smithay::wayland::compositor::Barrier;
#[cfg(target_os = "linux")]
use smithay::wayland::shell::xdg::ToplevelSurface;

use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{AppKind, AppWidget, IconDescriptor, Widget};

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{Blob, Extend, Fill, ImageAlphaType, ImageBrush, ImageData, ImageFormat};

#[cfg(target_os = "linux")]
use crate::compositor::surface_readback::{
    OwnedShmFrame, ReadbackError, ShmCopy, ShmRegion, ShmShape, apply_owned_shm, buffer_to_rgba,
    copy_shm_damage,
};
use crate::shell::state::{TileContent as ShellTile, WorkspaceTree};
use ui::TextRole;

mod commit_damage;
mod sheets;
pub use sheets::{PointRoute, SheetLink};

/// Wayland `wl_surface` protocol id, widened — same key space the rest of the compositor uses for foreign surfaces.
pub type SurfaceId = u64;

// dmabuf CPU import

/// `struct dma_buf_sync { __u64 flags; }` — `DMA_BUF_IOCTL_SYNC` is `_IOW('b', 0, struct dma_buf_sync)`, i.e. `(_IOC_WRITE << 30) | (8 << 16) | ('b' << 8) | 0`.
#[cfg(target_os = "linux")]
const DMA_BUF_IOCTL_SYNC: libc::c_ulong = 0x4008_6200;
#[cfg(target_os = "linux")]
const DMA_BUF_SYNC_READ: u64 = 1 << 0;
#[cfg(target_os = "linux")]
const DMA_BUF_SYNC_START: u64 = 0;
#[cfg(target_os = "linux")]
const DMA_BUF_SYNC_END: u64 = 1 << 2;

/// Why a dmabuf could not be imported — every variant is reported on the tile itself rather than degrading to a blank surface.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
impl std::error::Error for DmabufImportError {}

/// Whether a buffer carrying `modifier` has a row-major layout the CPU import can read.
///
/// `LINEAR` obviously qualifies. `INVALID` (the "implicit modifier"
/// escape hatch for an `add`-less `zwp_linux_buffer_params_v1.create`) is
/// accepted too, since the compositor only advertises `LINEAR`, so a
/// conforming client dropping the modifier still allocated linear. Anything else is rejected so the tile reports it rather than painting noise.
#[cfg(target_os = "linux")]
fn modifier_is_cpu_readable(modifier: Modifier) -> bool {
    matches!(modifier, Modifier::Linear | Modifier::Invalid)
}

/// Byte layout of one pixel in memory for a DRM `FourCC` — names read the
/// *most significant* byte first of a little-endian word, so `ARGB8888` is `[B, G, R, A]` and `ABGR8888` is `[R, G, B, A]`.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[cfg(any(test, target_os = "linux"))]
enum PixelOrder {
    /// `[B, G, R, A]` — needs an R/B swap to reach RGBA.
    Bgra { opaque: bool },
    /// `[R, G, B, A]` — already RGBA.
    Rgba { opaque: bool },
}

#[cfg(any(test, target_os = "linux"))]
impl PixelOrder {
    #[cfg(target_os = "linux")]
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

/// How the CPU import reads one advertised buffer format.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
#[cfg(any(test, target_os = "linux"))]
enum CpuLayout {
    /// 8-bit RGBA family, swizzled byte for byte.
    Rgba8(PixelOrder),
    /// 10-bit RGB in one little-endian word: `XB30`/`AB30` keep red in the
    /// low bits, `XR30`/`AR30` blue.
    Rgb10 { red_low: bool, opaque: bool },
    /// Half-float RGBA, red first (`AB4H`/`XB4H`).
    Half { opaque: bool },
    /// 10-bit 4:2:0 YUV in two planes (`P010`).
    P010,
}

#[cfg(any(test, target_os = "linux"))]
impl CpuLayout {
    #[cfg(target_os = "linux")]
    fn for_fourcc(code: Fourcc) -> Option<Self> {
        if let Some(order) = PixelOrder::for_fourcc(code) {
            return Some(Self::Rgba8(order));
        }
        match code {
            Fourcc::Xbgr2101010 => Some(Self::Rgb10 {
                red_low: true,
                opaque: true,
            }),
            Fourcc::Abgr2101010 => Some(Self::Rgb10 {
                red_low: true,
                opaque: false,
            }),
            Fourcc::Xrgb2101010 => Some(Self::Rgb10 {
                red_low: false,
                opaque: true,
            }),
            Fourcc::Argb2101010 => Some(Self::Rgb10 {
                red_low: false,
                opaque: false,
            }),
            Fourcc::Xbgr16161616f => Some(Self::Half { opaque: true }),
            Fourcc::Abgr16161616f => Some(Self::Half { opaque: false }),
            Fourcc::P010 => Some(Self::P010),
            _ => None,
        }
    }

    fn planes(self) -> usize {
        if matches!(self, Self::P010) { 2 } else { 1 }
    }

    /// Bytes per pixel of the first plane.
    fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::Rgba8(_) | Self::Rgb10 { .. } => 4,
            Self::Half { .. } => 8,
            Self::P010 => 2,
        }
    }
}

#[cfg(any(test, target_os = "linux"))]
use crate::shell::hdr_surface::half_to_f32;

/// Append one row of a 10-bit or half-float buffer to `texels` as the
/// matching [`crate::shell::hdr_surface::HdrTexelFormat`]: 10-bit words with
/// red in the low bits, half floats red first, padding alpha made opaque.
/// Other layouts carry no HDR plane and append nothing.
#[cfg(any(test, target_os = "linux"))]
fn append_hdr_texels(layout: CpuLayout, src: &[u8], texels: &mut Vec<u8>) {
    match layout {
        CpuLayout::Rgb10 { red_low, opaque } => {
            for px in src.chunks_exact(4) {
                let mut word = px.try_into().map(u32::from_le_bytes).unwrap_or_default();
                if !red_low {
                    let red = word.wrapping_shr(20) & 0x3FF;
                    let blue = word & 0x3FF;
                    word = (word & !0x3FF0_03FF) | red | blue.wrapping_shl(20);
                }
                if opaque {
                    word |= 0xC000_0000;
                }
                texels.extend_from_slice(&word.to_le_bytes());
            }
        }
        CpuLayout::Half { opaque } => {
            for px in src.chunks_exact(8) {
                texels.extend_from_slice(px.get(..6).unwrap_or_default());
                if opaque {
                    texels.extend_from_slice(&0x3C00_u16.to_le_bytes());
                } else {
                    texels.extend_from_slice(px.get(6..8).unwrap_or_default());
                }
            }
        }
        CpuLayout::Rgba8(_) | CpuLayout::P010 => {}
    }
}

/// The texel format [`append_hdr_texels`] writes for `layout`, when it
/// writes any.
#[cfg(any(test, target_os = "linux"))]
fn hdr_texel_format(layout: CpuLayout) -> Option<crate::shell::hdr_surface::HdrTexelFormat> {
    match layout {
        CpuLayout::Rgb10 { .. } => Some(crate::shell::hdr_surface::HdrTexelFormat::Rgb10a2),
        CpuLayout::Half { .. } => Some(crate::shell::hdr_surface::HdrTexelFormat::Rgba16Float),
        CpuLayout::Rgba8(_) | CpuLayout::P010 => None,
    }
}

/// The HDR signal a buffer in `layout` described by `colorimetry` carries,
/// when it carries one worth compositing at full precision.
#[cfg(any(test, target_os = "linux"))]
fn hdr_signal(
    layout: CpuLayout,
    colorimetry: crate::collab::colorimetry::Colorimetry,
) -> Option<crate::shell::hdr_surface::SurfaceSignal> {
    if matches!(layout, CpuLayout::Rgba8(_)) {
        return None;
    }
    crate::shell::hdr_surface::SurfaceSignal::for_colorimetry(
        colorimetry,
        crate::shell::hdr_tonemap::HdrTonemapConfig::default(),
    )
}

/// Convert tight rows of a 10-bit or half-float buffer: RGBA8 for the SDR
/// desktop into `out`, and the HDR plane when the description is HDR.
#[cfg(any(test, target_os = "linux"))]
fn convert_rows<'a>(
    layout: CpuLayout,
    rows: impl Iterator<Item = &'a [u8]>,
    (width, height): (u32, u32),
    colorimetry: crate::collab::colorimetry::Colorimetry,
    out: &mut Vec<u8>,
) -> Option<crate::shell::hdr_surface::HdrPlane> {
    let signal = hdr_signal(layout, colorimetry);
    let format = hdr_texel_format(layout).filter(|_| signal.is_some());
    let mut texels = Vec::new();
    if let Some(format) = format {
        texels.reserve(
            (width as usize)
                .saturating_mul(height as usize)
                .saturating_mul(format.bytes_per_pixel()),
        );
    }
    out.clear();
    out.reserve(
        (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(4),
    );
    for row in rows {
        convert_row(layout, row, colorimetry, out);
        if format.is_some() {
            append_hdr_texels(layout, row, &mut texels);
        }
    }
    crate::shell::hdr_surface::HdrPlane::new(width, height, format?, texels, signal?)
}

/// Convert one row of a 10-bit or half-float buffer to RGBA8: SDR signals
/// quantise to 8 bits, HDR ones (PQ/HLG per the surface's description) are
/// tone-mapped for the SDR desktop.
#[cfg(any(test, target_os = "linux"))]
fn convert_row(
    layout: CpuLayout,
    src: &[u8],
    colorimetry: crate::collab::colorimetry::Colorimetry,
    out: &mut Vec<u8>,
) {
    let tonemap = crate::shell::hdr_tonemap::HdrTonemapConfig::default();
    let mut push = |rgb: [f32; 3], alpha: u8| {
        let [r, g, b] =
            crate::shell::hdr_tonemap::tonemap_rgb_signal_to_srgb8(rgb, colorimetry, tonemap);
        out.extend_from_slice(&[r, g, b, alpha]);
    };
    match layout {
        CpuLayout::Rgba8(order) => swizzle_row(order, src, src.len() / 4, out),
        CpuLayout::Rgb10 { red_low, opaque } => {
            for px in src.chunks_exact(4) {
                let word = u32::from_le_bytes([
                    px.first().copied().unwrap_or(0),
                    px.get(1).copied().unwrap_or(0),
                    px.get(2).copied().unwrap_or(0),
                    px.get(3).copied().unwrap_or(0),
                ]);
                let channel = |shift: u32| {
                    #[allow(
                        clippy::cast_precision_loss,
                        reason = "a 10-bit code is exactly representable in f32"
                    )]
                    let code = (word.wrapping_shr(shift) & 0x3FF) as f32;
                    code / 1023.0
                };
                let (low, mid, high) = (channel(0), channel(10), channel(20));
                let rgb = if red_low {
                    [low, mid, high]
                } else {
                    [high, mid, low]
                };
                let alpha_bits = u8::try_from(word.wrapping_shr(30) & 0b11).unwrap_or(3);
                push(
                    rgb,
                    if opaque {
                        0xFF
                    } else {
                        alpha_bits.saturating_mul(85)
                    },
                );
            }
        }
        CpuLayout::Half { opaque } => {
            for px in src.chunks_exact(8) {
                let half = |index: usize| {
                    half_to_f32(u16::from_le_bytes([
                        px.get(index).copied().unwrap_or(0),
                        px.get(index.saturating_add(1)).copied().unwrap_or(0),
                    ]))
                };
                let rgb = [half(0), half(2), half(4)].map(|c| if c.is_finite() { c } else { 0.0 });
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "alpha is clamped to 0..=255 before the cast"
                )]
                let alpha = (half(6).clamp(0.0, 1.0) * 255.0).round() as u8;
                push(rgb, if opaque { 0xFF } else { alpha });
            }
        }
        // Two planes; converted by `convert_p010` instead.
        CpuLayout::P010 => {}
    }
}

/// Convert one row of `width` pixels laid out per `order` into RGBA,
/// appending to `out` — a free function so unit tests can pin the channel mapping without a GPU or wayland connection.
#[cfg(any(test, target_os = "linux"))]
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
#[cfg(target_os = "linux")]
pub fn validate_importable(dmabuf: &Dmabuf) -> Result<(), DmabufImportError> {
    let format = dmabuf.format();
    if !modifier_is_cpu_readable(format.modifier) {
        return Err(DmabufImportError::NonLinearModifier(u64::from(
            format.modifier,
        )));
    }
    let layout = CpuLayout::for_fourcc(format.code)
        .ok_or(DmabufImportError::UnsupportedFormat(format.code))?;
    let planes = dmabuf.num_planes();
    if planes != layout.planes() {
        return Err(DmabufImportError::MultiPlanar(planes));
    }
    Ok(())
}

/// CPU-import `dmabuf` into `out` as row-major RGBA, returning its `(width, height)`.
///
/// # Errors
///
/// Returns [`DmabufImportError`] for the same reasons as
/// [`validate_importable`], plus degenerate width/height/stride
/// (`InvalidDimensions`) or a failed `mmap` (`MapFailed`).
#[cfg(target_os = "linux")]
pub fn import_dmabuf_rgba(
    dmabuf: &Dmabuf,
    out: &mut Vec<u8>,
    colorimetry: Option<crate::collab::colorimetry::Colorimetry>,
    hdr: &mut Option<crate::shell::hdr_surface::HdrPlane>,
) -> Result<(u32, u32), DmabufImportError> {
    validate_importable(dmabuf)?;
    let format = dmabuf.format();
    let layout = CpuLayout::for_fourcc(format.code)
        .ok_or(DmabufImportError::UnsupportedFormat(format.code))?;
    let colorimetry =
        colorimetry.unwrap_or_else(crate::collab::colorimetry::Colorimetry::sdr_bt709);
    *hdr = None;
    if layout == CpuLayout::P010 {
        return import_p010(dmabuf, out, colorimetry, hdr);
    }

    let width = dmabuf.width();
    let height = dmabuf.height();
    let stride = dmabuf.strides().next().unwrap_or(0);
    let offset = dmabuf.offsets().next().unwrap_or(0);
    let row_bytes = (width as usize).saturating_mul(layout.bytes_per_pixel() as usize);
    if width == 0 || height == 0 || (stride as usize) < row_bytes {
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

    // SAFETY: the mapping covers `[0, map_len)`; every row slice below is
    // inside `[offset, offset + stride*height) ⊆ [0, map_len)`, checked
    // above. The slice is dropped before `munmap`.
    let mapped: &[u8] = unsafe { std::slice::from_raw_parts(ptr as *const u8, map_len) };
    // Proven in range by the `stride >= width*4` and `offset +
    // stride*height == map_len` checks above; `.get()` and a skip keep this
    // panic-free if that invariant is ever broken instead.
    let rows = (0..height as usize).filter_map(|row| {
        let start = (offset as usize).saturating_add(row.saturating_mul(stride as usize));
        mapped.get(start..start.saturating_add(row_bytes))
    });
    *hdr = convert_rows(layout, rows, (width, height), colorimetry, out);

    sync_dmabuf(raw_fd, DMA_BUF_SYNC_END | DMA_BUF_SYNC_READ);
    // SAFETY: `ptr`/`map_len` are exactly what `mmap` returned above and
    // `mapped` is out of scope for any further use.
    unsafe {
        libc::munmap(ptr, map_len);
    }
    Ok((width, height))
}

/// CPU-import a linear two-plane `P010` buffer (luma, then interleaved
/// chroma, 10 bits in the top of each `u16`) into RGBA8: BT.2020 YCbCr,
/// tone-mapped when the surface describes a PQ or HLG signal.
#[cfg(target_os = "linux")]
fn import_p010(
    dmabuf: &Dmabuf,
    out: &mut Vec<u8>,
    colorimetry: crate::collab::colorimetry::Colorimetry,
    hdr: &mut Option<crate::shell::hdr_surface::HdrPlane>,
) -> Result<(u32, u32), DmabufImportError> {
    let width = dmabuf.width();
    let height = dmabuf.height();
    let invalid = |stride: u32| DmabufImportError::InvalidDimensions {
        width,
        height,
        stride,
    };
    let strides: Vec<u32> = dmabuf.strides().collect();
    let offsets: Vec<u32> = dmabuf.offsets().collect();
    let fds: Vec<i32> = dmabuf.handles().map(|fd| fd.as_raw_fd()).collect();
    let (Some(&y_stride), Some(&uv_stride)) = (strides.first(), strides.get(1)) else {
        return Err(DmabufImportError::MultiPlanar(strides.len()));
    };
    if width == 0 || height == 0 || y_stride < width.saturating_mul(2) || uv_stride < width {
        return Err(invalid(y_stride));
    }
    let chroma_rows = height.div_ceil(2);
    let plane = |index: usize, stride: u32, rows: u32| -> Result<Vec<u16>, DmabufImportError> {
        let fd = *fds
            .get(index)
            .or_else(|| fds.first())
            .ok_or(DmabufImportError::MultiPlanar(0))?;
        let offset = offsets.get(index).copied().unwrap_or(0) as usize;
        let len = (stride as usize).saturating_mul(rows as usize);
        let map_len = offset.checked_add(len).ok_or_else(|| invalid(stride))?;
        // SAFETY: as in `import_dmabuf_rgba`: a read-only shared mapping of
        // a live dma-buf fd owned by `dmabuf`, unmapped before returning.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                map_len,
                libc::PROT_READ,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        if ptr == libc::MAP_FAILED {
            let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            return Err(DmabufImportError::MapFailed(errno));
        }
        sync_dmabuf(fd, DMA_BUF_SYNC_START | DMA_BUF_SYNC_READ);
        // SAFETY: the mapping covers `[0, map_len)` and the slice is dropped
        // before `munmap`.
        let mapped: &[u8] = unsafe { std::slice::from_raw_parts(ptr as *const u8, map_len) };
        let samples = mapped
            .get(offset..map_len)
            .unwrap_or_default()
            .chunks_exact(2)
            .map(|pair| {
                u16::from_le_bytes([
                    pair.first().copied().unwrap_or(0),
                    pair.get(1).copied().unwrap_or(0),
                ])
            })
            .collect();
        sync_dmabuf(fd, DMA_BUF_SYNC_END | DMA_BUF_SYNC_READ);
        // SAFETY: exactly the pointer and length `mmap` returned.
        unsafe {
            libc::munmap(ptr, map_len);
        }
        Ok(samples)
    };
    let luma = plane(0, y_stride, height)?;
    let chroma = plane(1, uv_stride, chroma_rows)?;
    let rgba = crate::shell::hdr_tonemap::tonemap_p010_to_rgba8(
        &luma,
        &chroma,
        y_stride / 2,
        uv_stride / 2,
        width,
        height,
        colorimetry,
        crate::shell::hdr_tonemap::HdrTonemapConfig::default(),
    );
    out.clear();
    out.extend_from_slice(&rgba);
    *hdr = hdr_signal(CpuLayout::P010, colorimetry).and_then(|signal| {
        crate::shell::hdr_surface::HdrPlane::p010(
            &luma,
            &chroma,
            (y_stride / 2, uv_stride / 2),
            (width, height),
            signal,
        )
    });
    Ok((width, height))
}

#[cfg(target_os = "linux")]
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
    /// A real compositor toplevel was adopted; a layout-only placeholder is not live.
    adopted: bool,
    damage_history: commit_damage::History,
    #[cfg(any(test, target_os = "linux"))]
    capture_damage: HashMap<u64, u64>,
    /// Most recent frame, row-major RGBA (`width*height*4` bytes). Shared,
    /// never written once published: a snapshot or the tile's image holds it
    /// without a copy, and a new frame replaces the `Arc` under the lock
    /// rather than copying pixels there.
    rgba: Arc<Vec<u8>>,
    /// The last commit seen on this surface, when it was an SHM buffer the
    /// capture worker builds: the next SHM commit's damage is relative to
    /// exactly that commit, so only damaged pixels are copied out of the
    /// client's pool, tagged with its generation for the worker to verify.
    /// `None` (any other commit, an unmap, a failure, a frame published by
    /// another route) makes the next copy whole.
    #[cfg(target_os = "linux")]
    shm_base: Option<ShmBaseCommit>,
    /// Bumped whenever `shm_base` is cleared, so a copy started against a
    /// base that was cleared meanwhile does not reinstate it.
    #[cfg(target_os = "linux")]
    shm_epoch: u64,
    width: u32,
    height: u32,
    /// Monotonic frame counter (`0` = no frame yet), bumped on capture *and* unmap so a sequence comparison always notices a transition.
    seq: u64,
    /// Human-readable reason the last capture failed, drawn on the tile so an unsupported buffer is visible, not silently blank.
    last_error: Option<String>,
    /// The tab's rectangle in the logical desktop, published by `apply_layout`; `None` until laid out.
    placement: Option<Rect>,
    /// A compositor-imposed rectangle that outranks the tab's own while it
    /// is set — the presenter view moved onto its own screen. The tab keeps
    /// publishing into `placement`, so clearing this falls straight back to
    /// the tab size.
    pinned: Option<Rect>,
    /// Size most recently sent to the client in an `xdg_toplevel` configure, in surface-local (logical) pixels.
    configured: Option<(i32, i32)>,
    /// That configure in the tab's (logical) units — smaller than the tab when the client's maximum size clamped it, which is what centres the window in its tab.
    configured_extent: Option<(f64, f64)>,
    /// The client's `xdg_toplevel.set_min_size`/`set_max_size`, logical pixels; `0` on an axis leaves it unconstrained.
    min_size: (i32, i32),
    max_size: (i32, i32),
    /// What the client declared its window to be with `xdg_surface.set_window_geometry` — everything outside it is shadow or resize border, which a tile never shows.
    geometry: Option<WindowGeometry>,
    /// When this surface was first heard of (toplevel adoption) — start of the window [`ForeignFrameStore::mark_presented`] waits before giving up.
    first_seen: Instant,
    /// Whether the client has committed a non-null buffer. This is deliberately
    /// independent of `presented`: Wayland keyboard focus needs a mapped
    /// surface, while the visual anti-spoof gate may still wait for a frame
    /// that answers the current configure.
    mapped: bool,
    /// Latched: this surface has produced something worth showing, so its
    /// window is on screen and stays there. One-way on purpose: the
    /// predicate asks whether the current frame fits its tab, which
    /// briefly stops being true on every resize — un-presenting would blink the window out mid-drag, worse than what this prevents.
    presented: bool,
    /// Capture token attached to each not-yet-applied buffer commit. The
    /// commit's Smithay barrier keeps the client from reusing that buffer
    /// until its worker job either completes or is explicitly superseded.
    #[cfg(any(test, target_os = "linux"))]
    capture_tokens: VecDeque<u64>,
    /// Worker results wait here until the corresponding blocked surface commit
    /// is applied. Publishing earlier would expose pending state as current.
    #[cfg(any(test, target_os = "linux"))]
    ready_captures: HashMap<u64, ReadyCapture>,
    /// Shm commits queued for the capture worker, and the latest it is done
    /// with. The worker keeps only a surface's newest job, so it can finish
    /// fewer than were queued; the newest one finished is what counts.
    shm_queued: u64,
    shm_done: u64,
    activated_capture: Option<u64>,
    /// The image description the client attached (`wp_color_management`),
    /// read when a 10-bit or YUV buffer is converted for display.
    colorimetry: Option<crate::collab::colorimetry::Colorimetry>,
    /// The current frame at full precision when it carries an HDR signal;
    /// `rgba` is then its tone-mapped SDR rendition.
    hdr: Option<Arc<crate::shell::hdr_surface::HdrPlane>>,
    /// The current frame when it stayed on the GPU; `rgba` is then empty and
    /// read back only on demand.
    gpu: Option<Arc<crate::shell::gpu_frames::GpuFrame>>,
    /// `gpu`, while it is drawn from a client buffer the compositor has not
    /// taken hold of yet: set when its commit applies, taken by the commit
    /// handler that holds the buffer ([`ForeignFrameStore::take_unheld_frame`]).
    unheld: Option<Arc<crate::shell::gpu_frames::GpuFrame>>,
}

impl ForeignEntry {
    /// Make `frame` the current GPU frame. The one it replaces is superseded:
    /// its buffer goes back once nothing draws it any more.
    fn set_gpu(&mut self, frame: Option<Arc<crate::shell::gpu_frames::GpuFrame>>) {
        self.unheld = None;
        if let Some(replaced) = std::mem::replace(&mut self.gpu, frame) {
            replaced.note_superseded();
        }
    }

    /// Drop the CPU frame; the next SHM commit then copies its whole buffer.
    /// Returned to be freed outside the lock.
    /// Make `pixels`, a captured `size` frame, the current one. Returns the
    /// frame it replaces, for the caller to drop outside the lock.
    #[cfg(target_os = "linux")]
    fn publish_frame(
        &mut self,
        pixels: Arc<Vec<u8>>,
        (width, height): (u32, u32),
        generation: u64,
    ) -> Arc<Vec<u8>> {
        let replaced = std::mem::replace(&mut self.rgba, pixels);
        // Protocol commit/unmap owns mapping. A late worker completion
        // cannot map a pending buffer early or remap a detached surface.
        self.hdr = None;
        self.set_gpu(None);
        self.width = width;
        self.height = height;
        self.seq = self.seq.wrapping_add(1);
        self.damage_history
            .publish(self.seq, generation, (width, height));
        self.last_error = None;
        replaced
    }

    /// The last capture failed: drop the stale frame and keep `reason`.
    /// Returns the pixels it held, for the caller to drop outside the lock.
    fn fail(&mut self, reason: String) -> Arc<Vec<u8>> {
        let released = self.take_pixels();
        self.hdr = None;
        self.set_gpu(None);
        self.width = 0;
        self.height = 0;
        self.seq = self.seq.wrapping_add(1);
        self.last_error = Some(reason);
        released
    }

    fn take_pixels(&mut self) -> Arc<Vec<u8>> {
        #[cfg(target_os = "linux")]
        self.forget_shm_base();
        std::mem::take(&mut self.rgba)
    }

    /// The next SHM commit is not laid over the last one: it copies whole.
    #[cfg(target_os = "linux")]
    fn forget_shm_base(&mut self) {
        if self.shm_base.take().is_some() {
            self.shm_epoch = self.shm_epoch.wrapping_add(1);
            // The worker can now release the frame it kept to build on.
            if let Some(Ok(worker)) = ASYNC_SHM_CAPTURE.get() {
                worker.poke();
            }
        }
    }

    /// A committed buffer whose pixels are not this entry's yet.
    fn capturing(&self) -> bool {
        #[cfg(any(test, target_os = "linux"))]
        if !self.capture_tokens.is_empty() {
            return true;
        }
        self.shm_queued > self.shm_done
    }

    fn new() -> Self {
        Self {
            adopted: false,
            damage_history: commit_damage::History::default(),
            #[cfg(any(test, target_os = "linux"))]
            capture_damage: HashMap::new(),
            rgba: Arc::default(),
            #[cfg(target_os = "linux")]
            shm_base: None,
            #[cfg(target_os = "linux")]
            shm_epoch: 0,
            width: 0,
            height: 0,
            seq: 0,
            last_error: None,
            placement: None,
            pinned: None,
            configured: None,
            configured_extent: None,
            min_size: (0, 0),
            max_size: (0, 0),
            geometry: None,
            first_seen: Instant::now(),
            mapped: false,
            presented: false,
            colorimetry: None,
            hdr: None,
            gpu: None,
            unheld: None,
            #[cfg(any(test, target_os = "linux"))]
            capture_tokens: VecDeque::new(),
            #[cfg(any(test, target_os = "linux"))]
            ready_captures: HashMap::new(),
            shm_queued: 0,
            shm_done: 0,
            activated_capture: None,
        }
    }
}

/// A toplevel's declared window geometry together with what it takes to
/// find that rectangle in the buffer: geometry is in surface-local logical
/// pixels, the buffer is in its own, and the two differ by the viewport
/// destination (fractional scaling) or else the integer buffer scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowGeometry {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    /// `wp_viewport.set_destination`, when the client uses one.
    pub logical_size: Option<(i32, i32)>,
    /// `wl_surface.set_buffer_scale`.
    pub buffer_scale: i32,
}

impl WindowGeometry {
    /// The declared window as a `(x, y, width, height)` crop of a
    /// `buffer_w`×`buffer_h` buffer, or `None` when it covers the whole buffer
    /// or does not describe any part of it.
    fn crop(&self, buffer_w: u32, buffer_h: u32) -> Option<(u32, u32, u32, u32)> {
        if self.width <= 0 || self.height <= 0 || buffer_w == 0 || buffer_h == 0 {
            return None;
        }
        let fallback = f64::from(self.buffer_scale.max(1));
        let (factor_x, factor_y) = match self.logical_size {
            Some((lw, lh)) if lw > 0 && lh > 0 => (
                f64::from(buffer_w) / f64::from(lw),
                f64::from(buffer_h) / f64::from(lh),
            ),
            _ => (fallback, fallback),
        };
        let (x0, x1) = crop_span(self.x, self.width, factor_x, buffer_w);
        let (y0, y1) = crop_span(self.y, self.height, factor_y, buffer_h);
        let (w, h) = (x1.saturating_sub(x0), y1.saturating_sub(y0));
        if w == 0 || h == 0 || (w, h) == (buffer_w, buffer_h) {
            return None;
        }
        Some((x0, y0, w, h))
    }
}

/// One axis of [`WindowGeometry::crop`]: the logical `origin..origin+extent`
/// in buffer pixels, clamped to the buffer.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "both ends are rounded and clamped into [0, limit] with limit a u32, so neither \
              cast truncates a fraction, loses a sign or wraps"
)]
fn crop_span(origin: i32, extent: i32, factor: f64, limit: u32) -> (u32, u32) {
    let limit = f64::from(limit);
    let start = (f64::from(origin) * factor).round().clamp(0.0, limit);
    let end = (f64::from(origin.saturating_add(extent)) * factor)
        .round()
        .clamp(start, limit);
    (start as u32, end as u32)
}

impl ForeignEntry {
    /// The part of the buffer that is the window: the declared geometry when
    /// there is one, otherwise all of it. `(x, y, width, height)` in buffer pixels.
    fn content(&self) -> (u32, u32, u32, u32) {
        self.geometry
            .and_then(|geometry| geometry.crop(self.width, self.height))
            .unwrap_or((0, 0, self.width, self.height))
    }

    /// Where in `tile` the window belongs: all of it, unless the client's
    /// maximum size kept it smaller, in which case it sits centred.
    fn target_box(&self, tile: Rect) -> Rect {
        let Some((w, h)) = self.configured_extent else {
            return tile;
        };
        // Rounding between logical and physical pixels leaves up to a pixel of
        // disagreement with an unconstrained tab; that is not a smaller window.
        if w >= tile.width() - 1.0 && h >= tile.height() - 1.0 {
            return tile;
        }
        let (w, h) = (w.min(tile.width()), h.min(tile.height()));
        let x = tile.x0 + ((tile.width() - w) / 2.0).round();
        let y = tile.y0 + ((tile.height() - h) / 2.0).round();
        Rect::new(x, y, x + w, y + h)
    }
}

/// Copy the `(x, y, w, h)` region out of a row-major RGBA frame.
fn crop_rgba(rgba: &[u8], width: u32, (x, y, w, h): (u32, u32, u32, u32)) -> Vec<u8> {
    let stride = usize::try_from(width)
        .unwrap_or(usize::MAX)
        .saturating_mul(4);
    let row_bytes = usize::try_from(w).unwrap_or(usize::MAX).saturating_mul(4);
    let left = usize::try_from(x).unwrap_or(usize::MAX).saturating_mul(4);
    let mut out = Vec::with_capacity(row_bytes.saturating_mul(usize::try_from(h).unwrap_or(0)));
    for row in y..y.saturating_add(h) {
        let start = usize::try_from(row)
            .unwrap_or(usize::MAX)
            .saturating_mul(stride)
            .saturating_add(left);
        if let Some(line) = rgba.get(start..start.saturating_add(row_bytes)) {
            out.extend_from_slice(line);
        }
    }
    out
}

/// A copy of a surface's most recent frame, handed to the render side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameSnapshot {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub seq: u64,
    /// The frame at full precision, the same size as `rgba`, when it
    /// carries an HDR signal and nothing is composited over it.
    pub hdr: Option<Arc<crate::shell::hdr_surface::HdrPlane>>,
}

/// A subsurface's pixels, tightly packed RGBA.
struct ChildPixels<'a> {
    rgba: &'a [u8],
    width: u32,
    height: u32,
}

/// Where a subsurface's pixels are.
enum ChildSource {
    Cpu(Arc<Vec<u8>>),
    Gpu(Arc<crate::shell::gpu_frames::GpuFrame>),
}

fn composite_child_rgba(root: &mut FrameSnapshot, child: &ChildPixels<'_>, x: i32, y: i32) {
    if child.rgba.is_empty() || child.width == 0 || child.height == 0 {
        return;
    }
    for child_y in 0..child.height {
        let destination_y = y.saturating_add(i32::try_from(child_y).unwrap_or(i32::MAX));
        let Ok(destination_y) = u32::try_from(destination_y) else {
            continue;
        };
        if destination_y >= root.height {
            continue;
        }
        for child_x in 0..child.width {
            let destination_x = x.saturating_add(i32::try_from(child_x).unwrap_or(i32::MAX));
            let Ok(destination_x) = u32::try_from(destination_x) else {
                continue;
            };
            if destination_x >= root.width {
                continue;
            }
            let pixel_offset = |row: u32, column: u32, width: u32| {
                usize::try_from(
                    row.saturating_mul(width)
                        .saturating_add(column)
                        .saturating_mul(4),
                )
                .unwrap_or(usize::MAX)
            };
            let source = pixel_offset(child_y, child_x, child.width);
            let destination = pixel_offset(destination_y, destination_x, root.width);
            let (Some(source), Some(destination)) = (
                child.rgba.get(source..source.saturating_add(4)),
                root.rgba
                    .get_mut(destination..destination.saturating_add(4)),
            ) else {
                continue;
            };
            let (Ok(&[sr, sg, sb, sa]), Ok([dr, dg, db, da])) = (
                <&[u8; 4]>::try_from(source),
                <&mut [u8; 4]>::try_from(destination),
            ) else {
                continue;
            };
            let alpha = u32::from(sa);
            let inverse = 255_u32.saturating_sub(alpha);
            let blend = |over: u8, under: u8| {
                u8::try_from(
                    u32::from(over)
                        .saturating_mul(alpha)
                        .saturating_add(u32::from(under).saturating_mul(inverse))
                        / 255,
                )
                .unwrap_or(u8::MAX)
            };
            *dr = blend(sr, *dr);
            *dg = blend(sg, *dg);
            *db = blend(sb, *db);
            *da = u8::try_from(alpha.saturating_add(u32::from(*da).saturating_mul(inverse) / 255))
                .unwrap_or(u8::MAX);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildFrame {
    pub surface: SurfaceId,
    pub x: i32,
    pub y: i32,
}

/// Process-wide capture + placement registry for foreign surfaces.
///
/// A singleton rather than a field threaded through the widget tree,
/// since `crate::shell::to_ui_tile` is reached from call sites with no
/// compositor context. Keying on the wayland protocol id keeps state trivially partitioned per surface.
#[derive(Default)]
pub struct ForeignFrameStore {
    entries: Mutex<HashMap<SurfaceId, ForeignEntry>>,
    painted_frames: Mutex<HashMap<SurfaceId, u64>>,
    presentation_viewport: Mutex<Option<Rect>>,
    children: Mutex<HashMap<SurfaceId, Vec<ChildFrame>>>,
    /// Transient toplevels drawn on their parent's tile, bottom to top.
    sheets: Mutex<Vec<SheetLink>>,
    /// Total completed buffer imports, of any route. A dmabuf import costs a
    /// GPU readback (or full-frame CPU map), so burst commits are coalesced and
    /// paint only reads the latest completed pixels.
    imports: AtomicU64,
    #[cfg(target_os = "linux")]
    next_capture: AtomicU64,
    /// Which surfaces a capture must draw as a placeholder — see
    /// [`crate::shell::capture_protection`].
    capture_protection: Mutex<CaptureProtection>,
    /// Advanced after every change to [`Self::capture_protection`], so the
    /// compositor can tell that nothing changed without taking the lock.
    capture_protection_epoch: AtomicU64,
    /// The size, in logical pixels, other devices of the person show each
    /// streamed window at. A window not shown here is configured to it.
    remote_extents: Mutex<HashMap<SurfaceId, (i32, i32)>>,
}

/// Capture-protection requests, kept beside the frames they withhold.
#[derive(Default)]
struct CaptureProtection {
    /// Authenticated shell-ui connections that asked for their app's windows
    /// to be protected, with the app id the launcher vouched for. Keyed by
    /// connection so one instance closing does not expose another's windows.
    apps: HashMap<u64, String>,
    /// Surfaces the compositor protects on their own account, such as the
    /// popups of a protected window.
    surfaces: std::collections::HashSet<SurfaceId>,
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
    /// Imported on the presenting GPU and copied into a compositor-owned
    /// texture; no pixel crossed the CPU.
    Shared,
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
    /// A dmabuf was handed to the latest-frame worker. The compositor thread
    /// did not wait for its GPU fence or copy its pixels.
    Queued,
    /// Neither path could produce pixels; the reason is recorded on the entry and drawn on the tile.
    Failed(String),
}

#[derive(Debug)]
#[cfg(target_os = "linux")]
struct CaptureJob<T> {
    surface: SurfaceId,
    token: u64,
    payload: T,
    barrier: Barrier,
    client: Client,
    /// The client's rendering into the buffer is known to be done. `false`
    /// only when its fence could not be watched on the compositor loop; the
    /// worker then waits for it.
    readable: bool,
}

#[cfg(target_os = "linux")]
type DmabufJob = CaptureJob<Dmabuf>;

#[cfg(target_os = "linux")]
struct ShmJob {
    surface: SurfaceId,
    /// The surface's shm capture count when this job was queued.
    generation: u64,
    damage_generation: u64,
    /// The copies of every queued commit the worker has not applied yet,
    /// oldest first. Only the first can be whole: a whole copy makes every
    /// older one moot. A damaged-only copy is laid over the frame before it,
    /// so a newer commit cannot simply replace one.
    patches: Vec<ShmPatch>,
}

/// The entry `job` was queued for, unless its surface went while the job
/// ran. A client that commits and closes its window at once leaves the worker
/// building a frame for a surface that is gone; `forget` removed the entry,
/// but a closing tile still placing itself makes an empty one again, and
/// publishing into that would keep a whole frame nothing frees. That entry has
/// queued no SHM job, so the job's generation tells it apart.
#[cfg(target_os = "linux")]
fn shm_job_entry<'a>(
    entries: &'a mut HashMap<SurfaceId, ForeignEntry>,
    job: &ShmJob,
) -> Option<&'a mut ForeignEntry> {
    entries
        .get_mut(&job.surface)
        .filter(|entry| job.generation != 0 && job.generation <= entry.shm_queued)
}

/// One SHM commit's copy, and what it was copied against.
#[cfg(target_os = "linux")]
struct ShmPatch {
    frame: OwnedShmFrame,
    /// The commit's capture generation.
    generation: u64,
    /// For a damaged-only copy, the generation of the SHM commit its damage
    /// is relative to. The worker lays it only over exactly that commit's
    /// frame; any other base drops it rather than guess.
    based_on: Option<u64>,
}

#[cfg(target_os = "linux")]
impl ShmJob {
    fn copied_bytes(&self) -> usize {
        self.patches
            .iter()
            .map(|patch| patch.frame.copied_bytes())
            .fold(0, usize::saturating_add)
    }

    fn starts_whole(&self) -> bool {
        self.patches
            .first()
            .is_none_or(|patch| patch.frame.regions().is_none())
    }

    /// Take `older`, still queued, in front of this job's copies. Returns the
    /// copies that no longer matter.
    fn absorb(&mut self, older: Self) -> Vec<ShmPatch> {
        if self.starts_whole() {
            return older.patches;
        }
        let mut patches = older.patches;
        patches.append(&mut self.patches);
        self.patches = patches;
        Vec::new()
    }
}

/// A surface's SHM base as a copy read it, before the copy was made.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug)]
struct ShmBaseRead {
    base: Option<ShmBaseCommit>,
    /// The base epoch then: a copy only becomes the next base if no clear
    /// happened in between.
    epoch: u64,
}

/// Bytes of a tightly packed RGBA frame of `shape`.
#[cfg(target_os = "linux")]
fn frame_bytes(shape: ShmShape) -> usize {
    shape.width.saturating_mul(shape.height).saturating_mul(4)
}

/// The SHM commit a surface's next damaged commit is laid over.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ShmBaseCommit {
    shape: ShmShape,
    generation: u64,
}

/// What the SHM worker keeps of one surface: the frame its last SHM commit
/// built, which the next damaged commit is laid over. It is independent of
/// what the frame store shows, which a dmabuf or an unmap may have replaced
/// meanwhile; only a commit tagged with `generation` is laid over it.
#[cfg(target_os = "linux")]
struct ShmSurface {
    pixels: Arc<Vec<u8>>,
    shape: ShmShape,
    generation: u64,
    /// The frame `pixels` replaced, written into next instead of a full copy
    /// when nothing draws it any more.
    spare: Option<ShmSpare>,
}

/// A frame kept for reuse after a newer one replaced it.
#[cfg(target_os = "linux")]
struct ShmSpare {
    pixels: Arc<Vec<u8>>,
    /// The regions it lacks against its successor; `None`: unknown, all.
    stale: Option<Vec<ShmRegion>>,
    /// When it was replaced: a surface that stopped committing drops it.
    since: Instant,
}

#[derive(Debug)]
#[cfg(any(test, target_os = "linux"))]
struct ReadyCapture {
    rgba: Vec<u8>,
    /// The frame at full precision, when it carries an HDR signal.
    hdr: Option<crate::shell::hdr_surface::HdrPlane>,
    /// The frame, when it stayed on the GPU; `rgba` is then empty.
    gpu: Option<Arc<crate::shell::gpu_frames::GpuFrame>>,
    result: Result<(u32, u32, CaptureRoute), String>,
}

#[derive(Clone, Copy, Debug)]
enum CaptureRoute {
    Dmabuf(DmabufRoute),
}

/// One pending job per surface. A client may commit much faster than a GPU
/// readback completes; retaining every intermediate buffer turns animation
/// into unbounded input latency, while only the newest frame can ever become
/// visible.
#[derive(Debug)]
#[cfg(any(test, target_os = "linux"))]
struct LatestJobs<T> {
    jobs: HashMap<SurfaceId, T>,
}

#[cfg(any(test, target_os = "linux"))]
impl<T> Default for LatestJobs<T> {
    fn default() -> Self {
        Self {
            jobs: HashMap::new(),
        }
    }
}

#[cfg(any(test, target_os = "linux"))]
impl<T> LatestJobs<T> {
    fn push(&mut self, surface: SurfaceId, job: T) -> Option<T> {
        self.jobs.insert(surface, job)
    }

    fn pop(&mut self) -> Option<T> {
        let surface = *self.jobs.keys().next()?;
        self.jobs.remove(&surface)
    }

    fn remove(&mut self, surface: SurfaceId) -> Option<T> {
        self.jobs.remove(&surface)
    }
}

#[cfg(target_os = "linux")]
struct AsyncDmabufCapture {
    pending: Arc<Mutex<LatestJobs<DmabufJob>>>,
    /// Commits whose buffer the client's GPU is still rendering, the newest
    /// per surface. The compositor loop watches each buffer's implicit fence
    /// and moves the job to `pending` when it signals
    /// ([`dmabuf_fence_signalled`]), so no thread waits on a client's GPU and
    /// a slow client never holds up another surface's frames.
    awaiting_fence: Mutex<LatestJobs<DmabufJob>>,
    completed: Arc<Mutex<HashMap<ClientId, Client>>>,
    /// Frames handed back after a newer frame becomes current. A snapshot or
    /// the tile's image may still share one; the worker reuses it only once
    /// it holds the last reference, so readback and painting never alias.
    recycled: Arc<Mutex<HashMap<SurfaceId, Arc<Vec<u8>>>>>,
    wake: SyncSender<()>,
    completion_ping: Arc<Mutex<Option<smithay::reexports::calloop::ping::Ping>>>,
}

#[cfg(target_os = "linux")]
struct AsyncShmCapture {
    pending: Arc<Mutex<LatestJobs<ShmJob>>>,
    /// Copy allocations the worker is done with, for the compositor thread's
    /// next copy out of a client pool: a steady client then copies into
    /// memory that is already mapped.
    scratch: Arc<Mutex<Vec<Vec<u8>>>>,
    /// The frame each surface's last SHM commit built, which the worker lays
    /// that surface's next damaged commit over.
    surfaces: Arc<Mutex<HashMap<SurfaceId, ShmSurface>>>,
    wake: SyncSender<()>,
    completion_ping: Arc<Mutex<Option<smithay::reexports::calloop::ping::Ping>>>,
}

#[cfg(target_os = "linux")]
fn signal_capture_barrier(barrier: &Barrier) {
    barrier.signal();
}

#[cfg(target_os = "linux")]
fn mark_capture_complete(client: Client, completed: &Mutex<HashMap<ClientId, Client>>) {
    completed
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(client.id(), client);
}

#[cfg(target_os = "linux")]
fn release_superseded<T>(job: CaptureJob<T>, completed: &Mutex<HashMap<ClientId, Client>>) {
    signal_capture_barrier(&job.barrier);
    mark_capture_complete(job.client, completed);
}

/// Let a captured commit apply, and wake the compositor loop to apply it.
#[cfg(target_os = "linux")]
fn complete_capture(
    barrier: &Barrier,
    client: Client,
    completed: &Mutex<HashMap<ClientId, Client>>,
    ping: &Mutex<Option<smithay::reexports::calloop::ping::Ping>>,
) {
    signal_capture_barrier(barrier);
    mark_capture_complete(client, completed);
    if let Some(ping) = ping
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
    {
        ping.ping();
    }
}

#[cfg(target_os = "linux")]
impl AsyncDmabufCapture {
    fn spawn() -> Result<Self, String> {
        let pending: Arc<Mutex<LatestJobs<DmabufJob>>> =
            Arc::new(Mutex::new(LatestJobs::default()));
        let worker_pending = Arc::clone(&pending);
        let completed = Arc::new(Mutex::new(HashMap::new()));
        let worker_completed = Arc::clone(&completed);
        let recycled = Arc::new(Mutex::new(HashMap::<SurfaceId, Arc<Vec<u8>>>::new()));
        let worker_recycled = Arc::clone(&recycled);
        let completion_ping = Arc::new(Mutex::new(None::<smithay::reexports::calloop::ping::Ping>));
        let worker_completion_ping = Arc::clone(&completion_ping);
        // Capacity one is only a wake edge, never a job queue. The authoritative
        // queue is the latest-only map above, so `try_send` cannot block the
        // compositor even while the worker is busy.
        let (wake, wakes) = sync_channel::<()>(1);
        std::thread::Builder::new()
            .name("tilo-foreign-capture".into())
            .spawn(move || {
                let mut scratch = HashMap::<SurfaceId, Vec<u8>>::new();
                while wakes.recv().is_ok() {
                    loop {
                        let job = worker_pending
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .pop();
                        let Some(job) = job else { break };
                        let rgba = scratch.entry(job.surface).or_default();
                        let recycled = worker_recycled
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .remove(&job.surface);
                        // Still drawn somewhere: whoever holds it frees it.
                        if rgba.capacity() == 0
                            && let Some(mut previous) =
                                recycled.and_then(|pixels| Arc::try_unwrap(pixels).ok())
                        {
                            previous.clear();
                            *rgba = previous;
                        }
                        let surface = job.surface;
                        let (accepted, barrier, client) = capture_dmabuf_job(job, rgba);
                        if !accepted {
                            // The toplevel was destroyed while this import was
                            // in flight. Do not retain a full framebuffer in
                            // the worker's per-surface scratch map forever.
                            scratch.remove(&surface);
                        }
                        complete_capture(
                            &barrier,
                            client,
                            &worker_completed,
                            &worker_completion_ping,
                        );
                    }
                }
            })
            .map_err(|error| format!("could not start foreign-window capture worker: {error}"))?;
        Ok(Self {
            pending,
            awaiting_fence: Mutex::new(LatestJobs::default()),
            completed,
            recycled,
            wake,
            completion_ping,
        })
    }

    /// A job whose buffer is still being rendered: it runs once
    /// [`dmabuf_fence_signalled`] says the client's GPU is done with it.
    fn park(&self, job: DmabufJob) {
        let replaced = self
            .awaiting_fence
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(job.surface, job);
        if let Some(replaced) = replaced {
            // As in `enqueue`: only the newest commit can become visible, and
            // the older one never read its buffer.
            release_superseded(replaced, &self.completed);
        }
    }

    /// Run the parked job of `surface` for commit `token`, if it is still the
    /// newest. `readable` is `false` when its fence could not be watched.
    fn unpark(&self, surface: SurfaceId, token: u64, readable: bool) {
        let job = {
            let mut awaiting = self
                .awaiting_fence
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if awaiting
                .jobs
                .get(&surface)
                .is_some_and(|job| job.token == token)
            {
                awaiting.remove(surface)
            } else {
                None
            }
        };
        if let Some(mut job) = job {
            job.readable = readable;
            self.enqueue(job);
        }
    }

    fn enqueue(&self, job: DmabufJob) {
        // Tokens grow with every commit, so a job still waiting for its fence
        // belongs to an older commit of this surface.
        let parked = self
            .awaiting_fence
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(job.surface);
        if let Some(parked) = parked {
            release_superseded(parked, &self.completed);
        }
        let replaced = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(job.surface, job);
        if let Some(replaced) = replaced {
            // The newer commit is the only pending frame that can become
            // visible. Release the superseded transaction deliberately; a
            // barrier must never be abandoned in the queue.
            release_superseded(replaced, &self.completed);
        }
        // Full means a wake is already waiting; disconnected means the worker
        // failed after startup. Neither condition may stall Wayland dispatch.
        let _ = self.wake.try_send(());
    }

    fn drain_completed(&self) -> Vec<Client> {
        std::mem::take(
            &mut *self
                .completed
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
        .into_values()
        .collect()
    }

    fn recycle_pixels(&self, surface: SurfaceId, rgba: Arc<Vec<u8>>) {
        if rgba.capacity() == 0 {
            return;
        }
        self.recycled
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(surface, rgba);
    }

    fn forget_surface(&self, surface: SurfaceId) {
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(surface);
        if let Some(pending) = pending {
            release_superseded(pending, &self.completed);
        }
        let parked = self
            .awaiting_fence
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(surface);
        if let Some(parked) = parked {
            release_superseded(parked, &self.completed);
        }
        self.recycled
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&surface);
    }
}

/// Capture one readable dmabuf on the worker: share it on the GPU, or read it
/// back. Returns whether the surface still took the frame, and the commit's
/// barrier and client to let it apply: the shared route reads nothing here
/// (the frame is drawn from the buffer itself, which the compositor holds
/// from the commit on), the readback route is done reading.
#[cfg(target_os = "linux")]
fn capture_dmabuf_job(job: DmabufJob, rgba: &mut Vec<u8>) -> (bool, Barrier, Client) {
    if !job.readable {
        crate::shell::gpu_frames::wait_readable(&job.payload);
    }
    if let Some(frame) = share_dmabuf(&job.payload, job.surface) {
        let accepted = store().stage_gpu_capture(job.surface, job.token, frame);
        return (accepted, job.barrier, job.client);
    }
    let colorimetry = store().colorimetry(job.surface);
    let mut hdr = None;
    let result = decode_dmabuf(&job.payload, rgba, colorimetry, &mut hdr)
        .map(|(width, height, route)| (width, height, CaptureRoute::Dmabuf(route)));
    let accepted = store().stage_dmabuf_capture(job.surface, job.token, result, rgba, hdr);
    (accepted, job.barrier, job.client)
}

#[cfg(target_os = "linux")]
impl AsyncShmCapture {
    fn spawn() -> Result<Self, String> {
        let pending: Arc<Mutex<LatestJobs<ShmJob>>> = Arc::new(Mutex::new(LatestJobs::default()));
        let worker_pending = Arc::clone(&pending);
        let completion_ping = Arc::new(Mutex::new(None::<smithay::reexports::calloop::ping::Ping>));
        let worker_completion_ping = Arc::clone(&completion_ping);
        let scratch = Arc::new(Mutex::new(Vec::new()));
        let worker_scratch = Arc::clone(&scratch);
        let surfaces = Arc::new(Mutex::new(HashMap::new()));
        let worker_surfaces = Arc::clone(&surfaces);
        let (wake, wakes) = sync_channel::<()>(1);
        std::thread::Builder::new()
            .name("tilo-shm-capture".into())
            .spawn(move || {
                let worker = ShmWorker {
                    pending: &worker_pending,
                    surfaces: &worker_surfaces,
                    scratch: &worker_scratch,
                };
                loop {
                    // A spare frame is only worth keeping while its surface
                    // keeps committing; a worker holding none blocks without
                    // a timeout.
                    let woken = if worker.holds_spares() {
                        match wakes.recv_timeout(SHM_SPARE_IDLE) {
                            Ok(()) => true,
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => false,
                            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                        }
                    } else if wakes.recv().is_ok() {
                        true
                    } else {
                        break;
                    };
                    worker.release_idle_spares();
                    if !woken {
                        continue;
                    }
                    while let Some(job) = worker.next_job() {
                        worker.run(job);
                        SHM_FRAMES_PUBLISHED.store(true, std::sync::atomic::Ordering::Release);
                        if let Some(ping) = worker_completion_ping
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .as_ref()
                        {
                            ping.ping();
                        }
                    }
                    worker.release_unanchored();
                }
            })
            .map_err(|error| format!("could not start SHM capture worker: {error}"))?;
        Ok(Self {
            pending,
            scratch,
            surfaces,
            wake,
            completion_ping,
        })
    }

    fn enqueue(&self, mut job: ShmJob) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let moot = pending
            .remove(job.surface)
            .map_or_else(Vec::new, |older| job.absorb(older));
        pending.push(job.surface, job);
        drop(pending);
        recycle_shm_scratch(&self.scratch, moot);
        self.poke();
    }

    /// Wake the worker; it then also releases frames no commit can build on.
    fn poke(&self) {
        // Full means a wake is already waiting; disconnected means the worker
        // failed after startup. Neither may stall Wayland dispatch.
        let _ = self.wake.try_send(());
    }

    /// Bytes `surface` has queued and the worker has not taken yet.
    fn queued_bytes(&self, surface: SurfaceId) -> usize {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .jobs
            .get(&surface)
            .map_or(0, ShmJob::copied_bytes)
    }

    /// An allocation to copy the next commit into: the smallest returned one
    /// holding `needed` bytes, the largest when the size is unknown, or a
    /// fresh one when none fits.
    fn take_scratch(&self, needed: Option<usize>) -> Vec<u8> {
        let mut scratch = self
            .scratch
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let chosen = match needed {
            Some(needed) => scratch
                .iter()
                .enumerate()
                .filter(|(_, bytes)| bytes.capacity() >= needed)
                .min_by_key(|(_, bytes)| bytes.capacity())
                .map(|(index, _)| index),
            None => scratch
                .iter()
                .enumerate()
                .max_by_key(|(_, bytes)| bytes.capacity())
                .map(|(index, _)| index),
        };
        let taken = chosen.map(|index| scratch.swap_remove(index));
        drop(scratch);
        taken.unwrap_or_default()
    }

    fn forget_surface(&self, surface: SurfaceId) {
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(surface);
        let built = self
            .surfaces
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&surface);
        drop(built);
        if let Some(job) = pending {
            recycle_shm_scratch(&self.scratch, job.patches);
        }
    }
}

/// How long a worker with nothing to do keeps spare frames for reuse.
#[cfg(target_os = "linux")]
const SHM_SPARE_IDLE: Duration = Duration::from_secs(1);

/// The SHM capture worker's view of its shared state.
#[cfg(target_os = "linux")]
struct ShmWorker<'a> {
    pending: &'a Mutex<LatestJobs<ShmJob>>,
    surfaces: &'a Mutex<HashMap<SurfaceId, ShmSurface>>,
    scratch: &'a Mutex<Vec<Vec<u8>>>,
}

#[cfg(target_os = "linux")]
impl ShmWorker<'_> {
    fn next_job(&self) -> Option<ShmJob> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop()
    }

    fn surfaces(&self) -> std::sync::MutexGuard<'_, HashMap<SurfaceId, ShmSurface>> {
        self.surfaces
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Build one job's frame and publish it, or record why not.
    fn run(&self, mut job: ShmJob) {
        let previous = self.surfaces().remove(&job.surface);
        match build_shm_frame(&job, previous) {
            Ok(built) => {
                let size = (
                    u32::try_from(built.shape.width).unwrap_or(u32::MAX),
                    u32::try_from(built.shape.height).unwrap_or(u32::MAX),
                );
                let published = store().publish_shm_frame(&job, Arc::clone(&built.pixels), size);
                // A surface forgotten meanwhile stays forgotten.
                if published {
                    let replaced = self.surfaces().insert(job.surface, built);
                    drop(replaced);
                }
            }
            Err(ShmBuildError::Readback(error)) => {
                tracing::warn!(
                    target: "shell::compositor::foreign_capture",
                    surface_id = job.surface,
                    %error,
                    "foreign shm readback failed"
                );
                store().note_shm_failure(&job, error.to_string());
            }
            Err(ShmBuildError::Unanchored) => {
                // Its damage is relative to a frame this worker no longer
                // has; only a failed capture before it gets here. Laid over
                // another frame it would corrupt the window, so the surface
                // keeps what it shows and its next commit copies whole.
                tracing::debug!(
                    target: "shell::compositor::foreign_capture",
                    surface_id = job.surface,
                    "dropped an SHM commit whose damage has no base frame"
                );
                store().reset_shm_base(job.surface);
            }
        }
        store().note_shm_done(job.surface, job.generation);
        recycle_shm_scratch(self.scratch, std::mem::take(&mut job.patches));
    }

    fn holds_spares(&self) -> bool {
        self.surfaces()
            .values()
            .any(|surface| surface.spare.is_some())
    }

    /// Drop the spare frames of surfaces that stopped committing.
    fn release_idle_spares(&self) {
        let released: Vec<ShmSpare> = self
            .surfaces()
            .values_mut()
            .filter_map(|surface| {
                surface
                    .spare
                    .take_if(|spare| spare.since.elapsed() >= SHM_SPARE_IDLE)
            })
            .collect();
        drop(released);
    }

    /// Drop the frames of surfaces whose next commit cannot be laid over
    /// them: their last commit was not an SHM one this worker built (a
    /// dmabuf, an unmap, a forgotten surface), and nothing is queued.
    fn release_unanchored(&self) {
        let ids: Vec<SurfaceId> = self.surfaces().keys().copied().collect();
        // The base is read before the queue: a damaged commit is queued before
        // the base it was copied against is cleared, so one built on a frame
        // released here is still seen queued.
        let unanchored = store().without_shm_base(&ids);
        if unanchored.is_empty() {
            return;
        }
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let idle: Vec<SurfaceId> = unanchored
            .into_iter()
            .filter(|id| !pending.jobs.contains_key(id))
            .collect();
        drop(pending);
        let mut surfaces = self.surfaces();
        let released: Vec<_> = idle.iter().filter_map(|id| surfaces.remove(id)).collect();
        drop(surfaces);
        drop(released);
    }
}

/// Why a job's frame was not built.
#[cfg(target_os = "linux")]
enum ShmBuildError {
    /// The copied pixels could not be converted.
    Readback(ReadbackError),
    /// A damaged-only copy whose base frame is not the one it was copied
    /// against: missing, of another commit, size or format.
    Unanchored,
}

/// Build the frame one SHM job describes on `previous`, the surface's last
/// built frame. Damaged-only copies are laid only over exactly the commit
/// they were copied against; the frame `previous` replaced is written into
/// when nothing draws it any more, so a small commit touches only what
/// changed instead of copying the whole frame.
#[cfg(target_os = "linux")]
fn build_shm_frame(
    job: &ShmJob,
    previous: Option<ShmSurface>,
) -> Result<ShmSurface, ShmBuildError> {
    let first = job.patches.first().ok_or(ShmBuildError::Unanchored)?;
    let shape = first.frame.shape();
    let bytes = frame_bytes(shape);
    let (mut pixels, mut anchor, mut stale, replaced) = match (first.based_on, previous) {
        (None, previous) => {
            let (spare, replaced) = previous.map_or((None, None), |previous| {
                (
                    previous.spare.map(|spare| spare.pixels),
                    Some(previous.pixels),
                )
            });
            // Every pixel is about to be written; any content will do.
            let target = spare
                .and_then(|pixels| Arc::try_unwrap(pixels).ok())
                .filter(|pixels| pixels.len() == bytes)
                .unwrap_or_else(|| vec![0; bytes]);
            (target, None, None, replaced)
        }
        (Some(base), Some(previous))
            if previous.generation == base
                && previous.shape == shape
                && previous.pixels.len() == bytes =>
        {
            let target = damaged_target(&previous.pixels, previous.spare, shape.width);
            (target, Some(base), Some(Vec::new()), Some(previous.pixels))
        }
        (Some(_), _) => return Err(ShmBuildError::Unanchored),
    };
    for patch in &job.patches {
        if patch.frame.shape() != shape {
            return Err(ShmBuildError::Unanchored);
        }
        match (patch.based_on, patch.frame.regions()) {
            // Damage with nothing to lay it over.
            (None, Some(_)) => return Err(ShmBuildError::Unanchored),
            (Some(base), _) if anchor != Some(base) => return Err(ShmBuildError::Unanchored),
            (Some(_), Some(regions)) => {
                if let Some(stale) = stale.as_mut() {
                    stale.extend_from_slice(regions);
                }
            }
            (_, None) => stale = None,
        }
        apply_owned_shm(&patch.frame, &mut pixels).map_err(ShmBuildError::Readback)?;
        anchor = Some(patch.generation);
    }
    let stale = stale.filter(|regions| regions.len() <= 128);
    Ok(ShmSurface {
        pixels: Arc::new(pixels),
        shape,
        generation: anchor.unwrap_or(job.generation),
        spare: replaced.map(|pixels| ShmSpare {
            pixels,
            stale,
            since: Instant::now(),
        }),
    })
}

/// A copy of `current` to lay damage over: the spare frame brought up to
/// date when nothing else holds it, a full copy otherwise.
#[cfg(target_os = "linux")]
fn damaged_target(current: &Arc<Vec<u8>>, spare: Option<ShmSpare>, width: usize) -> Vec<u8> {
    let reusable = spare
        .and_then(|spare| Some((Arc::try_unwrap(spare.pixels).ok()?, spare.stale)))
        .filter(|(pixels, _)| pixels.len() == current.len());
    match reusable {
        Some((mut pixels, Some(stale))) => {
            copy_rgba_regions(current, &mut pixels, width, &stale);
            pixels
        }
        Some((mut pixels, None)) => {
            pixels.copy_from_slice(current);
            pixels
        }
        None => current.as_ref().clone(),
    }
}

/// Copy allocations kept for reuse. Two cover a client copying while the
/// worker converts; each can be a whole 4K frame, so no more are kept.
#[cfg(target_os = "linux")]
const SHM_SCRATCH_KEPT: usize = 2;

/// Keep the largest of `patches`' allocations for the next copies.
#[cfg(target_os = "linux")]
fn recycle_shm_scratch(scratch: &Mutex<Vec<Vec<u8>>>, patches: Vec<ShmPatch>) {
    if patches.is_empty() {
        return;
    }
    let mut dropped = Vec::new();
    let mut kept = scratch
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for patch in patches {
        let bytes = patch.frame.into_bytes();
        if bytes.capacity() == 0 {
            continue;
        }
        kept.push(bytes);
        if kept.len() > SHM_SCRATCH_KEPT
            && let Some(smallest) = kept
                .iter()
                .enumerate()
                .min_by_key(|(_, bytes)| bytes.capacity())
                .map(|(index, _)| index)
        {
            dropped.push(kept.swap_remove(smallest));
        }
    }
    drop(kept);
    drop(dropped);
}

/// Copy `regions` of one tightly packed RGBA image of `width` pixels a row
/// into another of the same size.
#[cfg(target_os = "linux")]
fn copy_rgba_regions(from: &[u8], to: &mut [u8], width: usize, regions: &[ShmRegion]) {
    let row_bytes = width.saturating_mul(4);
    for region in regions {
        let span = region.width.saturating_mul(4);
        for row in region.y..region.y.saturating_add(region.height) {
            let start = row
                .saturating_mul(row_bytes)
                .saturating_add(region.x.saturating_mul(4));
            let range = start..start.saturating_add(span);
            if let (Some(source), Some(target)) = (from.get(range.clone()), to.get_mut(range)) {
                target.copy_from_slice(source);
            }
        }
    }
}

#[cfg(target_os = "linux")]
static ASYNC_DMABUF_CAPTURE: OnceLock<Result<AsyncDmabufCapture, String>> = OnceLock::new();

#[cfg(target_os = "linux")]
static ASYNC_SHM_CAPTURE: OnceLock<Result<AsyncShmCapture, String>> = OnceLock::new();

/// Set by the SHM worker after it publishes (or fails) a frame. Unlike a
/// dmabuf, an SHM commit holds no transaction barrier, so nothing else tells
/// the loop a client frame landed.
#[cfg(target_os = "linux")]
static SHM_FRAMES_PUBLISHED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Whether the SHM worker published a frame since the last call.
#[cfg(target_os = "linux")]
pub fn take_published_shm_frames() -> bool {
    SHM_FRAMES_PUBLISHED.swap(false, std::sync::atomic::Ordering::AcqRel)
}

#[cfg(target_os = "linux")]
pub fn install_completion_ping(ping: &smithay::reexports::calloop::ping::Ping) {
    for slot in [
        ASYNC_DMABUF_CAPTURE
            .get_or_init(AsyncDmabufCapture::spawn)
            .as_ref()
            .ok()
            .map(|worker| &worker.completion_ping),
        ASYNC_SHM_CAPTURE
            .get_or_init(AsyncShmCapture::spawn)
            .as_ref()
            .ok()
            .map(|worker| &worker.completion_ping),
    ]
    .into_iter()
    .flatten()
    {
        *slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(ping.clone());
    }
}

/// Clients whose capture barriers changed to released since the last
/// compositor tick. Keeping the client handle here (instead of looking it up
/// through the surface later) also releases a fused transaction if that
/// surface was destroyed while its readback was in flight. The caller
/// re-evaluates Smithay's transaction queue on the main thread; the worker
/// never dispatches Wayland itself.
#[cfg(target_os = "linux")]
pub fn drain_completed_dmabuf_captures() -> Vec<Client> {
    ASYNC_DMABUF_CAPTURE
        .get()
        .and_then(|worker| worker.as_ref().ok())
        .map_or_else(Vec::new, AsyncDmabufCapture::drain_completed)
}

/// Client buffers the compositor holds because frames are drawn from them,
/// counted per buffer: a buffer re-attached while still held is held by each
/// of its frames and goes back once, after the last.
#[cfg(any(test, target_os = "linux"))]
#[derive(Debug)]
struct BufferHolds<B> {
    held: HashMap<B, usize>,
    /// Holds whose frame went, waiting to be counted off on the loop.
    returned: Vec<B>,
}

#[cfg(any(test, target_os = "linux"))]
impl<B: std::hash::Hash + Eq + Clone> BufferHolds<B> {
    fn new() -> Self {
        Self {
            held: HashMap::new(),
            returned: Vec::new(),
        }
    }

    fn hold(&mut self, buffer: B) {
        let count = self.held.entry(buffer).or_insert(0);
        *count = count.saturating_add(1);
    }

    /// Count off every returned hold; the buffers no frame holds any more.
    fn give_back(&mut self) -> Vec<B> {
        let mut free = Vec::new();
        for buffer in std::mem::take(&mut self.returned) {
            match self.held.get_mut(&buffer) {
                Some(count) if *count > 1 => *count = count.saturating_sub(1),
                Some(_) => {
                    self.held.remove(&buffer);
                    free.push(buffer);
                }
                // Never held, or already given back: not ours to release.
                None => {}
            }
        }
        free
    }
}

#[cfg(target_os = "linux")]
static BUFFER_HOLDS: std::sync::LazyLock<Mutex<BufferHolds<WlBuffer>>> =
    std::sync::LazyLock::new(|| Mutex::new(BufferHolds::new()));

/// Hold `buffer` for `frame`, which is drawn from it: the client gets it
/// back (`wl_buffer.release`) on the compositor loop once `frame` is gone and
/// the GPU finished reading it ([`crate::shell::gpu_frames::BufferLease`]).
#[cfg(target_os = "linux")]
pub fn hold_buffer(buffer: WlBuffer, frame: &crate::shell::gpu_frames::GpuFrame) {
    BUFFER_HOLDS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .hold(buffer.clone());
    frame.hold(crate::shell::gpu_frames::BufferLease::new(move || {
        BUFFER_HOLDS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .returned
            .push(buffer);
        if let Some(Ok(worker)) = ASYNC_DMABUF_CAPTURE.get()
            && let Some(ping) = worker
                .completion_ping
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
        {
            ping.ping();
        }
    }));
}

/// On the compositor loop: give every buffer no frame holds any more back to
/// its client.
#[cfg(target_os = "linux")]
pub fn give_back_released_buffers() {
    let free = BUFFER_HOLDS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .give_back();
    for buffer in free {
        // A buffer its client already destroyed takes no event.
        buffer.release();
    }
}

#[cfg(any(test, target_os = "linux"))]
fn recycle_dmabuf_pixels(surface: SurfaceId, rgba: Arc<Vec<u8>>) {
    #[cfg(not(target_os = "linux"))]
    let _ = (surface, rgba);
    #[cfg(target_os = "linux")]
    if let Some(Ok(worker)) = ASYNC_DMABUF_CAPTURE.get() {
        worker.recycle_pixels(surface, rgba);
    }
}

/// A dmabuf commit's capture, as [`ForeignFrameStore::prepare_dmabuf_capture`]
/// queued it.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub struct PreparedDmabufCapture {
    /// Names the commit's frame when the commit applies.
    pub token: u64,
    /// Attach to the same commit: it holds the commit, and so the buffer it
    /// replaces, until the capture is done.
    pub barrier: Barrier,
    /// The buffer's implicit fence, when the client's GPU was still writing
    /// the buffer. Insert it into the compositor loop with a callback that
    /// calls [`dmabuf_fence_signalled`]; the capture starts then. If it cannot
    /// be inserted, call [`dmabuf_fence_unwatched`]. `None` when the buffer
    /// was readable and the capture is already queued.
    pub fence: Option<smithay::backend::allocator::dmabuf::DmabufSource>,
}

/// The fence of commit `token`'s buffer on `surface` signalled: the client's
/// rendering into it is done, so its capture may read it. Nothing happens if
/// a newer commit superseded it or the surface is gone.
#[cfg(target_os = "linux")]
pub fn dmabuf_fence_signalled(surface: SurfaceId, token: u64) {
    if let Some(Ok(worker)) = ASYNC_DMABUF_CAPTURE.get() {
        worker.unpark(surface, token, true);
    }
}

/// The fence of commit `token`'s buffer could not be watched on the
/// compositor loop: capture it anyway, waiting for the fence on the worker.
#[cfg(target_os = "linux")]
pub fn dmabuf_fence_unwatched(surface: SurfaceId, token: u64) {
    if let Some(Ok(worker)) = ASYNC_DMABUF_CAPTURE.get() {
        worker.unpark(surface, token, false);
    }
}

/// Keep an SDR client frame on the GPU: import it on the presenting device
/// (once per buffer) and draw it from that import.
/// `None` sends it down the readback route instead (no presenting GPU yet, a
/// 10-bit, half-float or YUV buffer, or a layout the import cannot describe),
/// reported once per reason. An 8-bit buffer is shown as it is on either route.
#[cfg(target_os = "linux")]
fn share_dmabuf(
    dmabuf: &Dmabuf,
    surface: SurfaceId,
) -> Option<Arc<crate::shell::gpu_frames::GpuFrame>> {
    static REPORTED: Mutex<Vec<String>> = Mutex::new(Vec::new());
    match crate::shell::gpu_frames::import_dmabuf(dmabuf, surface) {
        Ok(frame) => Some(frame),
        Err(reason) => {
            let mut reported = REPORTED
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !reported.contains(&reason) && reported.len() < 32 {
                eprintln!(
                    "tilo compositor: client frame read back through the CPU, not shared on the GPU: {reason}"
                );
                reported.push(reason);
            }
            None
        }
    }
}

/// Read one dmabuf outside Wayland dispatch and outside the frame-store lock.
/// GPU import is required for device-local Vulkan buffers; CPU mmap remains
/// the fallback for linear system-memory buffers and machines without EGL.
///
/// A 10-bit or half-float buffer is read back at its own precision and
/// converted like the CPU import, so an HDR description yields its
/// [`crate::shell::hdr_surface::HdrPlane`] on either route. P010 cannot be
/// read back through GL, so it goes to the CPU first.
#[cfg(target_os = "linux")]
fn decode_dmabuf(
    dmabuf: &Dmabuf,
    rgba: &mut Vec<u8>,
    colorimetry: Option<crate::collab::colorimetry::Colorimetry>,
    hdr: &mut Option<crate::shell::hdr_surface::HdrPlane>,
) -> Result<(u32, u32, DmabufRoute), String> {
    *hdr = None;
    let layout = CpuLayout::for_fourcc(dmabuf.format().code);
    let description =
        colorimetry.unwrap_or_else(crate::collab::colorimetry::Colorimetry::sdr_bt709);
    if layout == Some(CpuLayout::P010)
        && let Ok((width, height)) = import_dmabuf_rgba(dmabuf, rgba, colorimetry, hdr)
    {
        return Ok((width, height, DmabufRoute::CpuMmap));
    }
    // What the GPU reads back, and how those bytes are laid out.
    let precise = match layout {
        Some(CpuLayout::Rgb10 { opaque, .. }) => Some((
            Fourcc::Abgr2101010,
            CpuLayout::Rgb10 {
                red_low: true,
                opaque,
            },
        )),
        Some(CpuLayout::Half { opaque }) => {
            Some((Fourcc::Abgr16161616f, CpuLayout::Half { opaque }))
        }
        _ => None,
    };
    let gpu_error = match crate::compositor::render_node::advertised() {
        Some(node) => {
            match crate::compositor::gpu_import::with_importer(node, |importer| match precise {
                Some((readback, _)) => {
                    let mut raw = Vec::new();
                    importer
                        .import_as(dmabuf, &mut raw, readback)
                        .map(|size| (size, Some(raw)))
                }
                None => importer.import(dmabuf, rgba).map(|size| (size, None)),
            }) {
                Ok(Ok(((width, height), raw))) => {
                    if let (Some(raw), Some((_, read_layout))) = (raw, precise) {
                        let row_bytes =
                            (width as usize).saturating_mul(read_layout.bytes_per_pixel() as usize);
                        *hdr = convert_rows(
                            read_layout,
                            raw.chunks_exact(row_bytes.max(1)),
                            (width, height),
                            description,
                            rgba,
                        );
                    }
                    return Ok((width, height, DmabufRoute::Gpu));
                }
                Ok(Err(error)) | Err(error) => Some(error.to_string()),
            }
        }
        None => Some("no dmabuf main device was advertised".to_string()),
    };

    match import_dmabuf_rgba(dmabuf, rgba, colorimetry, hdr) {
        Ok((width, height)) => Ok((width, height, DmabufRoute::CpuMmap)),
        Err(cpu_error) => Err(match gpu_error {
            Some(gpu_error) => format!("{cpu_error}; GPU import also failed: {gpu_error}"),
            None => cpu_error.to_string(),
        }),
    }
}

impl ForeignFrameStore {
    /// The image description the surface's client last attached.
    #[cfg(target_os = "linux")]
    fn colorimetry(&self, id: SurfaceId) -> Option<crate::collab::colorimetry::Colorimetry> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&id)
            .and_then(|entry| entry.colorimetry)
    }

    fn with_entry<R>(&self, id: SurfaceId, f: impl FnOnce(&mut ForeignEntry) -> R) -> R {
        let mut guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        f(guard.entry(id).or_insert_with(ForeignEntry::new))
    }

    #[cfg(any(test, target_os = "linux"))]
    fn note_import(&self) {
        self.imports.fetch_add(1, Ordering::Relaxed);
    }

    /// Buffer imports since process start, across every surface/route — read by the repaint-cost test (see the field's doc for why it must not track paints).
    /// The shm capture worker finished the job queued as `generation`.
    #[cfg(target_os = "linux")]
    fn note_shm_done(&self, id: SurfaceId, generation: u64) {
        let mut guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = guard.get_mut(&id) {
            entry.shm_done = entry.shm_done.max(generation);
        }
    }

    /// Whether a buffer some surface of the client `client_index` committed
    /// is still being captured, so its pixels are not yet what a screenshot
    /// shows. `client_index` is the high half of a [`SurfaceId`].
    pub fn capturing_for_client(&self, client_index: u32) -> bool {
        let guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard
            .iter()
            .any(|(id, entry)| id.wrapping_shr(32) == u64::from(client_index) && entry.capturing())
    }

    pub fn capture_seq_for_token(&self, id: SurfaceId, token: u64) -> Option<u64> {
        let entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = entries.get(&id)?;
        let result = (entry.activated_capture == Some(token) && entry.last_error.is_none())
            .then_some(entry.seq);
        drop(entries);
        result
    }

    /// Exact completed pixel generation; pending/superseded captures are not proof.
    pub fn presentation_pixels(&self, id: SurfaceId) -> Option<(u64, u64)> {
        let entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = entries.get(&id)?;
        let result =
            (!entry.capturing() && entry.last_error.is_none() && entry.mapped && entry.seq != 0)
                .then_some((entry.seq, entry.shm_done));
        drop(entries);
        result
    }

    pub fn begin_presentation_frame(&self, viewport: Rect) {
        *self
            .presentation_viewport
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(viewport);
        self.painted_frames
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }

    pub fn take_painted_frames(&self) -> HashMap<SurfaceId, u64> {
        std::mem::take(
            &mut *self
                .painted_frames
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    fn note_painted_frame(&self, id: SurfaceId, seq: u64, bounds: Rect) {
        let visible = self
            .presentation_viewport
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some_and(|viewport| bounds.intersect(viewport).area() > 0.0);
        if visible && !crate::shell::capture_protection::painting_for_capture() {
            self.painted_frames
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(id, seq);
        }
    }

    pub fn import_count(&self) -> u64 {
        self.imports.load(Ordering::Relaxed)
    }

    /// Capture the buffer a client just committed. Tries `wl_shm` readback
    /// first; a not-shm buffer routes to the dmabuf importer. The `wl_buffer → Dmabuf` lookup is done by the caller-visible [`smithay::wayland::dmabuf::get_dmabuf`], keeping this a single entry point.
    #[cfg(target_os = "linux")]
    pub fn capture_commit(&self, id: SurfaceId, buffer: &WlBuffer) -> CaptureOutcome {
        self.capture_commit_associated(id, buffer, None)
    }

    /// Capture one buffer commit with the token assigned by its pre-commit
    /// hook. Supplying the exact token prevents an unrelated damage-only
    /// commit from consuming a later blocked dmabuf result.
    #[cfg(target_os = "linux")]
    pub fn capture_commit_associated(
        &self,
        id: SurfaceId,
        buffer: &WlBuffer,
        capture_token: Option<u64>,
    ) -> CaptureOutcome {
        note_client_pump();
        if let Some(token) = capture_token {
            self.with_entry(id, |entry| entry.mapped = true);
            return self
                .activate_dmabuf_capture_token(id, token)
                .unwrap_or(CaptureOutcome::Queued);
        }
        // Converted outside the frame-store lock: painting takes it too.
        let mut pixels = Vec::new();
        let converted = buffer_to_rgba(buffer, &mut pixels);
        let (shm_result, replaced) = self.with_entry(id, |e| {
            // A non-null buffer commit maps the xdg surface even when its
            // pixels cannot yet be captured or do not fit its latest
            // configure. Input readiness must follow this protocol fact, not
            // the later visual-presentation decision.
            e.mapped = true;
            e.forget_shm_base();
            match converted {
                Ok((w, h)) => {
                    let replaced = std::mem::replace(&mut e.rgba, Arc::new(pixels));
                    e.hdr = None;
                    e.set_gpu(None);
                    e.width = w;
                    e.height = h;
                    e.seq = e.seq.wrapping_add(1);
                    e.last_error = None;
                    (Ok((w, h)), Some(replaced))
                }
                // Leave the previous frame intact — the dmabuf branch below may still succeed; a genuine failure clears it via `note_failure`.
                Err(err) => (Err(err), None),
            }
        });
        drop(replaced);
        match shm_result {
            Ok((w, h)) => {
                self.note_import();
                CaptureOutcome::Shm {
                    width: w,
                    height: h,
                }
            }
            Err(ReadbackError::DmaBufUnsupported) => {
                if smithay::wayland::dmabuf::get_dmabuf(buffer).is_ok() {
                    CaptureOutcome::Queued
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

    /// Queue the pending dmabuf from a surface pre-commit hook and return the
    /// barrier that must be attached to that same commit. This is the lifetime
    /// contract: Smithay cannot release/reuse the buffer until capture has
    /// completed or the job has been explicitly superseded.
    ///
    /// When the client's GPU is still rendering into the buffer the capture
    /// waits for its implicit fence, which the caller watches on the
    /// compositor loop: see [`PreparedDmabufCapture::fence`].
    #[cfg(target_os = "linux")]
    pub fn prepare_dmabuf_capture(
        &self,
        id: SurfaceId,
        client: Client,
        dmabuf: Dmabuf,
        colorimetry: Option<crate::collab::colorimetry::Colorimetry>,
    ) -> Option<PreparedDmabufCapture> {
        self.prepare_dmabuf_capture_with_damage(id, client, dmabuf, colorimetry, None)
    }

    #[cfg(target_os = "linux")]
    pub fn prepare_dmabuf_capture_with_damage(
        &self,
        id: SurfaceId,
        client: Client,
        dmabuf: Dmabuf,
        colorimetry: Option<crate::collab::colorimetry::Colorimetry>,
        damage: Option<Vec<Rect>>,
    ) -> Option<PreparedDmabufCapture> {
        let token = self
            .next_capture
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        self.with_entry(id, |entry| {
            entry.capture_tokens.push_back(token);
            entry.colorimetry = colorimetry;
            let generation = entry.damage_history.begin(damage);
            entry.capture_damage.insert(token, generation);
        });
        let worker = ASYNC_DMABUF_CAPTURE.get_or_init(AsyncDmabufCapture::spawn);
        match worker {
            Ok(worker) => {
                let barrier = Barrier::new(false);
                // Already readable unless the buffer still carries an
                // unsignalled write fence; asking does not wait.
                let fence = smithay::backend::allocator::dmabuf::DmabufSource::new(
                    dmabuf.clone(),
                    smithay::reexports::calloop::Interest::READ,
                )
                .ok();
                let job = DmabufJob {
                    surface: id,
                    token,
                    payload: dmabuf,
                    barrier: barrier.clone(),
                    client,
                    readable: true,
                };
                if fence.is_some() {
                    worker.park(job);
                } else {
                    worker.enqueue(job);
                }
                Some(PreparedDmabufCapture {
                    token,
                    barrier,
                    fence,
                })
            }
            Err(reason) => {
                // No worker owns this token, so it must not remain at the
                // head of the per-commit queue and steal a later result.
                self.with_entry(id, |entry| {
                    entry.capture_damage.remove(&token);
                    if let Some(position) = entry.capture_tokens.iter().position(|v| *v == token) {
                        entry.capture_tokens.remove(position);
                    }
                });
                self.note_failure(id, reason.clone());
                None
            }
        }
    }

    #[cfg(target_os = "linux")]
    pub fn prepare_shm_capture(&self, id: SurfaceId, frame: OwnedShmFrame) -> Option<u64> {
        self.prepare_shm_capture_with_damage(id, frame, None)
    }

    /// A validated SHM buffer commit maps the protocol surface immediately.
    /// Pixel conversion may finish later; it must not decide input eligibility.
    #[cfg(target_os = "linux")]
    pub fn note_shm_committed(&self, id: SurfaceId, generation: u64) -> bool {
        self.with_entry(id, |entry| {
            if generation == 0 || generation > entry.shm_queued {
                return false;
            }
            entry.mapped = true;
            true
        })
    }

    /// Queue an SHM copy made outside [`Self::capture_shm_commit`] for the
    /// capture worker, returning its generation. It is anchored exactly as
    /// the production route anchors its copies.
    #[cfg(target_os = "linux")]
    pub fn prepare_shm_capture_with_damage(
        &self,
        id: SurfaceId,
        frame: OwnedShmFrame,
        damage: Option<Vec<Rect>>,
    ) -> Option<u64> {
        match ASYNC_SHM_CAPTURE.get_or_init(AsyncShmCapture::spawn) {
            Ok(worker) => {
                let read = self.read_shm_base(id);
                Some(self.queue_shm_frame(worker, id, (frame, read), damage))
            }
            Err(reason) => {
                self.note_failure(id, reason.clone());
                None
            }
        }
    }

    /// Copy what an SHM commit changed out of `buffer`'s pool (on the
    /// compositor thread, inside Smithay's guarded mapping) and queue it for
    /// the capture worker. `damage` is the commit's buffer damage, `regions`
    /// the same in whole pixels. Only those pixels are copied when the
    /// surface's previous commit was an SHM buffer of the same size and
    /// format, and the copy is tagged with that commit for the worker to
    /// verify. Returns the capture generation, or `None` once the failure is
    /// recorded.
    #[cfg(target_os = "linux")]
    pub fn capture_shm_commit(
        &self,
        id: SurfaceId,
        buffer: &WlBuffer,
        damage: Option<Vec<Rect>>,
        regions: Option<&[ShmRegion]>,
    ) -> Option<u64> {
        let worker = match ASYNC_SHM_CAPTURE.get_or_init(AsyncShmCapture::spawn) {
            Ok(worker) => worker,
            Err(reason) => {
                self.note_failure(id, reason.clone());
                return None;
            }
        };
        let read = self.read_shm_base(id);
        // A surface whose queued copies already add up to a frame copies
        // whole, which supersedes them: what a slow worker holds stays bounded.
        let base = read
            .base
            .filter(|base| worker.queued_bytes(id) < frame_bytes(base.shape));
        let request = ShmCopy {
            damage: regions,
            base: base.map(|base| base.shape),
        };
        let scratch = worker.take_scratch(request.expected_bytes());
        match copy_shm_damage(buffer, &request, scratch) {
            Ok(frame) => Some(self.queue_shm_frame(worker, id, (frame, read), damage)),
            Err(error) => {
                self.note_failure(id, error.to_string());
                None
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn read_shm_base(&self, id: SurfaceId) -> ShmBaseRead {
        self.with_entry(id, |entry| ShmBaseRead {
            base: entry.shm_base,
            epoch: entry.shm_epoch,
        })
    }

    /// Queue `frame`, copied after `read`, as the surface's next SHM commit.
    /// A damaged-only copy is tagged with the commit it is relative to: the
    /// base it read, when that has the copy's size and format; otherwise
    /// nothing, and the worker drops it. The commit becomes the next base
    /// unless the base was cleared since `read`: a clear the copy did not see
    /// makes the next commit copy whole.
    #[cfg(target_os = "linux")]
    fn queue_shm_frame(
        &self,
        worker: &AsyncShmCapture,
        id: SurfaceId,
        (frame, read): (OwnedShmFrame, ShmBaseRead),
        damage: Option<Vec<Rect>>,
    ) -> u64 {
        let shape = frame.shape();
        let based_on = frame
            .regions()
            .and(read.base)
            .filter(|base| base.shape == shape)
            .map(|base| base.generation);
        let (generation, damage_generation) = self.with_entry(id, |entry| {
            entry.shm_queued = entry.shm_queued.wrapping_add(1);
            if entry.shm_epoch == read.epoch {
                entry.shm_base = Some(ShmBaseCommit {
                    shape,
                    generation: entry.shm_queued,
                });
            }
            (entry.shm_queued, entry.damage_history.begin(damage))
        });
        worker.enqueue(ShmJob {
            surface: id,
            generation,
            damage_generation,
            patches: vec![ShmPatch {
                frame,
                generation,
                based_on,
            }],
        });
        generation
    }

    /// The next SHM commit copies its whole buffer: the last commit was not
    /// one the SHM worker builds (a dmabuf, an unmap, a commit not captured),
    /// or its frame is gone.
    #[cfg(target_os = "linux")]
    pub fn reset_shm_base(&self, id: SurfaceId) {
        let mut guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(entry) = guard.get_mut(&id) {
            entry.forget_shm_base();
        }
        drop(guard);
    }

    /// Whether `id`'s next SHM commit may be laid over its last one.
    #[cfg(all(test, target_os = "linux"))]
    fn has_shm_base(&self, id: SurfaceId) -> bool {
        self.without_shm_base(&[id]).is_empty()
    }

    /// Those of `ids` whose next SHM commit cannot be laid over their last.
    #[cfg(target_os = "linux")]
    fn without_shm_base(&self, ids: &[SurfaceId]) -> Vec<SurfaceId> {
        let entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let unanchored = ids
            .iter()
            .copied()
            .filter(|id| entries.get(id).is_none_or(|entry| entry.shm_base.is_none()))
            .collect();
        drop(entries);
        unanchored
    }

    #[cfg(any(test, target_os = "linux"))]
    fn stage_dmabuf_capture(
        &self,
        id: SurfaceId,
        token: u64,
        result: Result<(u32, u32, CaptureRoute), String>,
        rgba: &mut Vec<u8>,
        hdr: Option<crate::shell::hdr_surface::HdrPlane>,
    ) -> bool {
        let mut guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(entry) = guard.get_mut(&id) else {
            return false;
        };
        if !entry.capture_tokens.contains(&token) {
            return false;
        }
        let mut captured = Vec::new();
        std::mem::swap(&mut captured, rgba);
        entry.ready_captures.insert(
            token,
            ReadyCapture {
                rgba: captured,
                hdr,
                gpu: None,
                result,
            },
        );
        drop(guard);
        true
    }

    /// Stage a frame that stayed on the GPU for the commit `token` belongs to.
    #[cfg(target_os = "linux")]
    fn stage_gpu_capture(
        &self,
        id: SurfaceId,
        token: u64,
        frame: Arc<crate::shell::gpu_frames::GpuFrame>,
    ) -> bool {
        let mut guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(entry) = guard.get_mut(&id) else {
            return false;
        };
        if !entry.capture_tokens.contains(&token) {
            return false;
        }
        let size = (frame.width(), frame.height());
        entry.ready_captures.insert(
            token,
            ReadyCapture {
                rgba: Vec::new(),
                hdr: None,
                gpu: Some(frame),
                result: Ok((size.0, size.1, CaptureRoute::Dmabuf(DmabufRoute::Shared))),
            },
        );
        drop(guard);
        true
    }

    #[cfg(any(test, target_os = "linux"))]
    fn activate_dmabuf_capture_token(
        &self,
        id: SurfaceId,
        expected_token: u64,
    ) -> Option<CaptureOutcome> {
        let mut guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = guard.get_mut(&id)?;
        let token = entry.capture_tokens.front().copied()?;
        if token != expected_token {
            return None;
        }
        entry.capture_tokens.pop_front();
        let damage_generation = entry.capture_damage.remove(&token);
        let ready = entry.ready_captures.remove(&token)?;
        let (outcome, imported, recycled, released) = match ready.result {
            Ok((width, height, route)) => {
                #[cfg(target_os = "linux")]
                entry.forget_shm_base();
                let previous = std::mem::replace(&mut entry.rgba, Arc::new(ready.rgba));
                entry.hdr = ready.hdr.map(Arc::new);
                let shared = matches!(route, CaptureRoute::Dmabuf(DmabufRoute::Shared));
                entry.set_gpu(ready.gpu);
                if shared {
                    entry.unheld.clone_from(&entry.gpu);
                }
                entry.width = width;
                entry.height = height;
                entry.seq = entry.seq.wrapping_add(1);
                let generation =
                    damage_generation.unwrap_or_else(|| entry.damage_history.begin(None));
                entry
                    .damage_history
                    .publish(entry.seq, generation, (width, height));
                entry.activated_capture = Some(token);
                entry.last_error = None;
                let outcome = match route {
                    CaptureRoute::Dmabuf(route) => CaptureOutcome::Dmabuf {
                        width,
                        height,
                        route,
                    },
                };
                (outcome, true, previous, None)
            }
            Err(reason) => {
                let previous = entry.take_pixels();
                entry.hdr = None;
                entry.set_gpu(None);
                entry.width = 0;
                entry.height = 0;
                entry.seq = entry.seq.wrapping_add(1);
                entry.last_error = Some(reason.clone());
                let outcome = CaptureOutcome::Failed(reason);
                (outcome, false, Arc::new(ready.rgba), Some(previous))
            }
        };
        drop(guard);
        // Freed, or handed back for reuse, outside the lock painting takes too.
        drop(released);
        recycle_dmabuf_pixels(id, recycled);
        if imported {
            self.note_import();
        }
        Some(outcome)
    }

    #[cfg(test)]
    fn activate_dmabuf_capture(&self, id: SurfaceId) -> Option<CaptureOutcome> {
        let token = self.with_entry(id, |entry| entry.capture_tokens.front().copied())?;
        self.activate_dmabuf_capture_token(id, token)
    }

    /// Activate a completed owned GPU copy through the ordinary capture-token path.
    #[cfg(test)]
    pub(crate) fn publish_gpu_for_test(
        &self,
        id: SurfaceId,
        frame: Arc<crate::shell::gpu_frames::GpuFrame>,
    ) -> Option<CaptureOutcome> {
        self.publish_damaged_gpu_for_test(id, frame, None)
    }

    #[cfg(test)]
    pub(crate) fn publish_damaged_gpu_for_test(
        &self,
        id: SurfaceId,
        frame: Arc<crate::shell::gpu_frames::GpuFrame>,
        damage: Option<Vec<Rect>>,
    ) -> Option<CaptureOutcome> {
        let token = self
            .next_capture
            .fetch_add(1, Ordering::Relaxed)
            .wrapping_add(1);
        self.with_entry(id, |entry| {
            entry.capture_tokens.push_back(token);
            if let Some(damage) = damage {
                let generation = entry.damage_history.begin(Some(damage));
                entry.capture_damage.insert(token, generation);
            }
        });
        if !self.stage_gpu_capture(id, token, frame) {
            return None;
        }
        self.activate_dmabuf_capture_token(id, token)
    }

    /// Publish an RGBA frame directly — used by callers that already hold decoded pixels (tests, future non-`wl_buffer` capture routes).
    pub fn publish_rgba(&self, id: SurfaceId, rgba: &[u8], width: u32, height: u32) {
        self.publish_frame(id, rgba, width, height, None);
    }

    /// Publish a frame with its full-precision HDR plane: `rgba` is the
    /// tone-mapped rendition of `hdr`, at the same size.
    pub fn publish_hdr_frame(
        &self,
        id: SurfaceId,
        rgba: &[u8],
        hdr: crate::shell::hdr_surface::HdrPlane,
    ) {
        let (width, height) = (hdr.width(), hdr.height());
        self.publish_frame(id, rgba, width, height, Some(Arc::new(hdr)));
    }

    fn publish_frame(
        &self,
        id: SurfaceId,
        rgba: &[u8],
        width: u32,
        height: u32,
        hdr: Option<Arc<crate::shell::hdr_surface::HdrPlane>>,
    ) {
        // Copied before the lock: painting takes it too.
        let pixels = Arc::new(rgba.to_vec());
        let replaced = self.with_entry(id, |e| {
            #[cfg(target_os = "linux")]
            e.forget_shm_base();
            e.mapped = true;
            e.hdr = hdr;
            e.set_gpu(None);
            let replaced = std::mem::replace(&mut e.rgba, pixels);
            e.width = width;
            e.height = height;
            e.seq = e.seq.wrapping_add(1);
            let generation = e.damage_history.begin(None);
            e.damage_history.publish(e.seq, generation, (width, height));
            e.last_error = None;
            replaced
        });
        drop(replaced);
    }

    /// Make a frame the SHM worker built for `job` current: the pixels
    /// change hands, they are not copied under the lock. Returns `false`,
    /// publishing nothing, when the job's surface went meanwhile.
    #[cfg(target_os = "linux")]
    fn publish_shm_frame(&self, job: &ShmJob, pixels: Arc<Vec<u8>>, size: (u32, u32)) -> bool {
        let mut guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(entry) = shm_job_entry(&mut guard, job) else {
            return false;
        };
        let replaced = entry.publish_frame(pixels, size, job.damage_generation);
        drop(guard);
        drop(replaced);
        true
    }

    /// Record that the SHM worker could not read `job`'s buffer, unless the
    /// job's surface went meanwhile.
    #[cfg(target_os = "linux")]
    fn note_shm_failure(&self, job: &ShmJob, reason: String) {
        let mut guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let released = shm_job_entry(&mut guard, job).map(|entry| entry.fail(reason));
        drop(guard);
        drop(released);
    }

    /// Publish a captured frame into `id`'s entry, as the SHM worker does,
    /// without a queued job. `false` when `id` is not tracked.
    #[cfg(test)]
    #[cfg(target_os = "linux")]
    fn publish_captured_rgba(
        &self,
        id: SurfaceId,
        pixels: Arc<Vec<u8>>,
        size: (u32, u32),
        generation: u64,
    ) -> bool {
        let mut guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(entry) = guard.get_mut(&id) else {
            return false;
        };
        let replaced = entry.publish_frame(pixels, size, generation);
        drop(guard);
        drop(replaced);
        true
    }

    /// Exercise the identical capture publication/history seam from pixel
    /// oracles. Production damage provenance is independently wire-tested.
    #[cfg(test)]
    #[cfg(target_os = "linux")]
    pub(crate) fn publish_damaged_rgba_for_test(
        &self,
        id: SurfaceId,
        rgba: &[u8],
        size: (u32, u32),
        damage: Vec<Rect>,
    ) {
        let generation = self.with_entry(id, |entry| entry.damage_history.begin(Some(damage)));
        self.publish_captured_for_test(id, rgba, size, generation);
    }

    /// Publish `rgba` as the SHM worker does for commit `generation`.
    #[cfg(test)]
    #[cfg(target_os = "linux")]
    fn publish_captured_for_test(
        &self,
        id: SurfaceId,
        rgba: &[u8],
        size: (u32, u32),
        generation: u64,
    ) {
        self.publish_captured_rgba(id, Arc::new(rgba.to_vec()), size, generation);
    }

    /// Buffer-pixel damage across every captured frame since a previous paint.
    /// Missing/coalesced metadata, extent changes and history overflow return
    /// None so callers retain the whole-image damage fallback.
    pub fn frame_damage_between(
        &self,
        id: SurfaceId,
        previous: u64,
        current: u64,
        size: (u32, u32),
    ) -> Option<Vec<Rect>> {
        self.with_entry(id, |entry| {
            entry.damage_history.between(previous, current, size)
        })
    }

    /// Record that the last capture failed and drop the stale frame.
    ///
    /// Two things read it: the window stops waiting (shown at once rather
    /// than held back for a frame that isn't coming), and `ForeignSurfaceWidget::unshowable_text` chooses the words shown, leaving `reason` here for debugging.
    pub fn note_failure(&self, id: SurfaceId, reason: String) {
        let released = self.with_entry(id, |e| e.fail(reason));
        drop(released);
    }

    /// The client detached its buffer (`attach(NULL)`): unmap it and drop the
    /// frame, while keeping placement bookkeeping until the toplevel is
    /// destroyed.
    pub fn clear_frame(&self, id: SurfaceId) {
        note_client_pump();
        let released = self.with_entry(id, |e| {
            e.mapped = false;
            let released = e.take_pixels();
            e.hdr = None;
            e.set_gpu(None);
            e.width = 0;
            e.height = 0;
            e.seq = e.seq.wrapping_add(1);
            e.last_error = None;
            released
        });
        drop(released);
    }

    /// Record that the app behind authenticated shell-ui `connection` does
    /// (or no longer does) want its windows withheld from captures. Covers
    /// windows it maps later too, so a new window is never briefly exposed.
    pub fn request_app_capture_protection(&self, connection: u64, app_id: &str, protected: bool) {
        let changed = {
            let mut guard = self
                .capture_protection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if protected {
                guard.apps.insert(connection, app_id.to_string()).as_deref() != Some(app_id)
            } else {
                guard.apps.remove(&connection).is_some()
            }
        };
        if changed {
            self.advance_capture_protection_epoch();
        }
    }

    /// Keep the last displayed protected buffer private when its control
    /// connection disappears. Toplevel destruction clears this surface latch.
    pub fn retain_surface_capture_protection(&self, id: SurfaceId, app_id: &str) {
        if self.is_capture_protected(id, app_id) {
            self.set_surface_capture_protected(id, true);
        }
    }

    /// The connection closed: its request goes with it.
    pub fn release_capture_protection(&self, connection: u64) {
        let released = self
            .capture_protection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .apps
            .remove(&connection)
            .is_some();
        if released {
            self.advance_capture_protection_epoch();
        }
    }

    /// Protect (or stop protecting) one surface regardless of its app.
    pub fn set_surface_capture_protected(&self, id: SurfaceId, protected: bool) {
        self.set_surfaces_capture_protected(&[(id, protected)]);
    }

    /// [`Self::set_surface_capture_protected`] for several surfaces under
    /// one acquisition of the lock.
    pub fn set_surfaces_capture_protected(&self, surfaces: &[(SurfaceId, bool)]) {
        let changed = {
            let mut guard = self
                .capture_protection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            surfaces.iter().fold(false, |changed, &(id, protected)| {
                let flipped = if protected {
                    guard.surfaces.insert(id)
                } else {
                    guard.surfaces.remove(&id)
                };
                changed | flipped
            })
        };
        if changed {
            self.advance_capture_protection_epoch();
        }
    }

    /// Changes to which apps and surfaces are protected so far. Equal values
    /// read at two moments mean nothing was protected or released in
    /// between. Takes no lock.
    pub fn capture_protection_epoch(&self) -> u64 {
        self.capture_protection_epoch.load(Ordering::Acquire)
    }

    /// Called after a change lands, so a reader that loaded the epoch
    /// before reading the protection state sees a new epoch next time.
    fn advance_capture_protection_epoch(&self) {
        self.capture_protection_epoch
            .fetch_add(1, Ordering::Release);
    }

    /// Whether a capture must draw surface `id`, owned by `app_id`, as a
    /// placeholder.
    pub fn is_capture_protected(&self, id: SurfaceId, app_id: &str) -> bool {
        let guard = self
            .capture_protection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.surfaces.contains(&id)
            || guard.apps.values().any(|app| app == app_id)
            || crate::shell::capture_protection::holds_secrets(app_id)
    }

    /// Drop everything tracked for `id` on toplevel destroy — without this the frame `Vec` (a full framebuffer) would outlive the client.
    pub fn forget(&self, id: SurfaceId) {
        self.set_surface_capture_protected(id, false);
        let mut guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let forgotten = guard.remove(&id);
        drop(guard);
        if let Some(mut entry) = forgotten {
            entry.set_gpu(None);
        }
        crate::shell::gpu_frames::forget_surface(id);
        let mut children = self
            .children
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        children.remove(&id);
        for tree in children.values_mut() {
            tree.retain(|child| child.surface != id);
        }
        drop(children);
        self.detach_sheet(id);
        #[cfg(target_os = "linux")]
        if let Some(Ok(worker)) = ASYNC_DMABUF_CAPTURE.get() {
            worker.forget_surface(id);
        }
        #[cfg(target_os = "linux")]
        if let Some(Ok(worker)) = ASYNC_SHM_CAPTURE.get() {
            worker.forget_surface(id);
        }
    }

    /// Current frame counter for `id`, its subsurfaces and the sheets on
    /// its tile; `0` when nothing has landed.
    pub fn frame_seq(&self, id: SurfaceId) -> u64 {
        self.surface_and_children_seq(id)
            .wrapping_add(self.sheets_seq(id))
    }

    fn surface_and_children_seq(&self, id: SurfaceId) -> u64 {
        let children = self
            .children
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&id)
            .cloned()
            .unwrap_or_default();
        let guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut seq = guard.get(&id).map_or(0, |e| e.seq);
        for child in children {
            seq = seq.wrapping_add(guard.get(&child.surface).map_or(0, |entry| entry.seq));
        }
        seq
    }

    /// Whether `id` has committed a non-null buffer and is therefore eligible
    /// for Wayland seat focus. Unlike [`Self::is_presented`], this does not
    /// wait for a captured frame to match the tab's configured size.
    pub fn is_mapped(&self, id: SurfaceId) -> bool {
        let guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.get(&id).is_some_and(|e| e.mapped)
    }

    /// The frame the commit just applied on `id` published, when it is drawn
    /// from the client's buffer and nothing holds that buffer yet. The caller
    /// takes hold of the buffer for it ([`crate::shell::gpu_frames::GpuFrame::hold`])
    /// before anything can draw it, or withdraws it
    /// ([`Self::withdraw_unheld_frame`]).
    pub fn take_unheld_frame(
        &self,
        id: SurfaceId,
    ) -> Option<Arc<crate::shell::gpu_frames::GpuFrame>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_mut(&id)?
            .unheld
            .take()
    }

    /// Stop showing `frame` on `id`: its buffer could not be held, so it must
    /// not be drawn from.
    pub fn withdraw_unheld_frame(
        &self,
        id: SurfaceId,
        frame: &Arc<crate::shell::gpu_frames::GpuFrame>,
    ) {
        self.with_entry(id, |entry| {
            if entry
                .gpu
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, frame))
            {
                entry.set_gpu(None);
                entry.width = 0;
                entry.height = 0;
                entry.seq = entry.seq.wrapping_add(1);
            }
        });
    }

    /// Copy out the current frame, or `None` when the surface has no pixels staged.
    pub fn snapshot(&self, id: SurfaceId) -> Option<FrameSnapshot> {
        let guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let e = guard.get(&id)?;
        let content = e.content();
        if e.width == 0 || e.height == 0 {
            return None;
        }
        let gpu = if e.rgba.is_empty() {
            Some(e.gpu.clone()?)
        } else {
            None
        };
        // Shared under the lock, copied after it.
        let cpu = Arc::clone(&e.rgba);
        let mut snapshot = FrameSnapshot {
            rgba: Vec::new(),
            width: e.width,
            height: e.height,
            seq: e.seq,
            hdr: e.hdr.clone(),
        };
        drop(guard);
        // A frame that stayed on the GPU is read back here, on demand.
        let pixels = match gpu {
            Some(gpu) => gpu.rgba()?,
            None => cpu,
        };
        snapshot.rgba.clone_from(pixels.as_ref());
        let children = self
            .children
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&id)
            .cloned()
            .unwrap_or_default();
        let guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let child_frames: Vec<_> = children
            .iter()
            .filter_map(|child| {
                let frame = guard.get(&child.surface)?;
                let pixels = if frame.rgba.is_empty() {
                    frame.gpu.clone().map(ChildSource::Gpu)
                } else {
                    Some(ChildSource::Cpu(Arc::clone(&frame.rgba)))
                };
                Some((
                    child.x,
                    child.y,
                    frame.width,
                    frame.height,
                    frame.seq,
                    pixels,
                ))
            })
            .collect();
        drop(guard);
        for (x, y, width, height, seq, pixels) in child_frames {
            let pixels = match pixels {
                Some(ChildSource::Cpu(rgba)) => Some(rgba),
                Some(ChildSource::Gpu(gpu)) => gpu.rgba(),
                None => None,
            };
            snapshot.seq = snapshot.seq.wrapping_add(seq);
            let Some(rgba) = pixels.filter(|rgba| !rgba.is_empty()) else {
                continue;
            };
            composite_child_rgba(
                &mut snapshot,
                &ChildPixels {
                    rgba: &rgba,
                    width,
                    height,
                },
                x,
                y,
            );
            // Subsurfaces live only in the SDR rendition; a window with any
            // is shown through it.
            snapshot.hdr = None;
        }
        // Subsurfaces are positioned against the whole buffer, so the window
        // is cut out only after they are composited onto it.
        if content != (0, 0, snapshot.width, snapshot.height) {
            snapshot.rgba = crop_rgba(&snapshot.rgba, snapshot.width, content);
            snapshot.hdr = snapshot
                .hdr
                .and_then(|plane| plane.crop(content))
                .map(Arc::new);
            snapshot.width = content.2;
            snapshot.height = content.3;
        }
        Some(snapshot)
    }

    /// The image a tile draws for `id`: straight from the GPU texture when the
    /// frame stayed there and nothing is composited onto it on the CPU,
    /// otherwise from [`Self::snapshot`]. Also the frame's HDR plane, if any.
    pub fn frame_image(
        &self,
        id: SurfaceId,
    ) -> Option<(ImageData, Option<Arc<crate::shell::hdr_surface::HdrPlane>>)> {
        let has_children = self
            .children
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&id)
            .is_some_and(|children| !children.is_empty());
        if !has_children {
            let shared = {
                let guard = self
                    .entries
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                guard.get(&id).and_then(|e| {
                    if e.rgba.is_empty() {
                        return Some(SharedFrame::Gpu(e.gpu.clone()?, e.content()));
                    }
                    // A CPU frame shown whole is drawn from the stored pixels
                    // themselves; a cropped one is cut out of a snapshot.
                    let whole = (0, 0, e.width, e.height);
                    (e.width != 0 && e.height != 0 && e.content() == whole).then(|| {
                        SharedFrame::Cpu(Arc::clone(&e.rgba), (e.width, e.height), e.hdr.clone())
                    })
                })
            };
            match shared {
                Some(SharedFrame::Gpu(gpu, content)) => {
                    if content.2 == 0 || content.3 == 0 {
                        return None;
                    }
                    return Some((gpu.image(content), None));
                }
                Some(SharedFrame::Cpu(pixels, (width, height), hdr)) => {
                    return Some((rgba_image(pixels, width, height), hdr));
                }
                None => {}
            }
        }
        let snapshot = self.snapshot(id)?;
        let hdr = snapshot.hdr.clone();
        Some((frame_image(snapshot), hdr))
    }

    /// `rgba`, a `width`×`height` buffer `id` committed, cut to the window
    /// its client declared (no shadow or resize border), as tiles show it.
    /// Unchanged without a declared geometry.
    pub fn crop_to_window(&self, id: SurfaceId, rgba: Vec<u8>, width: u32, height: u32) -> Vec<u8> {
        let crop = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&id)
            .and_then(|e| e.geometry)
            .and_then(|geometry| geometry.crop(width, height));
        match crop {
            Some(content) => crop_rgba(&rgba, width, content),
            None => rgba,
        }
    }

    /// Record the client's declared window geometry (`None` clears it), read
    /// from the state each commit applied.
    pub fn note_window_geometry(&self, id: SurfaceId, geometry: Option<WindowGeometry>) {
        self.with_entry(id, |e| e.geometry = geometry);
    }

    /// Record the client's minimum and maximum size (logical pixels, `0` for
    /// unconstrained); the next [`Self::take_pending_configure`] honours them.
    pub fn note_size_limits(&self, id: SurfaceId, min: (i32, i32), max: (i32, i32)) {
        self.with_entry(id, |e| {
            e.min_size = min;
            e.max_size = max;
        });
    }

    /// The box inside `tile` that `id`'s window is drawn into: the whole tile,
    /// or a centred box when the client's maximum size keeps it smaller.
    pub fn target_box(&self, id: SurfaceId, tile: Rect) -> Rect {
        let guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.get(&id).map_or(tile, |e| e.target_box(tile))
    }

    /// The point surface-local `(0, 0)` lands on when `id` is shown in
    /// `tile` — the origin every pointer and touch coordinate for it is
    /// measured from. Drawing puts the window's geometry at the corner of
    /// [`Self::target_box`], so the surface origin is that corner less the
    /// geometry's offset into the buffer (its shadow, say).
    pub fn surface_origin(&self, id: SurfaceId, tile: Rect) -> (f64, f64) {
        let guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let placed = guard.get(&id).map(|e| (e.target_box(tile), e.content()));
        drop(guard);
        let Some((target, (x, y, _, _))) = placed else {
            return (tile.x0, tile.y0);
        };
        (target.x0 - f64::from(x), target.y0 - f64::from(y))
    }

    pub fn set_children(&self, root: SurfaceId, children: Vec<ChildFrame>) {
        self.children
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(root, children);
    }

    pub fn child_at(&self, root: SurfaceId, x: f64, y: f64) -> Option<(SurfaceId, f64, f64)> {
        let children = self
            .children
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&root)
            .cloned()?;
        let entries = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        children.iter().rev().find_map(|child| {
            let frame = entries.get(&child.surface)?;
            let local_x = x - f64::from(child.x);
            let local_y = y - f64::from(child.y);
            (local_x >= 0.0
                && local_y >= 0.0
                && local_x < f64::from(frame.width)
                && local_y < f64::from(frame.height)
                && frame.mapped)
                .then_some((child.surface, local_x, local_y))
        })
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
        let (_, _, w, h) = e.content();
        let size = (w > 0 && h > 0).then_some((w, h));
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
    /// way: nothing presented, waited longer than `OPENING_DELAY`. Read
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
    /// `ForeignEntry::first_seen` dates from being asked for, not its first buffer.
    ///
    /// Both waits this store bounds — the opening configure in
    /// [`flush_configures`] and the presentation grace in
    /// [`Self::mark_presented`] — run from that instant.
    pub fn note_adopted(&self, id: SurfaceId) {
        self.with_entry(id, |entry| entry.adopted = true);
    }

    /// A real toplevel remains available to answer a close decision. The
    /// compositor records adoption even before the first buffer and forgets
    /// the entry on destruction; layout-only restored tabs remain placeholders.
    pub fn is_adopted(&self, id: SurfaceId) -> bool {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&id)
            .is_some_and(|entry| entry.adopted)
    }

    /// Whether this surface has waited longer than
    /// `OPENING_CONFIGURE_GRACE` since adopted. Read only by
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

    /// Impose `rect` over the tab's own placement until cleared with `None`:
    /// the client is configured to it and input and popups resolve against
    /// it, however the strip lays the tab out meanwhile.
    pub fn pin_placement(&self, id: SurfaceId, rect: Option<Rect>) {
        self.with_entry(id, |e| {
            e.pinned = rect;
        });
    }

    /// Whether [`Self::pin_placement`] currently holds this surface.
    pub fn is_pinned(&self, id: SurfaceId) -> bool {
        let guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.get(&id).is_some_and(|e| e.pinned.is_some())
    }

    /// Where the surface is: a pinned rectangle, else the tab rectangle last
    /// published by [`Self::note_placement`].
    pub fn placement(&self, id: SurfaceId) -> Option<Rect> {
        let guard = self
            .entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.get(&id).and_then(|e| e.pinned.or(e.placement))
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
    ///
    /// The tab is laid out in the logical desktop, so its size is the
    /// configure as is; the display's scale reaches the client separately,
    /// as its preferred `wp_fractional_scale`.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "each value is .round()-ed and .clamp()-ed into i32's range first"
    )]
    pub fn take_pending_configure(&self, id: SurfaceId) -> Option<(i32, i32)> {
        self.with_entry(id, |e| {
            let rect = e.pinned.or(e.placement)?;
            let logical = (
                rect.width().round().clamp(0.0, f64::from(i32::MAX)) as i32,
                rect.height().round().clamp(0.0, f64::from(i32::MAX)) as i32,
            );
            if logical.0 == 0 || logical.1 == 0 {
                return None;
            }
            let logical = (
                honour_size_limits(logical.0, e.min_size.0, e.max_size.0),
                honour_size_limits(logical.1, e.min_size.1, e.max_size.1),
            );
            e.configured_extent = Some((f64::from(logical.0), f64::from(logical.1)));
            if e.configured == Some(logical) {
                return None;
            }
            e.configured = Some(logical);
            Some(logical)
        })
    }

    /// Replace the sizes the person's other devices show streamed windows at.
    pub fn set_remote_extents(&self, extents: HashMap<SurfaceId, (i32, i32)>) {
        *self
            .remote_extents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = extents;
    }

    /// The size another device shows `id` at, if it is streamed to one.
    pub fn remote_extent(&self, id: SurfaceId) -> Option<(i32, i32)> {
        self.remote_extents
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&id)
            .copied()
    }

    /// [`Self::take_pending_configure`] for a window only other devices
    /// show: the size they show it at, within the client's limits, iff it
    /// differs from the last sent.
    pub fn take_remote_configure(&self, id: SurfaceId, extent: (i32, i32)) -> Option<(i32, i32)> {
        self.with_entry(id, |e| {
            if extent.0 <= 0 || extent.1 <= 0 {
                return None;
            }
            let logical = (
                honour_size_limits(extent.0, e.min_size.0, e.max_size.0),
                honour_size_limits(extent.1, e.min_size.1, e.max_size.1),
            );
            e.configured_extent = Some((f64::from(logical.0), f64::from(logical.1)));
            if e.configured == Some(logical) {
                return None;
            }
            e.configured = Some(logical);
            Some(logical)
        })
    }

    /// Forget the size last sent, so the next [`Self::take_pending_configure`]
    /// sends the tab size again even though the layout did not change: the
    /// client was configured to something else in between.
    pub fn resend_configure(&self, id: SurfaceId) {
        self.with_entry(id, |e| e.configured = None);
    }

    /// Whether this surface has something worth putting on screen, and
    /// latch the answer once it does. Read by
    /// [`ForeignSurfaceWidget::presented`], the predicate deciding when a launched window turns up.
    ///
    /// Presentable when: a **capture failed** (must say so, not be
    /// invisible); a **frame answers the tab** per `fills` (else the
    /// unasked-for buffer size scales via `fit_rect`, snapping to 1:1
    /// later); or **`PRESENT_GRACE` passes** (backstop against an unclosable window). `tile` degenerate means not laid out, unjudgeable.
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
            let (_, _, content_w, content_h) = e.content();
            if e.last_error.is_none()
                && !fills(content_w, content_h, e.target_box(tile))
                && !backstop
            {
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

/// One axis of a configure size, kept within the client's `min`..`max`
/// (either `0` when that side is unconstrained). A maximum below the tab
/// wins — the window is then shown centred — and a minimum above it wins
/// too, the oversized frame being scaled down into the tab.
fn honour_size_limits(tab: i32, min: i32, max: i32) -> i32 {
    let capped = if max > 0 { tab.min(max) } else { tab };
    if min > 0 { capped.max(min) } else { capped }
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
    /// The cached frame at full precision, when it carries an HDR signal.
    cached_hdr: Option<Arc<crate::shell::hdr_surface::HdrPlane>>,
    cached_seq: u64,
    /// Drawn transient sheets on this tile, bottom to top, refreshed with `cached`.
    sheet_images: Vec<(SheetLink, ImageData)>,
    /// Layout moved or resized the client since the last composed frame.
    /// Even when the client has not committed new pixels yet, the retained
    /// scene must be rebuilt so its cached image is drawn into the new rect.
    placement_dirty: bool,
    /// Stable, human-addressable name so `FrameBuilder::widget_registry` (and the shell's click/rect resolver) can find the tile.
    test_id: String,
    /// The app's own name, resolved on the frame that first needs it.
    ///
    /// Only the placeholder below reads it — the rare path where an app
    /// never showed a usable frame. The lookup consults launches then installed manifests, so it's not repeated on every rebuild.
    name: std::cell::OnceCell<String>,
    preserve_transparency: bool,
    last_draw: Option<std::time::Instant>,
}

impl ForeignSurfaceWidget {
    pub fn new(surface_id: SurfaceId, app_id: String) -> Self {
        Self {
            surface_id,
            app_id,
            rect: Rect::ZERO,
            cached: None,
            cached_hdr: None,
            cached_seq: 0,
            sheet_images: Vec::new(),
            placement_dirty: true,
            test_id: format!("foreign-surface-{surface_id}"),
            name: std::cell::OnceCell::new(),
            preserve_transparency: false,
            last_draw: None,
        }
    }

    pub fn new_floating(surface_id: SurfaceId, app_id: String) -> Self {
        Self {
            preserve_transparency: true,
            ..Self::new(surface_id, app_id)
        }
    }

    /// The app's name, as the tab above this surface says it.
    fn display_name(&self) -> &str {
        self.name
            .get_or_init(|| crate::shell::tab_binding::tab_label_for_foreign(&self.app_id))
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
            Some(_) => crate::shell::i18n::t("foreign-window-unshowable"),
            None => crate::shell::i18n::t("foreign-window-not-responding"),
        };
        (self.display_name().to_string(), detail)
    }

    fn draw_collaboration_overlay(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        presence.draw_interactions_for_app(
            scene,
            transform,
            self.rect,
            text_renderer,
            theme,
            Instant::now(),
            &self.app_id,
        );
    }

    pub fn surface_id(&self) -> SurfaceId {
        self.surface_id
    }

    /// Where this surface is drawn, when a capture must replace it with the
    /// placeholder; `None` when it is not protected or not laid out.
    pub fn capture_protected_rect(&self) -> Option<Rect> {
        (self.rect.width() > 0.0
            && self.rect.height() > 0.0
            && store().is_capture_protected(self.surface_id, &self.app_id))
        .then_some(self.rect)
    }

    /// Place a compositor-owned floating surface without putting it through
    /// the workspace's Taffy tree. The same placement store still drives the
    /// xdg configure handshake, so the client renders at this exact size.
    pub fn place_floating(&mut self, rect: Rect) {
        self.rect = rect;
        // A pinned surface's size is the pin's business; recording the
        // overlay rect as its tab rect would outlive the pin.
        if rect.width() > 0.0 && rect.height() > 0.0 && !store().is_pinned(self.surface_id) {
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

    /// Why this tile is asking for frames, for the paint-stats diagnostic.
    pub(crate) fn animation_detail(&self) -> String {
        format!(
            "seq={} cached_seq={} cached={} presented={} drawn_ms_ago={:?}",
            store().frame_seq(self.surface_id),
            self.cached_seq,
            self.cached.is_some(),
            store().is_presented(self.surface_id),
            self.last_draw.map(|at| at.elapsed().as_millis()),
        )
    }

    /// Pull a fresh frame out of the store if one has landed since the last call; returns `true` when the cache changed.
    fn refresh_cache(&mut self) -> bool {
        let seq = store().frame_seq(self.surface_id);
        // A cache can be empty at a non-zero sequence when an earlier paint
        // observed the sequence edge before its snapshot became available.
        // Retry that state instead of permanently pinning the fallback card;
        // steady clients with a real cached image retain the zero-copy fast
        // path below.
        if seq == self.cached_seq && (seq == 0 || self.cached.is_some()) {
            return false;
        }
        self.cached_seq = seq;
        let frame = store().frame_image(self.surface_id);
        if store().frame_seq(self.surface_id) != seq {
            self.cached_seq = 0;
            return false;
        }
        self.cached_hdr = frame.as_ref().and_then(|(_, hdr)| hdr.clone());
        self.cached = frame.map(|(image, _)| image);
        self.sheet_images = store()
            .sheets_over(self.surface_id)
            .into_iter()
            .filter_map(|link| Some((link, store().frame_image(link.surface)?.0)))
            .collect();
        true
    }

    /// Paint the tile's sheets over its image: each centred where
    /// [`ForeignFrameStore::sheet_rect_in_root`] puts it, a modal one
    /// behind the OS scrim so the window it blocks reads as inactive.
    fn draw_sheets(
        &self,
        scene: &mut Scene,
        transform: Affine,
        dst: Rect,
        image: &ImageData,
        theme: &Theme,
    ) {
        let sx = dst.width() / f64::from(image.width.max(1));
        let sy = dst.height() / f64::from(image.height.max(1));
        for (link, sheet) in &self.sheet_images {
            if link.modal {
                ui::overlay::scrim::modal(scene, transform, self.rect, 1.0, theme);
            }
            let Some(local) = store().sheet_rect_in_root(self.surface_id, link.surface) else {
                continue;
            };
            let at = translated_and_scaled(
                transform,
                dst.x0 + local.x0 * sx,
                dst.y0 + local.y0 * sy,
                sx,
                sy,
            );
            scene.fill(
                Fill::NonZero,
                at,
                &ImageBrush::new(sheet.clone()),
                None,
                &Rect::new(0.0, 0.0, f64::from(sheet.width), f64::from(sheet.height)),
            );
        }
    }

    fn record_foreign_damage(
        &self,
        image: &ImageData,
        brush_transform: Affine,
        transform: Affine,
        dst: Rect,
    ) {
        super::foreign_damage::record(
            self.surface_id,
            image,
            self.cached_seq,
            brush_transform,
            transform.transform_rect_bbox(self.rect.union(dst)),
        );
    }

    /// Register the frame's full-precision plane, when it has one, for the
    /// image about to be drawn at `brush_transform` into `dst`.
    fn record_hdr_layer(
        &self,
        image: &ImageData,
        brush_transform: Affine,
        transform: Affine,
        dst: Rect,
    ) {
        let Some(plane) = &self.cached_hdr else {
            return;
        };
        let visible = if self.preserve_transparency {
            dst
        } else {
            dst.intersect(self.rect)
        };
        crate::shell::hdr_surface::record(
            plane,
            image,
            brush_transform,
            transform.transform_rect_bbox(visible),
        );
    }

    /// Whether this surface's window is still being held back, and so the
    /// shell has to keep asking.
    ///
    /// The frame counter alone can't carry this: an uncommitted client
    /// leaves it at zero, matching the cache, so the strip is judged
    /// unchanged; the ten-second backstop is only read from a draw — without this the window sits invisible until an unrelated repaint.
    fn awaiting_first_paint(&self) -> bool {
        // The native paint gate asks `needs_animation_frame` before it calls
        // `draw`.  Merely reading the old latch here creates a permanent
        // redraw loop: the first fitting frame is already visible, but only
        // `presented()` (normally reached from draw) used to settle it.  Judge
        // the same live rect here so the first client frame produces one
        // final paint and then lets a static desktop sleep.
        !store().mark_presented(self.surface_id, self.rect)
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
        // A window whose maximum size keeps it smaller than the tab is fitted
        // into its centred box instead; `surface_origin` shifts input to match.
        let bounds = store().target_box(self.surface_id, self.rect);
        let iw = f64::from(img_w);
        let ih = f64::from(img_h);
        let scale = (bounds.width() / iw).min(bounds.height() / ih);
        let dw = iw * scale;
        let dh = ih * scale;
        let dx = bounds.x0;
        let dy = bounds.y0;
        Some(Rect::new(dx, dy, dx + dw, dy + dh))
    }
}

/// A captured frame as a GPU-uploadable image.
fn frame_image(frame: FrameSnapshot) -> ImageData {
    rgba_image(Arc::new(frame.rgba), frame.width, frame.height)
}

/// Tightly packed RGBA `pixels` as a GPU-uploadable image, sharing them.
fn rgba_image(pixels: Arc<Vec<u8>>, width: u32, height: u32) -> ImageData {
    ImageData {
        data: Blob::new(pixels),
        format: ImageFormat::Rgba8,
        // Wayland `ARGB8888` carries premultiplied alpha; `X` variants are forced opaque during the swizzle, where premultiplied and straight coincide.
        alpha_type: ImageAlphaType::AlphaPremultiplied,
        width,
        height,
    }
}

/// The current frame of a surface with nothing composited onto it, shared
/// with the frame store rather than copied.
enum SharedFrame {
    Gpu(
        Arc<crate::shell::gpu_frames::GpuFrame>,
        (u32, u32, u32, u32),
    ),
    Cpu(
        Arc<Vec<u8>>,
        (u32, u32),
        Option<Arc<crate::shell::hdr_surface::HdrPlane>>,
    ),
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

/// Paint an output-owned presentation surface directly into its physical
/// output rectangle.  Unlike [`ForeignSurfaceWidget`], this deliberately
/// bypasses the workspace tree: a clean audience output must not consume or
/// redraw the presenter's local group.
pub fn paint_output_presentation_surface(
    scene: &mut Scene,
    surface_id: SurfaceId,
    rect: Rect,
    theme: &Theme,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    // An audience surface is deliberately opaque: its renderer may carry
    // transparent margins while loading, but projector pixels must never
    // reveal the presenter's desktop behind the slide.
    scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        theme.tokens.colors.surfaces.content,
        None,
        &rect,
    );
    let Some((image, _)) = store().frame_image(surface_id) else {
        return;
    };
    let (width, height) = (image.width, image.height);
    if width == 0 || height == 0 {
        return;
    }
    let transform = translated_and_scaled(
        Affine::IDENTITY,
        rect.x0,
        rect.y0,
        rect.width() / f64::from(image.width),
        rect.height() / f64::from(image.height),
    );
    let brush = ImageBrush::new(image).with_extend(Extend::Pad);
    scene.fill(
        Fill::NonZero,
        transform,
        &brush,
        None,
        &Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
    );
}

impl Widget for ForeignSurfaceWidget {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // `new_leaf` can't fail in practice for a single-node insert; a
        // dummy id here just means the fallback is unreachable by real
        // traversal instead of panicking.
        let style = taffy::Style {
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::percent(1.0),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(l) = taffy.layout(node) else {
            return;
        };
        let x = origin.x + f64::from(l.location.x);
        let y = origin.y + f64::from(l.location.y);
        let next_rect = Rect::new(
            x,
            y,
            x + f64::from(l.size.width),
            y + f64::from(l.size.height),
        );
        if self.rect != next_rect {
            self.rect = next_rect;
            self.placement_dirty = true;
        }
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
        presence: &PresenceCtx,
    ) {
        self.last_draw = Some(std::time::Instant::now());
        self.refresh_cache();
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return;
        }
        let radius = f64::from(theme.tokens.radii.tile);
        if crate::shell::capture_protection::painting_for_capture()
            && self.capture_protected_rect().is_some()
        {
            crate::shell::capture_protection::paint_placeholder(
                scene, transform, self.rect, radius, theme,
            );
            return;
        }
        let clip = RoundedRect::from_rect(self.rect, radius);

        if let Some(image) = self.cached.clone()
            && let Some(dst) = self.fit_rect(image.width, image.height)
        {
            store().note_painted_frame(
                self.surface_id,
                self.cached_seq,
                transform.transform_rect_bbox(self.rect),
            );
            let sx = dst.width() / f64::from(image.width);
            let sy = dst.height() / f64::from(image.height);
            let brush_transform = translated_and_scaled(transform, dst.x0, dst.y0, sx, sy);
            self.record_foreign_damage(&image, brush_transform, transform, dst);
            let brush = ImageBrush::new(image.clone()).with_extend(Extend::Pad);
            self.record_hdr_layer(&image, brush_transform, transform, dst);
            if self.preserve_transparency {
                // The app buffer already carries the shared floating chrome's
                // soft silhouette. Drawing it directly preserves transparent
                // corners and prevents a second radius/backdrop from making
                // the native handoff look wider than the in-app surface.
                scene.fill(
                    Fill::NonZero,
                    brush_transform,
                    &brush,
                    None,
                    &Rect::new(0.0, 0.0, f64::from(image.width), f64::from(image.height)),
                );
                self.draw_collaboration_overlay(scene, transform, text_renderer, theme, presence);
                return;
            }
            // Neutral backdrop so letterbox bands (and client transparency) read as compositor surface, not whatever the tile card painted.
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.base,
                None,
                &clip,
            );
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
            self.draw_sheets(scene, transform, dst, &image, theme);
            scene.pop_layer();
            self.draw_collaboration_overlay(scene, transform, text_renderer, theme, presence);
            return;
        }

        let (headline, detail) = self.unshowable_text();
        // Fixed ramp indices were a guess about which way round the card
        // is, not an answer: `neutral[1]` is near-black on the dark ramp
        // and the card is `#16171b`, so the app's own name measured 1.03:1 and was invisible. `on_card` derives both tiers from the card's actual luminance instead.
        let tiers = theme.tokens.colors.on_content();
        let on_card = tiers.ink;
        let on_card_dim = tiers.ink_dim;
        let style = theme.text(TextRole::Title);
        let (tw, _) = text_renderer.measure_style(&headline, None, &style);
        let cx = self.rect.center().x;
        let y = self.rect.center().y - f64::from(style.size) / 2.0;
        text_renderer.draw_style(
            scene,
            &headline,
            on_card,
            translated(transform, cx - f64::from(tw) / 2.0, y),
            None,
            &style,
        );
        let sm = theme.text(TextRole::Caption);
        let (dw, _) = text_renderer.measure_style(&detail, None, &sm);
        text_renderer.draw_style(
            scene,
            &detail,
            on_card_dim,
            translated(
                transform,
                cx - f64::from(dw) / 2.0,
                y + f64::from(style.size) + 8.0,
            ),
            None,
            &sm,
        );
        self.draw_collaboration_overlay(scene, transform, text_renderer, theme, presence);
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

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// Only an unpresented window holds animation cadence: nothing else
    /// wakes the loop for its first paint. A later client commit is
    /// `needs_redraw` (one shot). Treating a pending sequence as cadence
    /// kept off-screen tiles — never drawn, so never cached — asking for
    /// frames at the refresh rate after the app itself had gone idle.
    fn needs_animation_frame(&self) -> bool {
        self.awaiting_first_paint()
    }

    fn needs_redraw(&self) -> bool {
        self.placement_dirty
            || self.awaiting_first_paint()
            || store().frame_seq(self.surface_id) != self.cached_seq
    }

    fn mark_drawn(&mut self) {
        self.placement_dirty = false;
    }
}

impl AppWidget for ForeignSurfaceWidget {
    fn uses_host_record(&self) -> bool {
        false
    }

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
    let Some(ws) = tree.desktop.active_workspace() else {
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
    for ws in &tree.desktop.workspaces {
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
    let Some(ws) = tree.desktop.active_workspace() else {
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
        if crate::shell::tab_binding::close_tab_for_destroyed_surface(rendered, gone.surface_id) {
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
        crate::shell::tab_binding::adopt_toplevel(
            rendered,
            add.surface_id,
            Some(add.app_id.clone()),
        );
        changed = true;
    }

    // Keep titles/app-ids in step for tabs on both sides (a client can call `set_app_id` after mapping).
    for want in &wanted {
        // A client may announce its toplevel before setting app-id.  The
        // protocol tree re-homes that anonymous tab when the late identity
        // arrives; apply the same claimed launch placement to the rendered
        // mirror before syncing its label, otherwise the two trees can show
        // the same surface in different groups.
        if crate::shell::tab_binding::app_id_for_surface(rendered, want.surface_id)
            .is_some_and(|app_id| app_id != want.app_id)
            && crate::shell::tab_binding::rebind_toplevel_for_app_id(
                rendered,
                want.surface_id,
                &want.app_id,
                Some(&want.title),
            )
        {
            changed = true;
        }
        for ws in &mut rendered.desktop.workspaces {
            for strip in &mut ws.strips {
                for group in &mut strip.window_groups {
                    for tab in &mut group.tabs {
                        if let ShellTile::Foreign { app_id, surface_id } = &mut tab.tile
                            && *surface_id == want.surface_id
                            && (*app_id != want.app_id || tab.title != want.title)
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
    // Resolve duplicates without mutating the cache during traversal.  The
    // ordinary close primitive updates both the cache and the Automerge
    // document and repairs active tab/group pointers.
    // A bare `retain` here used to make the duplicate disappear for one
    // frame while leaving it in the room document, so the next revision
    // resurrected it.
    let duplicate_tabs: Vec<_> = tree
        .desktop
        .workspaces
        .iter()
        .flat_map(|workspace| workspace.strips.iter())
        .flat_map(|strip| strip.window_groups.iter())
        .flat_map(|group| group.tabs.iter())
        .filter_map(|tab| {
            let ShellTile::Foreign { surface_id, .. } = &tab.tile else {
                return None;
            };
            (!seen.insert(*surface_id)).then_some(tab.id)
        })
        .collect();

    let mut removed = false;
    for tab in duplicate_tabs {
        removed |= crate::shell::tab_binding::remove_tab(tree, tab);
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
#[cfg(target_os = "linux")]
pub fn flush_configures(state: &mut crate::compositor::state::State) -> usize {
    let fallback = opening_fallback_size(state);
    // `toplevel_surfaces` borrows the shell state; a cheap clone releases the borrow before the configure calls below, which don't touch `State`.
    let toplevels: Vec<_> = state.xdg_shell_state.toplevel_surfaces().to_vec();
    // A fullscreen client is sized to its output by the fullscreen overlay;
    // its promoted tile still lays out underneath and must not resize it.
    let fullscreen = state
        .fullscreen_target
        .as_ref()
        .filter(|target| target.pinned_output.is_none())
        .map(|target| target.surface_id);
    let mut sent: usize = 0;
    for toplevel in toplevels {
        let id = crate::compositor::state::surface_id(toplevel.wl_surface());
        if store().is_sheet(id) {
            // A sheet sizes itself; its bounds were staged when it attached.
            if !toplevel.is_initial_configure_sent() {
                toplevel.send_configure();
                sent = sent.saturating_add(1);
            }
            continue;
        }
        let (min, max) =
            smithay::wayland::compositor::with_states(toplevel.wl_surface(), |states| {
                let mut cached = states
                    .cached_state
                    .get::<smithay::wayland::shell::xdg::SurfaceCachedState>();
                let current = cached.current();
                let limits = (
                    (current.min_size.w, current.min_size.h),
                    (current.max_size.w, current.max_size.h),
                );
                drop(cached);
                limits
            });
        store().note_size_limits(id, min, max);
        let tiled = !state.is_floating_toplevel(id);
        let scale = state.surface_scale(id);
        if fullscreen == Some(id) && toplevel.is_initial_configure_sent() {
            continue;
        }
        // A window only the person's other devices show renders at the
        // size they show it, not at its hidden tab's.
        if toplevel.is_initial_configure_sent()
            && let Some(extent) = store().remote_extent(id)
            && !is_visible_tab(&state.workspace_tree.read(), id)
        {
            if let Some((w, h)) = store().take_remote_configure(id, extent) {
                configure_to(&toplevel, id, (w, h), scale, tiled);
                sent = sent.saturating_add(1);
            }
            continue;
        }
        if let Some((w, h)) = store().take_pending_configure(id) {
            configure_to(&toplevel, id, (w, h), scale, tiled);
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
        configure_to(&toplevel, id, (w, h), scale, tiled);
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
#[cfg(target_os = "linux")]
fn configure_to(
    toplevel: &ToplevelSurface,
    id: SurfaceId,
    size: (i32, i32),
    scale: f64,
    tiled: bool,
) {
    use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State as Xdg;
    let (w, h) = within_client_texture_limit(size, scale);
    if (w, h) != size {
        // Whatever laid the tab out this large is a defect: say so, but
        // still send a size the client can render, since past the limit it
        // loses its drawing surface and closes the window.
        tracing::error!(
            target: "shell::compositor::foreign",
            surface_id = id,
            requested_width = size.0,
            requested_height = size.1,
            width = w,
            height = h,
            scale,
            "a tab was laid out larger than a client can render; configuring it to the largest size it can draw",
        );
    }
    toplevel.with_pending_state(|pending| {
        pending.size = Some((w, h).into());
        // A tile has a neighbour or the screen edge on every side. Saying so
        // is what makes a toolkit drop the shadow, rounded corners and resize
        // borders it would otherwise draw inside the tile.
        for edge in [
            Xdg::TiledLeft,
            Xdg::TiledRight,
            Xdg::TiledTop,
            Xdg::TiledBottom,
        ] {
            if tiled {
                pending.states.set(edge);
            } else {
                pending.states.unset(edge);
            }
        }
    });
    toplevel.send_configure();
}

/// Widest and tallest buffer, in physical pixels, a client is asked to
/// render: the 2D texture limit every GPU guarantees (WebGPU's default
/// `max_texture_dimension_2d`). A toolkit sized past it cannot create its
/// drawing surface at all.
pub const CLIENT_TEXTURE_LIMIT_PX: i32 = 8192;

/// `size` (logical pixels) kept within [`CLIENT_TEXTURE_LIMIT_PX`] once the
/// client renders it at `scale`.
#[must_use]
pub fn within_client_texture_limit(size: (i32, i32), scale: f64) -> (i32, i32) {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the limit over a positive scale is at most the limit itself, well inside i32"
    )]
    let limit = ((f64::from(CLIENT_TEXTURE_LIMIT_PX) / scale).floor() as i32).max(1);
    (size.0.min(limit), size.1.min(limit))
}

/// The size to configure an opening toplevel to when its tab was never
/// laid out, in surface-local (logical) pixels.
///
/// An approximation of a full-width window's tile: the output, less the
/// chrome around a strip. Not what layout would have given — it declined — but a size the window can honestly render at, close enough that a later correction is small.
#[cfg(target_os = "linux")]
fn opening_fallback_size(state: &crate::compositor::state::State) -> (i32, i32) {
    // `output_size` is the logical desktop, the unit a configure is in.
    let (out_w, out_h) = state.output_size;
    #[allow(
        clippy::cast_possible_truncation,
        reason = "clamped to [1.0, i32::MAX] and .round()-ed first, so the cast to i32 never \
                  truncates a fraction or wraps out of range"
    )]
    let logical = |px: f64| px.round().clamp(1.0, f64::from(i32::MAX)) as i32;
    let chrome_w = f64::from(tilo_desktop::sidebar_wheel_disc::SIDEBAR_RESERVE_PX);
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
/// freezes forever. Callbacks fire only for the *visible* tab of each group, also keeping a background client from burning CPU unseen,
/// and for a window the compositor asked to close, whose tab is already gone.
/// A window's subsurfaces and popups are drawn with it, so they are answered
/// with it: a toolkit that draws its menu on its own swapchain waits on that
/// surface's callback before it draws anything else.
///
/// Returns the number of surfaces signalled.
#[cfg(target_os = "linux")]
pub fn release_frame_throttle(state: &mut crate::compositor::state::State) -> usize {
    let mut signalled: usize = 0;
    for surface in throttled_surfaces(state) {
        if state.release_frame_callbacks(&surface) {
            signalled = signalled.saturating_add(1);
        }
    }
    signalled
}

/// Whether a surface [`release_frame_throttle`] serves is waiting for
/// `wl_surface.frame`. A callback can arrive on a commit without a new
/// buffer (a configure ack, a title). Nothing on screen changes, so the
/// loop must not build a frame for it; [`release_idle_frame_callbacks`]
/// fires it instead. A FIFO client draws nothing until the callback fires.
#[cfg(target_os = "linux")]
pub fn frame_callbacks_waiting(state: &crate::compositor::state::State) -> bool {
    throttled_surfaces(state).iter().any(|surface| {
        smithay::wayland::compositor::with_states(surface, |states| {
            !states
                .cached_state
                .get::<smithay::wayland::compositor::SurfaceAttributes>()
                .current()
                .frame_callbacks
                .is_empty()
        })
    })
}

/// Every surface whose frame callbacks [`release_frame_throttle`] answers:
/// each shown toplevel with its subsurfaces, and the popups (and their
/// subsurfaces) of a shown toplevel.
#[cfg(target_os = "linux")]
fn throttled_surfaces(
    state: &crate::compositor::state::State,
) -> Vec<smithay::reexports::wayland_server::protocol::wl_surface::WlSurface> {
    use smithay::desktop::{PopupKind, find_popup_root_surface};
    let mut surfaces = Vec::new();
    for toplevel in state.xdg_shell_state.toplevel_surfaces() {
        let surface = toplevel.wl_surface();
        if frame_throttle_releases(state, surface) {
            push_surface_tree(surface, &mut surfaces);
        }
    }
    for popup in state.xdg_popups() {
        let Ok(root) = find_popup_root_surface(&PopupKind::Xdg(popup.clone())) else {
            continue;
        };
        if frame_throttle_releases(state, &root) {
            push_surface_tree(popup.wl_surface(), &mut surfaces);
        }
    }
    surfaces
}

/// The toplevels shown on screen: each group's visible tab, the on-screen
/// keyboard, floating windows.
#[cfg(target_os = "linux")]
pub fn shown_toplevel_surfaces(
    state: &crate::compositor::state::State,
) -> Vec<smithay::reexports::wayland_server::protocol::wl_surface::WlSurface> {
    state
        .xdg_shell_state
        .toplevel_surfaces()
        .iter()
        .map(ToplevelSurface::wl_surface)
        .filter(|surface| frame_throttle_releases(state, surface))
        .cloned()
        .collect()
}

/// The toplevel whose surface is `id`.
#[cfg(target_os = "linux")]
pub fn toplevel_surface(
    state: &crate::compositor::state::State,
    id: SurfaceId,
) -> Option<smithay::reexports::wayland_server::protocol::wl_surface::WlSurface> {
    state
        .xdg_shell_state
        .toplevel_surfaces()
        .iter()
        .map(ToplevelSurface::wl_surface)
        .find(|surface| crate::compositor::state::surface_id(surface) == id)
        .cloned()
}

/// `surface` and its subsurfaces, parents first.
#[cfg(target_os = "linux")]
fn push_surface_tree(
    surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    out: &mut Vec<smithay::reexports::wayland_server::protocol::wl_surface::WlSurface>,
) {
    use smithay::wayland::compositor::{TraversalAction, with_surface_tree_downward};
    with_surface_tree_downward(
        surface,
        (),
        |_, _, ()| TraversalAction::DoChildren(()),
        |surface, _, ()| out.push(surface.clone()),
        |_, _, ()| true,
    );
}

/// Fire frame callbacks when this tick will not present.
///
/// The callback still has to run, or a FIFO swapchain waits forever and
/// takes every later sync with it. Building a scene only to release it
/// kept the shell busy while the apps sat still: the tree, the layout
/// and any animation had not changed.
///
/// Returns whether any callback was released.
#[cfg(target_os = "linux")]
pub fn release_idle_frame_callbacks(state: &mut crate::compositor::state::State) -> bool {
    if !frame_callbacks_waiting(state) {
        return false;
    }
    release_frame_throttle(state) > 0
}

/// Bumped when a client commit changes something the frame pump must see
/// (a buffer, a title, a new toplevel). A `wl_surface.frame` on its own does
/// not: answering it must not schedule another shell tick.
static CLIENT_PUMP_EPOCH: AtomicU64 = AtomicU64::new(0);

pub fn note_client_pump() {
    CLIENT_PUMP_EPOCH.fetch_add(1, Ordering::Release);
}

pub fn client_pump_epoch() -> u64 {
    CLIENT_PUMP_EPOCH.load(Ordering::Acquire)
}

/// Callbacks fire only for the visible tab of each group, the on-screen
/// keyboard, floating windows, windows asked to close and windows streamed
/// to a viewer.
#[cfg(target_os = "linux")]
fn frame_throttle_releases(
    state: &crate::compositor::state::State,
    surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
) -> bool {
    let id = crate::compositor::state::surface_id(surface);
    let visible_tab = {
        let tree = state.workspace_tree.read();
        is_visible_tab(&tree, store().sheet_root(id))
    };
    // A window streamed to another device paints there, so it keeps its
    // frame clock even while no tab here shows it.
    visible_tab
        || state.osk_surface == Some(id)
        || state.is_floating_toplevel(id)
        || state.is_closing_toplevel(id)
        || state.active_sessions.contains_key(&id)
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
    fn a_held_buffer_goes_back_once_after_its_last_frame() {
        let mut holds = BufferHolds::new();
        // Phase `a_re_attached_buffer_is_held_by_each_frame`.
        holds.hold("a");
        holds.hold("a");
        holds.hold("b");
        holds.returned.push("a");
        assert!(
            holds.give_back().is_empty(),
            "the newer frame still draws it"
        );
        // Phase `the_last_frame_gives_it_back_once`.
        holds.returned.extend(["a", "b"]);
        assert_eq!(holds.give_back(), vec!["a", "b"]);
        holds.returned.push("a");
        assert!(
            holds.give_back().is_empty(),
            "a buffer is released once, and one never held is not ours"
        );
        assert!(holds.held.is_empty());
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn capture_commits_publish_only_their_own_latest_frame() {
        fn pending_capture_queue_keeps_only_the_latest_frame_per_surface() {
            let mut pending = LatestJobs::default();
            assert_eq!(pending.push(10, "old"), None);
            assert_eq!(pending.push(20, "other surface"), None);
            assert_eq!(pending.push(10, "new"), Some("old"));

            assert_eq!(pending.jobs.len(), 2, "a burst is bounded by surface count");
            assert_eq!(pending.jobs.get(&10), Some(&"new"));
            assert_eq!(pending.remove(10), Some("new"));
            assert_eq!(pending.jobs.len(), 1);
            let drained: Vec<_> = std::iter::from_fn(|| pending.pop()).collect();
            assert_eq!(drained.len(), 1);
            assert!(drained.contains(&"other surface"));
        }

        #[cfg(target_os = "linux")]
        fn capture_barrier_release_is_visible_across_clones() {
            let barrier = Barrier::new(false);
            signal_capture_barrier(&barrier);
            assert!(barrier.is_signaled());
        }

        fn worker_result_is_published_only_when_its_blocked_commit_applies() {
            let frames = ForeignFrameStore::default();
            let id = sid(100);
            frames.with_entry(id, |entry| {
                entry.capture_tokens.push_back(2);
            });

            let mut stale = vec![1, 2, 3, 4];
            frames.stage_dmabuf_capture(
                id,
                1,
                Ok((1, 1, CaptureRoute::Dmabuf(DmabufRoute::Gpu))),
                &mut stale,
                None,
            );
            assert!(frames.snapshot(id).is_none());

            let mut latest = vec![5, 6, 7, 8];
            frames.stage_dmabuf_capture(
                id,
                2,
                Ok((1, 1, CaptureRoute::Dmabuf(DmabufRoute::Gpu))),
                &mut latest,
                None,
            );
            assert!(
                frames.snapshot(id).is_none(),
                "pending pixels must not become current before commit application"
            );
            assert!(matches!(
                frames.activate_dmabuf_capture(id),
                Some(CaptureOutcome::Dmabuf { .. })
            ));
            assert_eq!(
                frames.snapshot(id).expect("latest frame").rgba,
                vec![5, 6, 7, 8]
            );
            assert_eq!(
                frames.import_count(),
                1,
                "only published work counts as an import"
            );
        }

        fn superseded_commit_applies_without_stealing_the_newer_capture() {
            let frames = ForeignFrameStore::default();
            let id = sid(102);
            frames.with_entry(id, |entry| {
                entry.capture_tokens.extend([3, 4]);
            });
            let mut newest = vec![9, 8, 7, 6];
            frames.stage_dmabuf_capture(
                id,
                4,
                Ok((1, 1, CaptureRoute::Dmabuf(DmabufRoute::Gpu))),
                &mut newest,
                None,
            );

            assert!(
                frames.activate_dmabuf_capture(id).is_none(),
                "the superseded earlier commit has no pixels"
            );
            assert!(matches!(
                frames.activate_dmabuf_capture(id),
                Some(CaptureOutcome::Dmabuf { .. })
            ));
            assert_eq!(
                frames.snapshot(id).expect("newest frame").rgba,
                vec![9, 8, 7, 6]
            );
        }

        fn commit_token_mismatch_cannot_consume_a_later_capture() {
            let frames = ForeignFrameStore::default();
            let id = sid(103);
            frames.with_entry(id, |entry| entry.capture_tokens.push_back(42));
            let mut pixels = vec![7, 7, 7, 7];
            assert!(frames.stage_dmabuf_capture(
                id,
                42,
                Ok((1, 1, CaptureRoute::Dmabuf(DmabufRoute::Gpu))),
                &mut pixels,
                None,
            ));

            assert!(
                frames.activate_dmabuf_capture_token(id, 41).is_none(),
                "an unrelated commit must not pop the next buffer's token"
            );
            assert!(matches!(
                frames.activate_dmabuf_capture_token(id, 42),
                Some(CaptureOutcome::Dmabuf { .. })
            ));
        }

        fn late_worker_completion_cannot_resurrect_a_destroyed_surface() {
            let frames = ForeignFrameStore::default();
            let id = sid(101);
            frames.with_entry(id, |entry| {
                entry.capture_tokens.push_back(7);
            });
            frames.forget(id);

            let mut pixels = vec![1, 2, 3, 4];
            frames.stage_dmabuf_capture(
                id,
                7,
                Ok((1, 1, CaptureRoute::Dmabuf(DmabufRoute::Gpu))),
                &mut pixels,
                None,
            );
            assert!(frames.snapshot(id).is_none());
            assert_eq!(frames.frame_seq(id), 0);
        }

        pending_capture_queue_keeps_only_the_latest_frame_per_surface();
        #[cfg(target_os = "linux")]
        capture_barrier_release_is_visible_across_clones();
        worker_result_is_published_only_when_its_blocked_commit_applies();
        superseded_commit_applies_without_stealing_the_newer_capture();
        commit_token_mismatch_cannot_consume_a_later_capture();
        late_worker_completion_cannot_resurrect_a_destroyed_surface();
    }

    #[test]
    fn snapshots_composite_children_and_convert_pixel_orders() {
        fn snapshot_composites_positioned_children_with_alpha_and_clipping() {
            let frames = ForeignFrameStore::default();
            let root = sid(70);
            let child = sid(71);
            frames.publish_rgba(root, &[10, 20, 30, 255].repeat(9), 3, 3);
            frames.publish_rgba(
                child,
                &[
                    250, 0, 0, 255, 0, 250, 0, 128, 0, 0, 250, 255, 250, 250, 0, 255,
                ],
                2,
                2,
            );
            frames.set_children(
                root,
                vec![ChildFrame {
                    surface: child,
                    x: 2,
                    y: 1,
                }],
            );

            let snapshot = frames.snapshot(root).expect("composited root frame");
            let pixel = |x: usize, y: usize| &snapshot.rgba[(y * 3 + x) * 4..][..4];
            assert_eq!(pixel(0, 0), &[10, 20, 30, 255]);
            assert_eq!(pixel(2, 1), &[250, 0, 0, 255]);
            assert_eq!(pixel(2, 2), &[0, 0, 250, 255]);
            assert_eq!(snapshot.rgba.len(), 3 * 3 * 4, "child is clipped to root");
            let first_seq = frames.frame_seq(root);
            frames.publish_rgba(child, &[1, 2, 3, 255].repeat(4), 2, 2);
            assert_ne!(
                frames.frame_seq(root),
                first_seq,
                "child damage repaints root"
            );
            assert_eq!(frames.child_at(root, 2.25, 1.5), Some((child, 0.25, 0.5)));
            assert_eq!(frames.child_at(root, 0.5, 0.5), None);
        }

        #[cfg(any(test, target_os = "linux"))]
        fn swizzle_row_bgra_swaps_red_and_blue() {
            // Memory bytes for DRM_FORMAT_ARGB8888 are [B, G, R, A].
            let src = [10u8, 20, 30, 200, 1, 2, 3, 4];
            let mut out = Vec::new();
            swizzle_row(PixelOrder::Bgra { opaque: false }, &src, 2, &mut out);
            assert_eq!(out, vec![30, 20, 10, 200, 3, 2, 1, 4]);
        }

        #[cfg(any(test, target_os = "linux"))]
        fn swizzle_row_opaque_variant_forces_alpha() {
            let src = [10u8, 20, 30, 0x00];
            let mut out = Vec::new();
            swizzle_row(PixelOrder::Bgra { opaque: true }, &src, 1, &mut out);
            assert_eq!(out, vec![30, 20, 10, 0xFF]);
        }

        #[cfg(any(test, target_os = "linux"))]
        fn swizzle_row_rgba_passes_channels_through() {
            // DRM_FORMAT_ABGR8888 is already [R, G, B, A] in memory.
            let src = [10u8, 20, 30, 40];
            let mut out = Vec::new();
            swizzle_row(PixelOrder::Rgba { opaque: false }, &src, 1, &mut out);
            assert_eq!(out, vec![10, 20, 30, 40]);
        }

        #[cfg(target_os = "linux")]
        fn pixel_order_rejects_non_rgba_fourcc() {
            assert!(PixelOrder::for_fourcc(Fourcc::Nv12).is_none());
            assert!(PixelOrder::for_fourcc(Fourcc::Argb8888).is_some());
        }

        snapshot_composites_positioned_children_with_alpha_and_clipping();
        #[cfg(any(test, target_os = "linux"))]
        swizzle_row_bgra_swaps_red_and_blue();
        #[cfg(any(test, target_os = "linux"))]
        swizzle_row_opaque_variant_forces_alpha();
        #[cfg(any(test, target_os = "linux"))]
        swizzle_row_rgba_passes_channels_through();
        #[cfg(target_os = "linux")]
        pixel_order_rejects_non_rgba_fourcc();
    }

    #[test]
    fn ten_bit_and_half_float_rows_convert_for_the_sdr_desktop() {
        let sdr = crate::collab::colorimetry::Colorimetry::sdr_bt709();
        // XB30: red in the low bits. Full red, half green, zero blue.
        let word: u32 = 0xC008_03FF; // red 1023, green 512, padding set
        let mut out = Vec::new();
        convert_row(
            CpuLayout::Rgb10 {
                red_low: true,
                opaque: true,
            },
            &word.to_le_bytes(),
            sdr,
            &mut out,
        );
        assert_eq!(out, vec![255, 128, 0, 255]);
        // XR30 swaps red and blue.
        let mut out = Vec::new();
        convert_row(
            CpuLayout::Rgb10 {
                red_low: false,
                opaque: false,
            },
            &word.to_le_bytes(),
            sdr,
            &mut out,
        );
        assert_eq!(out, vec![0, 128, 255, 255]);
        // Half float: 1.0 = 0x3C00, 0.5 = 0x3800, alpha 0.5.
        let px: Vec<u8> = [0x3C00_u16, 0x3800, 0x0000, 0x3800]
            .iter()
            .flat_map(|h| h.to_le_bytes())
            .collect();
        let mut out = Vec::new();
        convert_row(CpuLayout::Half { opaque: false }, &px, sdr, &mut out);
        assert_eq!(out, vec![255, 128, 0, 128]);
        assert!((half_to_f32(0x3C00) - 1.0).abs() < f32::EPSILON);
        assert!((half_to_f32(0xC000) + 2.0).abs() < f32::EPSILON);
        assert!(half_to_f32(0x0001) > 0.0, "subnormals decode");
        // A PQ signal is tone-mapped rather than shown raw: PQ 0.58
        // (~200 cd/m² of a 1000 cd/m² master) lands darker than the
        // 148/255 an SDR reading of the same code would give.
        let pq = crate::collab::hdr_metadata::HdrConfig::pq_reference().colorimetry();
        let mid: u32 = 0x2529_4A52; // 594 in every channel
        let mut out = Vec::new();
        convert_row(
            CpuLayout::Rgb10 {
                red_low: true,
                opaque: true,
            },
            &mid.to_le_bytes(),
            pq,
            &mut out,
        );
        let red = out.first().copied().unwrap_or(0);
        assert!(
            (1..148).contains(&red),
            "tone-mapped below the raw 148, got {out:?}"
        );
    }

    #[test]
    fn hdr_frames_keep_a_full_precision_plane() {
        use crate::shell::hdr_surface::{HdrTexelFormat, SignalTransfer};
        let pq = crate::collab::colorimetry::Colorimetry::hdr10();
        let sdr = crate::collab::colorimetry::Colorimetry::sdr_bt709();

        // Phase `ten_bit_pq_rows_keep_their_codes_red_low`.
        {
            // XR30: blue in the low bits; blue 100, green 512, red 900.
            let word: u32 = 0x064 | (0x200 << 10) | (0x384 << 20);
            let row = [word.to_le_bytes(), word.to_le_bytes()].concat();
            let layout = CpuLayout::Rgb10 {
                red_low: false,
                opaque: true,
            };
            let mut rgba = Vec::new();
            let plane = convert_rows(layout, [row.as_slice()].into_iter(), (2, 1), pq, &mut rgba)
                .expect("a PQ description keeps its plane");
            assert_eq!(rgba.len(), 8, "the SDR rendition is still produced");
            assert_eq!(plane.format(), HdrTexelFormat::Rgb10a2);
            assert_eq!(plane.signal().transfer, SignalTransfer::Pq);
            let [r, g, b] = plane.signal_at(1, 0);
            assert!((r - 900.0 / 1023.0).abs() < 1e-6, "red {r}");
            assert!((g - 512.0 / 1023.0).abs() < 1e-6, "green {g}");
            assert!((b - 100.0 / 1023.0).abs() < 1e-6, "blue {b}");
            let first = u32::from_le_bytes(plane.bytes()[..4].try_into().expect("a word"));
            assert_eq!(first >> 30, 3, "padding reads as opaque alpha");
            let mut rgba = Vec::new();
            assert!(
                convert_rows(layout, [row.as_slice()].into_iter(), (2, 1), sdr, &mut rgba)
                    .is_none(),
                "an SDR description needs no plane"
            );
        }

        // Phase `half_float_rows_keep_their_values`.
        {
            let px: Vec<u8> = [0x4400_u16, 0x3C00, 0x3800, 0x0000]
                .iter()
                .flat_map(|h| h.to_le_bytes())
                .collect();
            let scrgb = scrgb();
            let mut rgba = Vec::new();
            let plane = convert_rows(
                CpuLayout::Half { opaque: true },
                [px.as_slice()].into_iter(),
                (1, 1),
                scrgb,
                &mut rgba,
            )
            .expect("scRGB keeps its plane");
            assert_eq!(plane.format(), HdrTexelFormat::Rgba16Float);
            assert_eq!(plane.signal_at(0, 0), [4.0, 1.0, 0.5]);
            assert_eq!(&plane.bytes()[6..8], &0x3C00_u16.to_le_bytes());
            assert_eq!(
                &rgba[..3],
                &[255, 255, 188],
                "linear light clips at SDR white"
            );
        }

        // Phase `snapshots_carry_the_plane_cropped_and_drop_it_under_subsurfaces`.
        {
            let frames = ForeignFrameStore::default();
            let root = sid(80);
            let signal = crate::shell::hdr_surface::SurfaceSignal::for_colorimetry(
                pq,
                crate::shell::hdr_tonemap::HdrTonemapConfig::default(),
            )
            .expect("PQ");
            let plane = crate::shell::hdr_surface::HdrPlane::from_signal_rgb(
                4,
                4,
                (0..16_u8).map(|i| [f32::from(i) / 16.0; 3]),
                signal,
            )
            .expect("plane");
            frames.publish_hdr_frame(root, &[9; 64], plane);
            let snapshot = frames.snapshot(root).expect("frame");
            let hdr = snapshot.hdr.expect("the plane travels with the frame");
            assert_eq!((hdr.width(), hdr.height()), (4, 4));

            frames.note_window_geometry(root, Some(geometry(1, 1, 2, 2)));
            let snapshot = frames.snapshot(root).expect("frame");
            let hdr = snapshot.hdr.expect("a cropped window keeps its plane");
            assert_eq!((hdr.width(), hdr.height()), (2, 2));
            assert_eq!((snapshot.width, snapshot.height), (2, 2));
            assert_eq!(hdr.signal_at(0, 0)[0], 5.0 / 16.0, "cut at the window");

            let child = sid(81);
            frames.publish_rgba(child, &[1, 2, 3, 255], 1, 1);
            frames.set_children(
                root,
                vec![ChildFrame {
                    surface: child,
                    x: 0,
                    y: 0,
                }],
            );
            assert!(
                frames.snapshot(root).expect("frame").hdr.is_none(),
                "subsurfaces exist only in the SDR rendition"
            );
            frames.set_children(root, Vec::new());
            frames.publish_rgba(root, &[9; 64], 4, 4);
            assert!(frames.snapshot(root).expect("frame").hdr.is_none());
        }
    }

    fn scrgb() -> crate::collab::colorimetry::Colorimetry {
        crate::collab::colorimetry::Colorimetry {
            primaries: crate::collab::colorimetry::ColorPrimaries::Bt709,
            transfer: crate::collab::colorimetry::TransferCharacteristics::Linear,
            matrix: crate::collab::colorimetry::MatrixCoefficients::Identity,
            range: crate::collab::colorimetry::ColorRange::Full,
        }
    }

    /// A tab is 1200x800. These are the sizes a client hands us.
    const TILE: Rect = Rect::new(0.0, 0.0, 1200.0, 800.0);

    #[test]
    fn foreign_window_status_copy_names_the_app_in_plain_words() {
        /// An app already on this machine can be on screen almost at once, so saying "on its way" that fast would flicker before anyone reads it.
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
        fn a_card_in_a_windows_place_names_the_app_not_its_id() {
            let w = ForeignSurfaceWidget::new(sid(53), "so.tilo.never-launched-fixture".into());
            let name = w.display_name();
            assert_eq!(
                name, "Never Launched Fixture",
                "a window's card must carry the app's name; got {name:?}",
            );
            assert!(
                !name.contains("so.tilo."),
                "a reverse-DNS package id reached a card somebody is reading",
            );
        }

        /// A window that has gone up with nothing in it says so, in words a
        /// person would use.
        ///
        /// This is the last thing between somebody and a window that will
        /// never fill in, so it names the app and says what happened —
        /// without the vocabulary of what went wrong. "waiting for first frame" over a package id taught nobody which app it was, or whether to keep waiting.
        fn a_window_that_cannot_be_shown_says_so_in_plain_words() {
            let id = sid(54);
            store().forget(id);
            let w = ForeignSurfaceWidget::new(id, "so.tilo.never-launched-fixture".into());

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
                    "so.tilo", "install",
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

        /// The window itself has to answer with the same name its tab does.
        ///
        /// The tab title was fixed once already; the widget kept handing back
        /// the raw wayland app-id from `app_title()`, so asking the tile directly (not the tab's stored title) got `so.tilo.chat`, an address nobody chose.
        fn a_foreign_window_answers_with_its_name_not_its_app_id() {
            let w = ForeignSurfaceWidget::new(3, "so.tilo.chat".into());
            assert_eq!(
                w.app_title(),
                "Chat",
                "a window says what it is called, not how it is addressed",
            );

            // A dotted id with a multi-word tail humanises the same way the package registry does, so an app reaches one name by either route.
            let w = ForeignSurfaceWidget::new(4, "so.tilo.word-processor".into());
            assert_eq!(w.app_title(), "Word Processor");
        }

        a_surface_just_heard_of_is_not_yet_worth_saying_anything_about();
        a_surface_still_starting_after_the_delay_says_so();
        a_surface_that_has_been_shown_is_never_opening_again();
        a_card_in_a_windows_place_names_the_app_not_its_id();
        a_window_that_cannot_be_shown_says_so_in_plain_words();
        a_foreign_window_answers_with_its_name_not_its_app_id();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn a_foreign_frame_is_shown_only_once_it_fits_or_must_explain_itself() {
        /// The window does not go up until there is a window to look at.
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

        a_surface_that_has_not_drawn_is_not_shown();
        a_frame_that_does_not_fit_the_tab_is_not_shown();
        a_frame_that_fits_the_tab_is_shown();
        a_shown_window_stays_shown_when_its_frame_stops_fitting();
        a_surface_whose_capture_failed_is_shown_so_it_can_say_why();
        a_frame_short_by_the_clients_own_decorations_is_shown();
        fills_accepts_a_buffer_sized_to_the_tab_and_nothing_else();
    }

    /// A `w`x`h` opaque RGBA frame whose neighbouring pixels differ.
    fn numbered_frame(w: u32, h: u32) -> Vec<u8> {
        (0..w.saturating_mul(h))
            .flat_map(|i| {
                let v = u8::try_from(i % 251).expect("fits");
                [v, v, v, 255]
            })
            .collect()
    }

    fn geometry(x: i32, y: i32, width: i32, height: i32) -> WindowGeometry {
        WindowGeometry {
            x,
            y,
            width,
            height,
            logical_size: None,
            buffer_scale: 1,
        }
    }

    /// A toolkit drawing its own shadow commits a buffer bigger than its
    /// window and says where the window is. The tile shows that rectangle —
    /// shadow and resize border are not part of a tiled window.
    #[test]
    fn a_declared_window_geometry_crops_the_frame_to_the_window() {
        let id = sid(60);
        store().forget(id);
        let (w, h) = (20u32, 16u32);
        let rgba = numbered_frame(w, h);
        store().publish_rgba(id, &rgba, w, h);
        store().note_window_geometry(id, Some(geometry(3, 2, 12, 10)));

        let frame = store().snapshot(id).expect("frame");
        assert_eq!((frame.width, frame.height), (12, 10));
        assert_eq!(store().frame_size(id), Some((12, 10)));
        for row in 0..10u32 {
            for col in 0..12u32 {
                let src = usize::try_from(((row + 2) * w + col + 3) * 4).expect("index");
                let dst = usize::try_from((row * 12 + col) * 4).expect("index");
                assert_eq!(
                    frame.rgba[dst..dst + 4],
                    rgba[src..src + 4],
                    "cropped pixel ({col}, {row}) must be buffer pixel ({}, {})",
                    col + 3,
                    row + 2,
                );
            }
        }

        // A shared window's stream is cut the same way from the buffer it commits.
        assert_eq!(
            store().crop_to_window(id, rgba.clone(), w, h),
            frame.rgba,
            "the stream shows what the tile shows"
        );

        store().note_window_geometry(id, None);
        assert_eq!(store().crop_to_window(id, rgba.clone(), w, h), rgba);
        let whole = store().snapshot(id).expect("frame");
        assert_eq!(
            (whole.width, whole.height),
            (w, h),
            "clearing geometry shows it all"
        );
        store().forget(id);
    }

    /// Geometry is logical; a scaled buffer holds it at the buffer scale, or
    /// at whatever ratio the viewport destination sets for fractional scaling.
    #[test]
    fn window_geometry_is_found_in_scaled_buffers() {
        let integer = WindowGeometry {
            buffer_scale: 2,
            ..geometry(10, 5, 100, 80)
        };
        assert_eq!(integer.crop(240, 180), Some((20, 10, 200, 160)));

        let fractional = WindowGeometry {
            logical_size: Some((120, 90)),
            ..geometry(10, 5, 100, 80)
        };
        assert_eq!(fractional.crop(180, 135), Some((15, 8, 150, 120)));

        assert_eq!(
            geometry(0, 0, 120, 90).crop(120, 90),
            None,
            "geometry covering the whole buffer is no crop"
        );
        assert_eq!(
            geometry(500, 500, 10, 10).crop(120, 90),
            None,
            "geometry outside the buffer must not blank the window"
        );
    }

    /// Pixels are drawn with the window geometry at the tile corner, so input
    /// must put surface-local `(x, y)` of the geometry corner at that corner too.
    #[test]
    fn input_origin_follows_the_window_geometry_offset() {
        let id = sid(61);
        store().forget(id);
        let tile = Rect::new(100.0, 50.0, 300.0, 250.0);
        store().publish_rgba(id, &numbered_frame(240, 240), 240, 240);
        assert_eq!(store().surface_origin(id, tile), (100.0, 50.0));
        store().note_window_geometry(id, Some(geometry(20, 20, 200, 200)));
        assert_eq!(
            store().surface_origin(id, tile),
            (80.0, 30.0),
            "the shadow's 20px lie left of and above the tile, out of sight"
        );
        let mut widget = ForeignSurfaceWidget::new(id, "x".into());
        widget.rect = tile;
        let dst = widget.fit_rect(200, 200).expect("fit");
        assert_eq!(
            (dst.x0 - 80.0, dst.y0 - 30.0),
            (20.0, 20.0),
            "the painted geometry corner is surface-local (20, 20)"
        );
        store().forget(id);
    }

    /// A window that may not grow as big as its tab is configured to its
    /// maximum and shown centred, and input follows it there.
    #[test]
    fn a_maximum_size_below_the_tab_centres_the_window() {
        let id = sid(62);
        store().forget(id);
        let tile = Rect::new(0.0, 0.0, 1200.0, 800.0);
        store().note_placement(id, tile);
        store().note_size_limits(id, (0, 0), (600, 0));
        assert_eq!(store().take_pending_configure(id), Some((600, 800)));
        let target = store().target_box(id, tile);
        assert_eq!(target, Rect::new(300.0, 0.0, 900.0, 800.0));

        store().publish_rgba(id, &vec![0; 600 * 800 * 4], 600, 800);
        let mut widget = ForeignSurfaceWidget::new(id, "x".into());
        widget.rect = tile;
        assert_eq!(
            widget.fit_rect(600, 800),
            Some(target),
            "drawn 1:1 in its centred box, not stretched across the tab"
        );
        assert_eq!(store().surface_origin(id, tile), (300.0, 0.0));
        assert!(
            store().mark_presented(id, tile),
            "a frame answering the clamped configure is the window's answer"
        );

        store().note_size_limits(id, (1400, 0), (0, 0));
        assert_eq!(
            store().take_pending_configure(id),
            Some((1400, 800)),
            "a minimum above the tab is honoured too"
        );
        assert_eq!(store().target_box(id, tile), tile);
        store().forget(id);
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn foreign_widget_configures_requests_frames_and_maps_geometry() {
        fn pending_configure_fires_once_per_size_change() {
            let id = sid(4);
            store().forget(id);
            assert_eq!(store().take_pending_configure(id), None);

            store().note_placement(id, Rect::new(0.0, 0.0, 1200.0, 800.0));
            assert_eq!(store().take_pending_configure(id), Some((1200, 800)));
            assert_eq!(
                store().take_pending_configure(id),
                None,
                "an unchanged layout must not re-send a configure every frame"
            );

            store().note_placement(id, Rect::new(0.0, 0.0, 600.0, 400.0));
            assert_eq!(store().take_pending_configure(id), Some((600, 400)));
            store().forget(id);
        }

        fn a_window_shown_only_elsewhere_takes_the_size_shown_there() {
            let id = sid(5);
            store().forget(id);
            store().note_placement(id, Rect::new(0.0, 0.0, 600.0, 400.0));
            assert_eq!(store().take_pending_configure(id), Some((600, 400)));

            assert_eq!(
                store().take_remote_configure(id, (1280, 720)),
                Some((1280, 720))
            );
            assert_eq!(store().take_remote_configure(id, (1280, 720)), None);
            store().note_size_limits(id, (0, 0), (1000, 0));
            assert_eq!(
                store().take_remote_configure(id, (1440, 900)),
                Some((1000, 900)),
                "the client's own limits still bound it"
            );
            // Shown here again, it goes back to its tab's size.
            assert_eq!(store().take_pending_configure(id), Some((600, 400)));

            let mut extents = HashMap::new();
            extents.insert(id, (800, 600));
            store().set_remote_extents(extents);
            assert_eq!(store().remote_extent(id), Some((800, 600)));
            store().set_remote_extents(HashMap::new());
            assert_eq!(store().remote_extent(id), None);
            store().forget(id);
        }

        /// A surface asks for frames until it has drawn, and after that only when
        /// a newer one is waiting. The first half matters because an
        /// undrawn-first-buffer client is the one case nothing else wakes the
        /// paint loop for; the second: an app with nothing newer must not hold the loop awake.
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
                !w.awaiting_first_paint(),
                "the paint gate itself must settle a fitting first frame; native asks it before draw"
            );
            assert!(
                !w.needs_animation_frame(),
                "an idle hosted app must not keep requesting frames after its first buffer landed"
            );
            assert!(
                w.needs_redraw(),
                "the undrawn first buffer still requests one paint"
            );
            assert!(w.refresh_cache(), "cache picks up the new frame");
            assert!(
                !w.needs_animation_frame(),
                "once drawn, an idle client must not keep the paint loop awake"
            );
            store().forget(id);
        }

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
        fn painted_frame_origin_matches_the_origin_input_is_mapped_with() {
            let tile = Rect::new(288.0, 124.0, 288.0 + 856.0, 124.0 + 868.0);
            let mut w = ForeignSurfaceWidget::new(sid(9), "x".into());
            w.rect = tile;

            // Every size a lagging client might still be presenting: exactly right, short, tall, narrow and wide.
            for (iw, ih) in [(856, 868), (856, 834), (856, 900), (800, 868), (900, 868)] {
                let dst = w.fit_rect(iw, ih).expect("fit");
                let (local_x, local_y) =
                    crate::shell::input_policy::surface_local((dst.x0, dst.y0), tile);
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

        fn moving_a_static_foreign_surface_requests_one_repaint() {
            use std::collections::HashMap;
            use taffy::prelude::TaffyMaxContent;
            use ui::widget::TaffyTreeExt;

            let id = sid(430);
            store().forget(id);
            store().publish_rgba(id, &vec![0; 1200 * 800 * 4], 1200, 800);
            let mut widget = ForeignSurfaceWidget::new(id, "dev.tilo.static".into());

            let mut taffy = TaffyTree::<()>::new();
            let mut nodes: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
            let node = widget.build_layout(&mut taffy, &Theme::dark(), &mut nodes);
            let root = taffy.parent_node(
                taffy::Style {
                    size: taffy::Size {
                        width: taffy::Dimension::length(1200.0),
                        height: taffy::Dimension::length(800.0),
                    },
                    ..Default::default()
                },
                &[node],
            );
            taffy
                .compute_layout(root, taffy::Size::MAX_CONTENT)
                .unwrap();
            widget.apply_layout(&taffy, node, Point::ZERO);
            assert!(widget.needs_redraw(), "first placement must paint");
            assert!(
                widget.refresh_cache(),
                "the first draw consumes the staged client frame",
            );
            widget.mark_drawn();
            assert!(!widget.needs_redraw(), "an unchanged static client idles");

            widget.apply_layout(&taffy, node, Point::new(248.0, 136.0));
            assert!(
                widget.needs_redraw(),
                "moving cached client pixels must invalidate the retained scene",
            );
            assert!(
                !widget.needs_animation_frame(),
                "a placement edge repaints once rather than starting an animation loop",
            );
            widget.mark_drawn();
            assert!(!widget.needs_redraw());
            store().forget(id);
        }

        pending_configure_fires_once_per_size_change();
        a_window_shown_only_elsewhere_takes_the_size_shown_there();
        widget_asks_for_frames_until_it_has_drawn_then_only_for_a_pending_one();
        widget_fit_rect_letterboxes_preserving_aspect();
        painted_frame_origin_matches_the_origin_input_is_mapped_with();
        moving_a_static_foreign_surface_requests_one_repaint();
    }

    /// Off-screen tiles never `draw()`, so `cached_seq` stays stale. A
    /// pending client sequence used to keep `needs_animation_frame` true
    /// forever and pin the host at the refresh rate. Fail if an idle
    /// hosted app still requests frames after its first buffer landed.
    #[test]
    fn idle_hosted_app_does_not_keep_requesting_frames() {
        let id = sid(71);
        store().forget(id);
        let mut w = ForeignSurfaceWidget::new(id, "org.example.app".into());
        w.rect = Rect::new(0.0, 0.0, 1.0, 1.0);
        store().publish_rgba(id, &[255, 0, 0, 255], 1, 1);
        assert!(
            !w.awaiting_first_paint(),
            "the first fitting buffer has already settled the paint gate"
        );
        assert!(
            !w.needs_animation_frame(),
            "an idle hosted app must not keep requesting frames"
        );
        store().forget(id);
    }

    /// Tests that drive the process-wide SHM capture worker: its
    /// "published" flag is global, so they take turns.
    #[cfg(target_os = "linux")]
    static SHM_WORKER_TESTS: Mutex<()> = Mutex::new(());

    /// An idle desktop paints only when the loop sees a change, and frame
    /// callbacks are released only after a paint. An SHM frame the worker
    /// published without reporting it left the client waiting for
    /// `wl_surface.frame` forever: headless guests froze every app.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_published_shm_frame_is_reported_to_the_loop() {
        let _worker = SHM_WORKER_TESTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = sid(72);
        store().forget(id);
        let _ = take_published_shm_frames();
        assert!(
            store()
                .prepare_shm_capture(id, OwnedShmFrame::argb8888(vec![0, 0, 255, 255], 1, 1))
                .is_some()
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !take_published_shm_frames() {
            assert!(
                std::time::Instant::now() < deadline,
                "the loop must learn a client frame landed, or it never paints nor releases frame callbacks"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_ne!(store().frame_seq(id), 0, "reported only once published");
        assert!(!take_published_shm_frames(), "reported once");
        store().forget(id);
    }

    /// A client that commits and closes its window at once leaves the shm
    /// worker building a frame for a surface that is gone. A closing tile
    /// still placing itself makes an empty entry for it again; publishing
    /// into that kept a whole frame nothing freed: a window's worth of
    /// compositor memory per close.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_shm_frame_finished_after_its_surface_went_is_dropped() {
        let frames = ForeignFrameStore::default();
        let queue = |id: SurfaceId| {
            let (generation, damage_generation) = frames.with_entry(id, |entry| {
                entry.shm_queued = entry.shm_queued.wrapping_add(1);
                (entry.shm_queued, entry.damage_history.begin(None))
            });
            ShmJob {
                surface: id,
                generation,
                damage_generation,
                patches: Vec::new(),
            }
        };
        let tracked = |id: SurfaceId| {
            frames
                .entries
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&id)
                .map(|entry| entry.rgba.len())
        };
        let pixels = || Arc::new(vec![0x40; 64 * 64 * 4]);

        let gone = sid(74);
        let job = queue(gone);
        frames.forget(gone);
        assert!(!frames.publish_shm_frame(&job, pixels(), (64, 64)));
        assert_eq!(
            tracked(gone),
            None,
            "the late frame brought the surface back"
        );

        frames.note_placement(gone, Rect::new(0.0, 0.0, 64.0, 64.0));
        assert!(!frames.publish_shm_frame(&job, pixels(), (64, 64)));
        frames.note_shm_failure(&job, "late".into());
        assert_eq!(
            tracked(gone),
            Some(0),
            "the late frame filled a re-created entry"
        );
        assert_eq!(frames.last_error(gone), None, "the late failure reached it");
        frames.forget(gone);

        let live = sid(75);
        let job = queue(live);
        assert!(frames.publish_shm_frame(&job, pixels(), (64, 64)));
        let snapshot = frames
            .snapshot(live)
            .expect("a live surface gets its frame");
        assert_eq!((snapshot.width, snapshot.height), (64, 64));
        frames.forget(live);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn shm_readback_cannot_map_a_pending_or_detached_surface() {
        let id = sid(73);
        store().forget(id);
        let first = store()
            .prepare_shm_capture(id, OwnedShmFrame::argb8888(vec![0, 0, 255, 255], 1, 1))
            .expect("queue the first SHM readback");
        let deadline = Instant::now() + Duration::from_secs(5);
        while store().frame_seq(id) == 0 {
            assert!(
                Instant::now() < deadline,
                "the actual SHM worker must publish"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!store().is_mapped(id), "readback is not a protocol commit");
        assert!(!store().note_shm_committed(id, first + 1));
        assert!(!store().is_mapped(id), "a generation not queued cannot map");
        assert!(store().note_shm_committed(id, first));
        assert!(store().is_mapped(id));
        store().clear_frame(id);
        let sequence = store().frame_seq(id);
        let second = store()
            .prepare_shm_capture(id, OwnedShmFrame::argb8888(vec![255, 0, 0, 255], 1, 1))
            .expect("queue readback while the surface is detached");
        let deadline = Instant::now() + Duration::from_secs(5);
        while store().frame_seq(id) == sequence {
            assert!(
                Instant::now() < deadline,
                "the second actual SHM readback must publish"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            !store().is_mapped(id),
            "worker completion cannot remap a detached surface"
        );
        assert!(store().note_shm_committed(id, second));
        assert!(
            store().is_mapped(id),
            "only the new buffer commit remaps it"
        );
        store().forget(id);
    }

    /// The tile draws a CPU frame from the stored pixels themselves: painting
    /// copies nothing, and the frame store's lock never guards a copy.
    #[test]
    fn frame_image_shares_the_published_frame() {
        let id = sid(586);
        store().forget(id);
        store().publish_rgba(id, &[1, 2, 3, 4, 5, 6, 7, 8], 2, 1);
        let (first, _) = store().frame_image(id).expect("frame image");
        let (second, _) = store().frame_image(id).expect("frame image");
        assert_eq!(first.data.data(), [1, 2, 3, 4, 5, 6, 7, 8]);
        assert_eq!(
            first.data.data().as_ptr(),
            second.data.data().as_ptr(),
            "both images share the stored pixels"
        );
        assert!(
            std::ptr::eq(
                first.data.data().as_ptr(),
                store().with_entry(id, |entry| entry.rgba.as_ptr())
            ),
            "the image is the frame store's own allocation"
        );
        store().publish_rgba(id, &[9; 8], 2, 1);
        assert_eq!(
            first.data.data(),
            [1, 2, 3, 4, 5, 6, 7, 8],
            "a newer frame never writes into pixels an image still draws"
        );
        store().forget(id);
    }

    /// A damaged-only SHM copy is laid over the current frame, and the frame
    /// it replaced is written into next instead of a fresh full copy.
    #[cfg(target_os = "linux")]
    #[test]
    fn damaged_shm_commits_build_on_the_current_frame_and_reuse_the_spare() {
        fn wait_seq(id: SurfaceId, seq: u64) {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while store().frame_seq(id) < seq {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the SHM worker never published commit {seq}"
                );
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        }
        fn pixels(id: SurfaceId) -> Vec<u8> {
            store().snapshot(id).expect("published frame").rgba
        }
        fn stored(id: SurfaceId) -> *const u8 {
            store().with_entry(id, |entry| entry.rgba.as_ptr())
        }
        let _worker = SHM_WORKER_TESTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = sid(587);
        store().forget(id);
        // A 3×2 frame; wire bytes are [B, G, R, A].
        let red = [0, 0, 255, 255].repeat(6);
        store()
            .prepare_shm_capture(id, OwnedShmFrame::argb8888(red, 3, 2))
            .expect("worker");
        wait_seq(id, 1);
        let first = stored(id);
        store()
            .prepare_shm_capture(id, damaged_pixel(1, 0, [255, 0, 0, 255]))
            .expect("worker");
        wait_seq(id, 2);
        let red_rgba = [255, 0, 0, 255];
        let blue_rgba = [0, 0, 255, 255];
        let green_rgba = [0, 255, 0, 255];
        assert_eq!(
            pixels(id),
            [red_rgba, blue_rgba, red_rgba, red_rgba, red_rgba, red_rgba].concat()
        );
        assert_ne!(stored(id), first, "the current frame was never written");
        store()
            .prepare_shm_capture(id, damaged_pixel(2, 1, [0, 255, 0, 255]))
            .expect("worker");
        wait_seq(id, 3);
        assert_eq!(
            pixels(id),
            [
                red_rgba, blue_rgba, red_rgba, red_rgba, red_rgba, green_rgba
            ]
            .concat(),
            "the spare caught up on the damage it missed"
        );
        assert_eq!(
            stored(id),
            first,
            "the frame nothing drew any more was written into again"
        );
        store().forget(id);
    }

    /// Commits the worker has not reached yet are kept as a chain of copies:
    /// a newer damaged copy cannot stand in for an older one, a whole one can.
    #[cfg(target_os = "linux")]
    #[test]
    fn queued_shm_copies_chain_until_a_whole_copy() {
        let job = |patches| ShmJob {
            surface: sid(588),
            generation: 0,
            damage_generation: 0,
            patches,
        };
        let whole = |fill, generation| ShmPatch {
            frame: OwnedShmFrame::argb8888(vec![fill; 16], 2, 2),
            generation,
            based_on: None,
        };
        let mut newer = job(vec![ShmPatch {
            frame: OwnedShmFrame::argb8888_damage(vec![1; 4], (2, 2), vec![pixel_region(0, 0)]),
            generation: 2,
            based_on: Some(1),
        }]);
        let moot = newer.absorb(job(vec![whole(2, 1)]));
        assert!(moot.is_empty());
        assert_eq!(newer.patches.len(), 2);
        assert!(
            newer.patches[0].based_on.is_none(),
            "older whole copy first"
        );
        assert_eq!(newer.copied_bytes(), 20);
        let mut latest = job(vec![whole(3, 3)]);
        let moot = latest.absorb(newer);
        assert_eq!(moot.len(), 2, "a whole copy supersedes everything older");
        assert_eq!(latest.patches.len(), 1);
    }

    #[cfg(target_os = "linux")]
    fn pixel_region(x: usize, y: usize) -> crate::compositor::surface_readback::ShmRegion {
        crate::compositor::surface_readback::ShmRegion {
            x,
            y,
            width: 1,
            height: 1,
        }
    }

    /// A 1-pixel damaged-only `Argb8888` copy of a 3×2 buffer.
    #[cfg(target_os = "linux")]
    fn damaged_pixel(x: usize, y: usize, bgra: [u8; 4]) -> OwnedShmFrame {
        OwnedShmFrame::argb8888_damage(bgra.to_vec(), (3, 2), vec![pixel_region(x, y)])
    }

    /// Wait until the SHM worker is done with every commit queued for `id`.
    #[cfg(target_os = "linux")]
    fn wait_shm_done(id: SurfaceId) {
        let deadline = std::time::Instant::now()
            .checked_add(std::time::Duration::from_secs(5))
            .expect("a deadline five seconds out");
        while store().with_entry(id, |entry| entry.shm_queued > entry.shm_done) {
            assert!(
                std::time::Instant::now() < deadline,
                "the SHM worker never finished the queued commits"
            );
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    /// A damaged SHM commit after a dmabuf commit is relative to the dmabuf
    /// frame, which the SHM worker never built: laying it over the last SHM
    /// frame would show that frame's stale pixels. It is dropped, the
    /// dmabuf frame stays, and the next SHM commit copies whole.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_damaged_shm_commit_after_a_dmabuf_frame_is_never_laid_over_the_old_shm_frame() {
        let _worker = SHM_WORKER_TESTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = sid(589);
        store().forget(id);
        let red = [0, 0, 255, 255].repeat(6);
        store()
            .prepare_shm_capture(id, OwnedShmFrame::argb8888(red, 3, 2))
            .expect("worker");
        wait_shm_done(id);
        // A dmabuf commit read back on the CPU, activated as its commit applies.
        store().with_entry(id, |entry| entry.capture_tokens.push_back(589));
        let mut green = [0, 255, 0, 255].repeat(6);
        assert!(store().stage_dmabuf_capture(
            id,
            589,
            Ok((3, 2, CaptureRoute::Dmabuf(DmabufRoute::Gpu))),
            &mut green,
            None,
        ));
        assert!(store().activate_dmabuf_capture_token(id, 589).is_some());
        assert!(
            !store().has_shm_base(id),
            "a dmabuf frame clears the SHM base"
        );
        let shown = store().snapshot(id).expect("dmabuf frame").rgba;
        store()
            .prepare_shm_capture(id, damaged_pixel(1, 0, [255, 0, 0, 255]))
            .expect("worker");
        wait_shm_done(id);
        assert_eq!(
            store().snapshot(id).expect("frame").rgba,
            shown,
            "damage with no base frame is never published"
        );
        assert!(
            !store().has_shm_base(id),
            "the next SHM commit copies whole"
        );
        store().forget(id);
    }

    /// After an unmap or a failed capture there is nothing to lay damage
    /// over: a damaged-only commit is dropped, not drawn over zeros, and it
    /// does not map the surface again.
    #[cfg(target_os = "linux")]
    #[test]
    fn damage_after_an_unmap_or_failure_is_dropped_not_zero_filled() {
        let _worker = SHM_WORKER_TESTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = sid(590);
        for reset in [(|id| store().clear_frame(id)) as fn(SurfaceId), |id| {
            store().note_failure(id, "test failure".into());
        }] {
            store().forget(id);
            store()
                .prepare_shm_capture(id, OwnedShmFrame::argb8888([7; 24].to_vec(), 3, 2))
                .expect("worker");
            wait_shm_done(id);
            reset(id);
            let seq = store().frame_seq(id);
            let mapped = store().with_entry(id, |entry| entry.mapped);
            store()
                .prepare_shm_capture(id, damaged_pixel(0, 0, [1, 2, 3, 255]))
                .expect("worker");
            wait_shm_done(id);
            assert_eq!(store().frame_seq(id), seq, "nothing was published");
            assert!(store().snapshot(id).is_none());
            assert_eq!(store().with_entry(id, |entry| entry.mapped), mapped);
        }
        store().forget(id);
    }

    /// A copy made against a base that was cleared before it was queued
    /// does not reinstate that base: the commit that cleared it came later.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_base_cleared_while_a_commit_is_copied_stays_cleared() {
        let Ok(worker) = ASYNC_SHM_CAPTURE.get_or_init(AsyncShmCapture::spawn) else {
            panic!("the SHM capture worker must start");
        };
        let _worker = SHM_WORKER_TESTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = sid(591);
        store().forget(id);
        store()
            .prepare_shm_capture(id, OwnedShmFrame::argb8888([7; 24].to_vec(), 3, 2))
            .expect("worker");
        wait_shm_done(id);
        let read = store().read_shm_base(id);
        assert!(read.base.is_some(), "an SHM commit is a base");
        // Cleared between reading the base and queueing the copy.
        store().reset_shm_base(id);
        let frame = damaged_pixel(2, 1, [1, 2, 3, 255]);
        store().queue_shm_frame(worker, id, (frame, read), None);
        assert!(
            !store().has_shm_base(id),
            "a stale copy must not make itself the next commit's base"
        );
        wait_shm_done(id);
        assert!(!store().has_shm_base(id));
        store().forget(id);
    }

    /// The worker lays damage only over the exact commit it was copied
    /// against; any other generation drops it and forces a whole copy.
    #[cfg(target_os = "linux")]
    #[test]
    fn damage_against_another_commit_is_dropped() {
        let Ok(worker) = ASYNC_SHM_CAPTURE.get_or_init(AsyncShmCapture::spawn) else {
            panic!("the SHM capture worker must start");
        };
        let _worker = SHM_WORKER_TESTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = sid(592);
        store().forget(id);
        store()
            .prepare_shm_capture(id, OwnedShmFrame::argb8888([7; 24].to_vec(), 3, 2))
            .expect("worker");
        wait_shm_done(id);
        let shown = store().snapshot(id).expect("frame").rgba;
        let read = store().read_shm_base(id);
        let base = read.base.expect("an SHM commit is a base");
        let older = ShmBaseRead {
            base: Some(ShmBaseCommit {
                generation: base.generation.wrapping_sub(1),
                ..base
            }),
            ..read
        };
        store().queue_shm_frame(
            worker,
            id,
            (damaged_pixel(0, 0, [1, 2, 3, 255]), older),
            None,
        );
        wait_shm_done(id);
        assert_eq!(store().snapshot(id).expect("frame").rgba, shown);
        assert!(
            !store().has_shm_base(id),
            "the next SHM commit copies whole"
        );
        store().forget(id);
    }

    /// Damage relative to a buffer of another format is never laid over it:
    /// an `X` buffer's pixels were forced opaque, an `A` buffer's were not.
    #[cfg(target_os = "linux")]
    #[test]
    fn damage_against_a_buffer_of_another_format_is_dropped() {
        let _worker = SHM_WORKER_TESTS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = sid(593);
        store().forget(id);
        store()
            .prepare_shm_capture(id, OwnedShmFrame::argb8888([7; 24].to_vec(), 3, 2))
            .expect("worker");
        wait_shm_done(id);
        let shown = store().snapshot(id).expect("frame").rgba;
        let xrgb =
            OwnedShmFrame::xrgb8888_damage(vec![1, 2, 3, 0], (3, 2), vec![pixel_region(0, 0)]);
        store().prepare_shm_capture(id, xrgb).expect("worker");
        wait_shm_done(id);
        assert_eq!(store().snapshot(id).expect("frame").rgba, shown);
        assert!(
            !store().has_shm_base(id),
            "the next SHM commit copies whole"
        );
        store().forget(id);
    }

    #[test]
    fn frame_store_publishes_retries_fails_and_forgets_by_sequence() {
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

        fn an_empty_widget_cache_retries_the_current_nonzero_sequence() {
            let id = sid(23);
            store().forget(id);
            store().publish_rgba(id, &[0xE0, 0x18, 0xA0, 0xFF], 1, 1);

            let mut widget = ForeignSurfaceWidget::new(id, "org.example.retry".into());
            // Model the interrupted observation that previously pinned the
            // fallback card: the sequence was recorded but no image survived.
            widget.cached_seq = store().frame_seq(id);
            widget.cached = None;
            assert!(widget.refresh_cache(), "the empty cache must retry");
            assert!(
                widget.cached.is_some(),
                "the current frame must be restored"
            );

            store().forget(id);
        }

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
            assert!(
                store().is_mapped(id),
                "a capture failure does not unmap the client or revoke its input eligibility"
            );
            store().forget(id);
        }

        fn clear_frame_unmaps_without_forgetting_placement() {
            let id = sid(3);
            store().forget(id);
            store().note_placement(id, Rect::new(0.0, 0.0, 800.0, 600.0));
            store().publish_rgba(id, &[7; 16], 2, 2);
            store().clear_frame(id);
            assert!(store().snapshot(id).is_none());
            assert!(
                !store().is_mapped(id),
                "attach(NULL) must revoke the same map-based input eligibility it created"
            );
            assert_eq!(
                store().viewport(id),
                Some((800, 600)),
                "unmap keeps the tab's placement; only destroy forgets it"
            );
            store().forget(id);
            assert_eq!(store().viewport(id), None);
        }

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

        publish_then_snapshot_round_trips_and_bumps_seq();
        an_empty_widget_cache_retries_the_current_nonzero_sequence();
        note_failure_drops_the_stale_frame_and_records_the_reason();
        clear_frame_unmaps_without_forgetting_placement();
        frame_seq_forget_resets_to_zero();
    }

    #[test]
    #[serial_test::serial(process_env)]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn reconcile_carries_adoptions_teardowns_and_late_app_ids_across_trees() {
        fn reconcile_copies_adoptions_and_teardowns_across_trees() {
            let mut source = WorkspaceTree::bootstrap_demo();
            let mut rendered = WorkspaceTree::bootstrap_demo();
            assert!(
                !reconcile_foreign_tabs(&source, &mut rendered),
                "two pristine trees need no mirroring"
            );

            crate::shell::tab_binding::adopt_toplevel(
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

            crate::shell::tab_binding::close_tab_for_destroyed_surface(&mut source, 4242);
            assert!(reconcile_foreign_tabs(&source, &mut rendered));
            assert!(foreign_tabs(&rendered).is_empty());
        }

        /// The rendered mirror must follow a late app-id re-home as well as the
        /// protocol tree.  This is the two-tree version of the Electron/GTK race:
        /// the first anonymous adoption is visible for one tick, then the launch
        /// note is claimed when the client finally identifies itself.
        fn reconcile_mirrors_late_app_id_launch_placement() {
            crate::shell::launcher::clear_pending_placements();
            let mut source = WorkspaceTree::bootstrap_demo();
            let mut rendered = source.clone();
            let target = source.desktop.workspaces[0].strips[0].window_groups[0].id;
            crate::shell::launcher::set_pending_placement(
                "org.example.electron",
                crate::shell::tab_binding::Placement::TabIn(target),
            );

            crate::shell::tab_binding::adopt_toplevel(&mut source, 0x501, None);
            assert!(reconcile_foreign_tabs(&source, &mut rendered));
            assert_eq!(foreign_tabs_anywhere(&rendered).len(), 1);

            assert!(crate::shell::tab_binding::rebind_toplevel_for_app_id(
                &mut source,
                0x501,
                "org.example.electron",
                None,
            ));
            assert!(reconcile_foreign_tabs(&source, &mut rendered));
            let mirrored = foreign_tabs_anywhere(&rendered);
            assert_eq!(mirrored.len(), 1);
            let mirrored_group = rendered
                .desktop
                .workspaces
                .iter()
                .flat_map(|workspace| workspace.strips.iter())
                .flat_map(|strip| strip.window_groups.iter())
                .find(|group| group.tabs.iter().any(|tab| tab.id == mirrored[0].tab_id))
                .expect("mirrored surface group");
            assert_eq!(mirrored_group.id, target);
            crate::shell::launcher::clear_pending_placements();
        }

        /// Satellite can give the rendered mirror a provisional non-anonymous
        /// class before the protocol tree receives the final `WM_CLASS`. The
        /// mirror must re-home on that identity transition too; restricting this
        /// path to `anon-*` leaves protocol and chrome in different groups.
        fn reconcile_rehomes_provisional_x11_identity_to_final_launch_placement() {
            crate::shell::launcher::clear_pending_placements();
            let mut source = WorkspaceTree::bootstrap_demo();
            let mut rendered = source.clone();
            let target = source.desktop.workspaces[0].strips[0].window_groups[0].id;
            let surface = 0x502;
            crate::shell::launcher::set_pending_placement_for_identities(
                "chatgpt",
                &["chatgpt".to_string()],
                crate::shell::tab_binding::Placement::NewGroup,
            );

            crate::shell::tab_binding::adopt_toplevel_at(
                &mut source,
                surface,
                Some("electron".into()),
                crate::shell::tab_binding::Placement::TabIn(target),
            );
            assert!(reconcile_foreign_tabs(&source, &mut rendered));
            assert!(crate::shell::tab_binding::rebind_toplevel_for_app_id(
                &mut source,
                surface,
                "Chatgpt",
                Some("ChatGPT"),
            ));
            assert!(reconcile_foreign_tabs(&source, &mut rendered));

            let mirrored = foreign_tabs_anywhere(&rendered);
            assert_eq!(mirrored.len(), 1);
            assert_eq!(mirrored[0].app_id, "Chatgpt");
            assert_eq!(mirrored[0].title, "ChatGPT");
            let mirrored_group = rendered.desktop.workspaces[0].strips[0]
                .window_groups
                .iter()
                .find(|group| group.tabs.iter().any(|tab| tab.id == mirrored[0].tab_id))
                .expect("mirrored provisional surface group");
            assert_ne!(mirrored_group.id, target);
            crate::shell::launcher::clear_pending_placements();
        }

        fn reconcile_propagates_a_late_set_app_id() {
            let mut source = WorkspaceTree::bootstrap_demo();
            let mut rendered = WorkspaceTree::bootstrap_demo();
            crate::shell::tab_binding::adopt_toplevel(&mut source, 77, None);
            assert!(reconcile_foreign_tabs(&source, &mut rendered));
            assert_eq!(foreign_tabs(&rendered)[0].app_id, "anon-77");

            crate::shell::tab_binding::rename_tab_for_app_id(&mut source, 77, "io.example.late");
            assert!(reconcile_foreign_tabs(&source, &mut rendered));
            let t = &foreign_tabs(&rendered)[0];
            assert_eq!(t.app_id, "io.example.late");
            // The identifier propagates; the *title* is humanised from it rather than shown as a machine identifier nobody chose.
            assert_eq!(
                t.title, "Late",
                "a tab wears a name, not the last segment of an app id"
            );
        }

        reconcile_copies_adoptions_and_teardowns_across_trees();
        reconcile_mirrors_late_app_id_launch_placement();
        reconcile_rehomes_provisional_x11_identity_to_final_launch_placement();
        reconcile_propagates_a_late_set_app_id();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn a_foreign_window_is_adopted_exactly_once() {
        /// Consuming a user close removes the exact surface from the protocol
        /// tree before reconciliation. The Wayland toplevel itself can still be
        /// alive while processing `xdg_toplevel.close`; it must not be mirrored
        /// back, and another window from the same application must be untouched.
        fn a_still_live_closing_surface_is_not_readopted_or_confused_with_its_sibling() {
            let mut source = WorkspaceTree::bootstrap_demo();
            let mut rendered = WorkspaceTree::bootstrap_demo();
            for surface in [51, 52] {
                crate::shell::tab_binding::adopt_toplevel(
                    &mut source,
                    surface,
                    Some("com.openai.chatgpt".into()),
                );
            }
            assert!(reconcile_foreign_tabs(&source, &mut rendered));
            assert_eq!(tabs_for_surface(&rendered, 51), 1);
            assert_eq!(tabs_for_surface(&rendered, 52), 1);

            // This is the frame-loop ordering for a queued close: remove from
            // both tree authorities, send close to the still-live toplevel, then
            // reconcile. The application's process and app-id are irrelevant.
            assert!(crate::shell::tab_binding::close_tab_for_destroyed_surface(
                &mut rendered,
                51,
            ));
            assert!(crate::shell::tab_binding::close_tab_for_destroyed_surface(
                &mut source,
                51,
            ));
            assert!(
                !reconcile_foreign_tabs(&source, &mut rendered),
                "the consumed close leaves nothing for reconciliation to undo"
            );
            assert_eq!(
                tabs_for_surface(&rendered, 51),
                0,
                "the still-live closing surface must not be adopted back"
            );
            assert_eq!(
                tabs_for_surface(&rendered, 52),
                1,
                "surface identity keeps the same-app sibling exactly once"
            );
        }

        /// One window, one tab — wherever in the tree that tab sits, including a workspace other than the one the user is looking at.
        fn a_window_in_another_workspace_is_not_adopted_a_second_time() {
            let mut source = WorkspaceTree::bootstrap_demo();
            let mut rendered = WorkspaceTree::bootstrap_demo();
            let surface = 31;

            crate::shell::tab_binding::adopt_toplevel(
                &mut source,
                surface,
                Some("so.tilo.word-processor".into()),
            );
            assert!(reconcile_foreign_tabs(&source, &mut rendered));
            assert_eq!(
                tabs_for_surface(&rendered, surface),
                1,
                "the mirror should adopt the window exactly once",
            );

            // The user goes to another workspace — nothing about the window changed, it's simply no longer the one on screen.
            let elsewhere = rendered
                .desktop
                .workspaces
                .iter()
                .map(|w| w.id)
                .find(|id| *id != rendered.desktop.active)
                .expect("the demo tree has more than one workspace");
            rendered.desktop.active = elsewhere;

            reconcile_foreign_tabs(&source, &mut rendered);
            assert_eq!(
                tabs_for_surface(&rendered, surface),
                1,
                "switching workspaces opened a second window onto the same surface",
            );

            // And back again, which is the sequence a person actually performs.
            rendered.desktop.active = source.desktop.active;
            reconcile_foreign_tabs(&source, &mut rendered);
            assert_eq!(
                tabs_for_surface(&rendered, surface),
                1,
                "returning to the window's own workspace duplicated it",
            );
        }

        /// A window the launcher placed in a workspace other than the active one
        /// is already adopted, must not be adopted again — the launch path, not the switch path: a launch records where asked, and the toplevel can map later.
        fn a_window_placed_outside_the_active_workspace_is_adopted_once() {
            let mut source = WorkspaceTree::bootstrap_demo();
            let mut rendered = WorkspaceTree::bootstrap_demo();
            let surface = 32;

            crate::shell::tab_binding::adopt_toplevel(
                &mut source,
                surface,
                Some("so.tilo.photos".into()),
            );
            assert!(reconcile_foreign_tabs(&source, &mut rendered));

            // Move the adopted tab into a workspace that isn't the active one, the way a recorded placement or a peer's room does.
            let elsewhere = rendered
                .desktop
                .workspaces
                .iter()
                .map(|w| w.id)
                .find(|id| *id != rendered.desktop.active)
                .expect("the demo tree has more than one workspace");
            let moved = take_foreign_tab(&mut rendered, surface).expect("the adopted tab");
            let ws = rendered
                .desktop
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

        a_still_live_closing_surface_is_not_readopted_or_confused_with_its_sibling();
        a_window_in_another_workspace_is_not_adopted_a_second_time();
        a_window_placed_outside_the_active_workspace_is_adopted_once();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn duplicate_foreign_entries_collapse_and_stay_collapsed() {
        /// A desktop that already has the duplicate (a session saved before the mirror compared whole trees) comes back with one window, not two.
        fn a_duplicate_already_in_the_tree_is_collapsed() {
            let mut source = WorkspaceTree::bootstrap_demo();
            let mut rendered = WorkspaceTree::bootstrap_demo();
            let surface = 33;

            crate::shell::tab_binding::adopt_toplevel(
                &mut source,
                surface,
                Some("so.tilo.photos".into()),
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
                .desktop
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
                .desktop
                .active_workspace()
                .and_then(|w| w.strips.first())
                .and_then(|s| s.window_groups.first())
                .expect("the group");
            assert!(
                group.tabs.iter().any(|t| t.id == group.active_tab),
                "the group is pointing at a tab that is no longer there",
            );
        }

        /// Duplicate cleanup is a document mutation, not a cosmetic `retain` on
        /// the current frame.  A later document update previously put the second
        /// app card back because it was never removed from the replicated list.
        fn duplicate_cleanup_replicates_and_does_not_reappear_after_resync() {
            let surface = 34;
            let mut source = WorkspaceTree::bootstrap_demo();
            crate::shell::tab_binding::adopt_toplevel(
                &mut source,
                surface,
                Some("so.tilo.photos".into()),
            );
            source.seed_doc_from_cache();
            let mut rendered = source.clone();

            let twin = foreign_tabs_anywhere(&rendered)
                .into_iter()
                .find(|tab| tab.surface_id == surface)
                .expect("the source foreign tab");
            let group = rendered
                .desktop
                .active_workspace_mut()
                .and_then(|workspace| workspace.strips.first_mut())
                .and_then(|strip| strip.window_groups.first_mut())
                .expect("the group the tab was adopted into");
            group.tabs.push(crate::shell::state::Tab {
                id: twin.tab_id + 2_000_000,
                title: twin.title,
                tile: ShellTile::Foreign {
                    app_id: twin.app_id,
                    surface_id: twin.surface_id,
                },
            });
            rendered.seed_doc_from_cache();
            let mut stale_replica = rendered.clone();
            let before_cleanup = rendered.current_heads();

            assert!(reconcile_foreign_tabs(&source, &mut rendered));
            assert_eq!(tabs_for_surface(&rendered, surface), 1);

            let deletion = rendered.export_changes_since(&before_cleanup);
            stale_replica
                .apply_changes(&deletion)
                .expect("the document deletion applies to a stale replica");
            assert_eq!(
                tabs_for_surface(&stale_replica, surface),
                1,
                "a later resync must not restore the duplicate application"
            );
        }

        /// A real session can receive a room revision while a local external app
        /// is mapping.  The revision may have been authored from before a close,
        /// so this covers the complete ordering rather than each operation in
        /// isolation: pointer/keyboard routes close and reorder the durable
        /// topology; the protocol side maps one foreign surface; then a stale
        /// peer revision and a normal restart run.
        fn stale_revision_after_close_reorder_and_foreign_launch_keeps_one_live_app() {
            use crate::shell::state::AppKind;
            use crate::shell::tab_binding::{Placement, open_hosted_app, remove_tab};

            let state_dir = tempfile::tempdir().expect("state directory");
            let state_path = state_dir.path().join("state.json");
            let mut source = WorkspaceTree::bootstrap_demo();
            let strip_id = source.desktop.workspaces[0].strips[0].id;

            // Build a three-window shared baseline. The app that is closed is a
            // symbolic hosted app, so its deletion is an actual room-document
            // mutation. The app launched later is deliberately a foreign
            // Wayland surface and therefore remains process-local.
            let closed_tab = open_hosted_app(
                &mut source,
                AppKind::Other("Close fixture".into()),
                "Close fixture",
                Placement::NewGroup,
            );
            let reorder_tab = open_hosted_app(
                &mut source,
                AppKind::Other("Reorder fixture".into()),
                "Reorder fixture",
                Placement::NewGroup,
            );
            let closed_group = group_for_tab(&source, closed_tab);
            let reordered_group = group_for_tab(&source, reorder_tab);
            source.seed_doc_from_cache();
            let base_heads = source.current_heads();
            let bootstrap = source.export_changes_since(&[]);

            let mut rendered = WorkspaceTree::default();
            rendered
                .apply_changes(&bootstrap)
                .expect("rendered tree accepts its shared baseline");
            rendered.persist_path = Some(state_path.clone());
            rendered.queue_save();

            // This is the durable close route used by both the close button and
            // Super+W after their event handlers resolve the exact tab.
            assert!(
                remove_tab(&mut rendered, closed_tab),
                "the closed app must leave the rendered document"
            );
            assert!(
                rendered.desktop.workspaces[0].strips[0]
                    .window_groups
                    .iter()
                    .all(|group| group.id != closed_group),
                "closing the only tab removes the whole group"
            );

            // Meanwhile a peer still based on the old document reorders another
            // group. Its change contains the now-closed group in its old list,
            // which is the regression that used to resurrect windows.
            assert!(source.reorder_group_for_tests(strip_id, reordered_group, 0));
            let stale_revision = source.export_changes_since(&base_heads);
            rendered
                .apply_changes(&stale_revision)
                .expect("a later peer revision applies");
            assert!(
                rendered.desktop.workspaces[0].strips[0]
                    .window_groups
                    .iter()
                    .all(|group| group.id != closed_group),
                "a stale reorder must not bring the closed group back"
            );

            // A conventional host application maps after that room revision.
            // It belongs to the protocol tree, then reconciliation makes exactly
            // one chrome tab for its surface.
            let surface = 0x51_000;
            crate::shell::tab_binding::adopt_toplevel(
                &mut source,
                surface,
                Some("org.example.ExternalEditor".into()),
            );
            assert!(reconcile_foreign_tabs(&source, &mut rendered));
            assert_eq!(
                tabs_for_surface(&rendered, surface),
                1,
                "one mapped external surface must produce exactly one tab"
            );
            assert!(
                !reconcile_foreign_tabs(&source, &mut rendered),
                "reconciliation is idempotent once the external app is present"
            );

            // A state round-trip may not revive the closed group; live Wayland
            // surfaces intentionally do not survive a compositor restart because
            // their surface ids belong to the old process.
            rendered.queue_save();
            let restarted = crate::shell::persist::load_tree_from(&state_path)
                .expect("the post-revision layout restarts cleanly");
            assert!(
                restarted.desktop.workspaces[0].strips[0]
                    .window_groups
                    .iter()
                    .all(|group| group.id != closed_group),
                "restart must not revive the closed group"
            );
            assert_eq!(
                tabs_for_surface(&restarted, surface),
                0,
                "a dead process-local surface must not be restored after restart"
            );
            assert_active_groups_are_valid(&restarted);
        }

        a_duplicate_already_in_the_tree_is_collapsed();
        duplicate_cleanup_replicates_and_does_not_reappear_after_resync();
        stale_revision_after_close_reorder_and_foreign_launch_keeps_one_live_app();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn foreign_layout_lifecycle_and_visibility_follow_the_tree() {
        /// Exercise the layout as one deterministic state machine rather than a
        /// collection of isolated mutations.  This is deliberately shaped like
        /// a busy desktop: hosted and external apps coexist, a legacy duplicate
        /// is discovered, tabs and groups move, focus crosses workspaces, an old
        /// peer revision lands, a newer revision follows it, and the compositor
        /// restarts after the external process exits.
        ///
        /// Pointer and keyboard close both resolve to `remove_tab` before this
        /// layer, so using that primitive here covers their shared durable
        /// transaction while `strip_navigation` checks both actual event routes.
        fn deterministic_layout_lifecycle_survives_interleaved_mutations() {
            use crate::shell::state::AppKind;
            use crate::shell::tab_binding::{
                Placement, close_tab_for_destroyed_surface, open_hosted_app, remove_tab,
            };

            crate::shell::launcher::clear_pending_placements();
            let state_dir = tempfile::tempdir().expect("state directory");
            let state_path = state_dir.path().join("state.json");
            let mut rendered = WorkspaceTree::bootstrap_demo();
            rendered.persist_path = Some(state_path.clone());

            let strip = rendered
                .desktop
                .active_workspace()
                .and_then(|workspace| workspace.strips.first())
                .expect("demo workspace strip")
                .id;
            let anchor = rendered
                .desktop
                .active_group_for(strip)
                .expect("demo workspace active group");
            let local_tab = open_hosted_app(
                &mut rendered,
                AppKind::Other("Native fixture".into()),
                "Native fixture",
                Placement::TabIn(anchor),
            );
            let reorder_tab = open_hosted_app(
                &mut rendered,
                AppKind::Other("Reorder fixture".into()),
                "Reorder fixture",
                Placement::NewGroup,
            );
            let reorder_group = group_for_tab(&rendered, reorder_tab);
            let close_tab = open_hosted_app(
                &mut rendered,
                AppKind::Other("Close fixture".into()),
                "Close fixture",
                Placement::NewGroup,
            );
            let close_group = group_for_tab(&rendered, close_tab);
            rendered.seed_doc_from_cache();

            // Protocol and chrome start from the same layout.  A pending launch
            // is queried independently by both trees in production; the same
            // surface id must get the same answer rather than minting two tabs.
            let mut source = rendered.clone();
            crate::shell::launcher::set_pending_placement(
                "org.example.ExternalEditor",
                Placement::TabIn(anchor),
            );
            let surface = 0x00C0_FFEE;
            crate::shell::tab_binding::adopt_toplevel(
                &mut source,
                surface,
                Some("org.example.ExternalEditor".into()),
            );
            assert!(reconcile_foreign_tabs(&source, &mut rendered));
            assert_eq!(tabs_for_surface(&rendered, surface), 1);
            assert!(
                !reconcile_foreign_tabs(&source, &mut rendered),
                "a second frame cannot duplicate one mapped toplevel"
            );

            // Model the bad on-disk state produced by an older build.  Make the
            // duplicate active as well, so cleanup has to repair selection, not
            // merely shorten a background vector.
            let original = foreign_tabs_anywhere(&rendered)
                .into_iter()
                .find(|tab| tab.surface_id == surface)
                .expect("mirrored foreign tab");
            let duplicate_id = original.tab_id.saturating_add(9_000_000);
            let owner = group_for_tab(&rendered, original.tab_id);
            let group = rendered
                .desktop
                .workspaces
                .iter_mut()
                .flat_map(|workspace| workspace.strips.iter_mut())
                .flat_map(|strip| strip.window_groups.iter_mut())
                .find(|group| group.id == owner)
                .expect("foreign tab owner");
            group.tabs.push(crate::shell::state::Tab {
                id: duplicate_id,
                title: original.title,
                tile: ShellTile::Foreign {
                    app_id: original.app_id,
                    surface_id: surface,
                },
            });
            group.active_tab = duplicate_id;
            rendered.seed_doc_from_cache();
            let stale_seed = rendered.export_changes_since(&[]);
            let mut stale_peer = WorkspaceTree::default();
            stale_peer
                .apply_changes(&stale_seed)
                .expect("stale peer accepts the shared baseline");
            let stale_base = stale_peer.current_heads();

            assert!(reconcile_foreign_tabs(&source, &mut rendered));
            assert_eq!(tabs_for_surface(&rendered, surface), 1);
            assert!(remove_tab(&mut rendered, close_tab));
            assert!(
                rendered
                    .desktop
                    .workspaces
                    .iter()
                    .flat_map(|workspace| workspace.strips.iter())
                    .flat_map(|strip| strip.window_groups.iter())
                    .all(|group| group.id != close_group),
                "closing a group's only tab removes the group immediately"
            );

            let moved_group = rendered
                .desktop
                .active_workspace()
                .and_then(|workspace| workspace.strips.first())
                .and_then(|strip| strip.window_groups.last())
                .expect("another group remains")
                .id;
            let _ = rendered.reorder_group_for_tests(strip, moved_group, 0);

            // The old peer only reorders an unrelated surviving group.  Its
            // change was authored while both the duplicate and closed group
            // still existed; neither may be revived when that revision arrives.
            assert!(stale_peer.reorder_group_for_tests(strip, reorder_group, 0));
            let stale_changes = stale_peer.export_changes_since(&stale_base);
            rendered
                .apply_changes(&stale_changes)
                .expect("old revision applies");
            assert_eq!(tabs_for_surface(&rendered, surface), 1);
            assert!(
                rendered
                    .desktop
                    .workspaces
                    .iter()
                    .flat_map(|workspace| workspace.strips.iter())
                    .flat_map(|strip| strip.window_groups.iter())
                    .all(|group| group.id != close_group),
                "an old reorder must not revive a closed window group"
            );

            // Travel away and back, then accept a genuinely newer tab-order
            // revision.  Focus is local state; neither activity can recreate a
            // tab that the document has already deleted.
            assert!(rendered.switch_down());
            assert!(rendered.switch_up());
            let future_seed = rendered.export_changes_since(&[]);
            let mut future_peer = WorkspaceTree::default();
            future_peer
                .apply_changes(&future_seed)
                .expect("future peer accepts the merged baseline");
            let future_base = future_peer.current_heads();
            let local_group = group_for_tab(&future_peer, local_tab);
            let group = future_peer
                .desktop
                .workspaces
                .iter()
                .flat_map(|workspace| workspace.strips.iter())
                .flat_map(|strip| strip.window_groups.iter())
                .find(|group| group.id == local_group)
                .expect("local tab group");
            let from = group
                .tabs
                .iter()
                .position(|tab| tab.id == local_tab)
                .expect("local tab index");
            let to = usize::from(from == 0);
            assert!(future_peer.reorder_tab_for_tests(local_group, from, to));
            let future_changes = future_peer.export_changes_since(&future_base);
            rendered
                .apply_changes(&future_changes)
                .expect("newer revision applies");
            assert_eq!(tabs_for_surface(&rendered, surface), 1);
            assert_active_groups_are_valid(&rendered);

            // A clean external exit is a protocol teardown, not a layout reset.
            assert!(close_tab_for_destroyed_surface(&mut source, surface));
            assert!(reconcile_foreign_tabs(&source, &mut rendered));
            assert_eq!(tabs_for_surface(&rendered, surface), 0);
            assert!(
                rendered
                    .desktop
                    .workspaces
                    .iter()
                    .flat_map(|workspace| workspace.strips.iter())
                    .flat_map(|strip| strip.window_groups.iter())
                    .flat_map(|group| group.tabs.iter())
                    .any(|tab| tab.id == local_tab),
                "closing an external client must not remove the hosted sibling"
            );

            rendered.queue_save();
            let restarted = crate::shell::persist::load_tree_from(&state_path)
                .expect("interleaved layout remains restartable");
            assert_eq!(tabs_for_surface(&restarted, surface), 0);
            assert!(
                restarted
                    .desktop
                    .workspaces
                    .iter()
                    .flat_map(|workspace| workspace.strips.iter())
                    .flat_map(|strip| strip.window_groups.iter())
                    .all(|group| group.id != close_group),
                "restart must retain the close after both old and new revisions"
            );
            assert_active_groups_are_valid(&restarted);
            crate::shell::launcher::clear_pending_placements();
        }

        fn visibility_tracks_the_groups_active_tab() {
            use crate::shell::tab_binding::{Placement, adopt_toplevel, adopt_toplevel_at};
            crate::shell::launcher::clear_pending_placements();
            let mut tree = WorkspaceTree::bootstrap_demo();
            // A lone selected window, so each adoption opens its own column rather than joining a tabbed group — the arrangement this test is about.
            {
                let strip_id = tree.desktop.workspaces[0].strips[0].id;
                let g = &mut tree.desktop.workspaces[0].strips[0].window_groups[0];
                g.tabs.truncate(1);
                g.active_tab = g.tabs[0].id;
                let gid = g.id;
                tree.desktop
                    .active_strip
                    .insert(tree.desktop.active, strip_id);
                tree.desktop.set_active_group(strip_id, gid);
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
                .desktop
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

        deterministic_layout_lifecycle_survives_interleaved_mutations();
        visibility_tracks_the_groups_active_tab();
    }

    /// How many tabs anywhere in `tree` show `surface` — walks the tree the chrome renders from, so a duplicate is counted where it'd draw.
    fn tabs_for_surface(tree: &WorkspaceTree, surface: SurfaceId) -> usize {
        tree.desktop.workspaces
            .iter()
            .flat_map(|w| w.strips.iter())
            .flat_map(|s| s.window_groups.iter())
            .flat_map(|g| g.tabs.iter())
            .filter(|t| {
                matches!(&t.tile, ShellTile::Foreign { surface_id, .. } if *surface_id == surface)
            })
            .count()
    }

    fn group_for_tab(tree: &WorkspaceTree, tab_id: u64) -> u64 {
        tree.desktop
            .workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .find(|group| group.tabs.iter().any(|tab| tab.id == tab_id))
            .map(|group| group.id)
            .expect("the fixture tab belongs to one group")
    }

    fn assert_active_groups_are_valid(tree: &WorkspaceTree) {
        for (workspace, strip_id) in &tree.desktop.active_strip {
            let Some(strip) = tree
                .desktop
                .workspaces
                .iter()
                .find(|candidate| candidate.id == *workspace)
                .and_then(|workspace| workspace.strips.iter().find(|strip| strip.id == *strip_id))
            else {
                panic!("active workspace points to a missing strip");
            };
            if let Some(group_id) = tree.desktop.active_group_for(*strip_id) {
                assert!(
                    strip.window_groups.iter().any(|group| group.id == group_id),
                    "active strip points to a deleted group"
                );
            }
        }
    }

    /// Detach the tab showing `surface` from wherever it is, so a test can put it somewhere else.
    fn take_foreign_tab(
        tree: &mut WorkspaceTree,
        surface: SurfaceId,
    ) -> Option<crate::shell::state::Tab> {
        for ws in &mut tree.desktop.workspaces {
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

    /// Wait until `barrier` is signalled, failing past the hang bound.
    #[cfg(target_os = "linux")]
    fn await_release(barrier: &Barrier, what: &str) {
        let started = Instant::now();
        while !barrier.is_signaled() {
            assert!(started.elapsed() < tilo_test_env::hang_bound(), "{what}");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_buffer_still_being_rendered_holds_only_its_own_commit() {
        use smithay::backend::allocator::dmabuf::DmabufFlags;
        use smithay::reexports::wayland_server::backend::ClientData;
        use std::os::fd::{FromRawFd as _, OwnedFd};

        struct NoClientData;
        impl ClientData for NoClientData {}

        let display = smithay::reexports::wayland_server::Display::<()>::new().expect("display");
        let (server_end, _client_end) =
            std::os::unix::net::UnixStream::pair().expect("socket pair");
        let client = display
            .handle()
            .insert_client(server_end, Arc::new(NoClientData))
            .expect("client");
        let buffer = |fd: OwnedFd| {
            let mut builder = Dmabuf::builder(
                (1, 1),
                Fourcc::Argb8888,
                Modifier::Linear,
                DmabufFlags::empty(),
            );
            assert!(builder.add_plane(fd, 0, 0, 4));
            builder.build().expect("dmabuf")
        };
        // An empty pipe polls unreadable, as a buffer whose write fence has
        // not signalled; a memfd polls readable, as a rendered buffer.
        let mut ends = [0; 2];
        // SAFETY: `ends` has room for the two descriptors pipe2 writes.
        assert_eq!(
            unsafe { libc::pipe2(ends.as_mut_ptr(), libc::O_CLOEXEC) },
            0
        );
        // SAFETY: pipe2 returned two new descriptors nothing else owns.
        let (rendering, _writer) =
            unsafe { (OwnedFd::from_raw_fd(ends[0]), OwnedFd::from_raw_fd(ends[1])) };
        // SAFETY: a static nul-terminated name and MFD_CLOEXEC.
        let raw = unsafe { libc::memfd_create(c"foreign-fence-test".as_ptr(), libc::MFD_CLOEXEC) };
        assert!(
            raw >= 0,
            "memfd_create: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: memfd_create returned a new descriptor nothing else owns.
        let rendered = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(raw) });
        rendered.set_len(4).expect("size memfd");

        let (slow, fast) = (sid(200), sid(201));
        // Phase `a_rendering_buffer_waits_for_its_fence_on_the_loop`.
        let held = store()
            .prepare_dmabuf_capture(slow, client.clone(), buffer(rendering), None)
            .expect("slow capture");
        assert!(
            held.fence.is_some(),
            "an unsignalled buffer is handed to the loop to watch"
        );
        // Phase `another_surface_is_captured_meanwhile`.
        let ready = store()
            .prepare_dmabuf_capture(fast, client, buffer(rendered.into()), None)
            .expect("fast capture");
        assert!(
            ready.fence.is_none(),
            "a rendered buffer is captured at once"
        );
        await_release(
            &ready.barrier,
            "a surface's commit waited behind another surface's unsignalled fence",
        );
        assert!(
            !held.barrier.is_signaled(),
            "nothing captured the slow buffer before its fence signalled"
        );
        // Phase `the_signalled_fence_starts_the_capture`.
        dmabuf_fence_signalled(slow, held.token.wrapping_add(1));
        assert!(
            !held.barrier.is_signaled(),
            "a fence of another commit does not start this one"
        );
        dmabuf_fence_signalled(slow, held.token);
        await_release(
            &held.barrier,
            "the signalled buffer's commit was never released",
        );
        store().forget(slow);
        store().forget(fast);
    }
}
