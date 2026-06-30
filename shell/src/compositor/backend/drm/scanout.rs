//! DRM scanout — modeset a connector and present GPU-rendered frames.
//!
//! [`runtime`](super::runtime) acquires the seat, enumerates connectors,
//! and wires input; this module keeps the primary card open as DRM
//! master, sets a mode on a connected connector, and presents each
//! frame.
//!
//! # What changed (Track B1 — GPU-composited scanout + hardware cursor)
//!
//! The original skeleton used a single CPU dumb buffer, drew a *software*
//! cursor into it, and flushed with `DRM_IOCTL_MODE_DIRTYFB`. That made
//! every cursor move cost a full-frame re-upload and put the cursor in
//! the wrong place (offset/flip). Two changes replace it:
//!
//! 1. **Hardware cursor plane.** The cursor bitmap is uploaded once into
//!    a 64×64 ARGB8888 buffer and shown via the KMS cursor ioctls
//!    (`set_cursor2` + `move_cursor`). Pointer motion repositions the
//!    plane with a tiny `move_cursor` ioctl — no framebuffer touch. The
//!    software-cursor compositing is kept only as a fallback for drivers
//!    that reject the cursor ioctls (`hw_cursor_active == false`).
//!
//! 2. **Double-buffered page flip.** Two dumb buffers ping-pong; each
//!    frame the renderer swizzles into the back buffer and we
//!    `page_flip` to it (legacy flip with a completion event we drain).
//!    This replaces `dirty_framebuffer` with the correct presentation
//!    primitive (tear-free, vsync-throttled). If `page_flip` is
//!    unsupported on the driver we latch back to the proven
//!    dirty_framebuffer / set_crtc path so the boot never regresses.
//!
//! # Why not zero-copy DMA-BUF (yet)
//!
//! The renderer is **vello on wgpu** (Vulkan). True zero-copy scanout
//! would export the wgpu Vulkan image as a DMA-BUF and import it as a
//! DRM framebuffer. smithay's `DrmCompositor` / `GbmBufferedSurface`
//! can't help here: they drive a GLES `Renderer`, not vello, so using
//! them would mean abandoning the vello scene. The wgpu→DMA-BUF export
//! path (`Device::as_hal` → `vulkan::Device` → externally-allocated
//! `VkImage` → `VK_EXT_external_memory_dma_buf` fd → `gbm`/`drm`
//! framebuffer with negotiated format modifiers) is a large, fragile
//! build that also can't be validated in the current VM (virtio-gpu +
//! llvmpipe is CPU rendering — there is no real GPU buffer to scan out).
//! So this lands the achievable win — fused swizzle (one copy removed in
//! [`super::paint`]) + page flip + hardware cursor — and documents the
//! DMA-BUF path as the remaining follow-up.
//!
//! Pixel byte order: vello renders `Rgba8Unorm`; the scanout framebuffer
//! is `Xrgb8888` (little-endian `[B, G, R, X]`). The swizzle now happens
//! inside [`super::paint::VelloDrmBridge::paint_into_xrgb8888`], writing
//! straight into the dumb-buffer mmap.

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::Path;

use smithay::backend::session::Session;
use smithay::reexports::drm as drm_re;
use smithay::reexports::rustix::fs::OFlags;

use drm_re::buffer::{Buffer as _, DrmFourcc};
use drm_re::control::dumbbuffer::DumbBuffer;
use drm_re::control::{
    connector, crtc, framebuffer, ClipRect, Device as ControlDevice, Event, Mode, ModeTypeFlags,
    PageFlipFlags,
};
use drm_re::Device as BasicDevice;

/// Cursor plane dimensions. 64×64 is the universally-supported legacy
/// cursor size (virtio-gpu, i915, amdgpu all advertise it); querying
/// `DriverCapability::CursorWidth/Height` is a refinement left for HW
/// bring-up.
const CURSOR_DIM: u32 = 64;

/// Minimal owned-fd wrapper implementing the `drm` crate's device traits.
/// The fd comes from libseat (`Session::open`) so it is privileged +
/// becomes DRM master while the session is active.
struct Card(OwnedFd);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}
impl BasicDevice for Card {}
impl ControlDevice for Card {}

/// Lightweight handle that repositions the hardware cursor plane without
/// touching the framebuffer. Holds a *dup* of the card fd (which shares
/// the open file description, so it stays DRM master) plus the crtc the
/// cursor lives on. Stored in the runtime's dispatch state so the
/// libinput motion handler can move the cursor at input rate, decoupled
/// from the paint/flip cadence.
pub struct HwCursor {
    card: Card,
    crtc: crtc::Handle,
}

impl HwCursor {
    /// Move the cursor so its top-left (the hot-spot of our arrow) sits
    /// at `(x, y)` in output pixels. A failing ioctl is logged-and-
    /// ignored — a missed cursor move must never crash the input path.
    pub fn move_to(&self, x: i32, y: i32) {
        #[allow(deprecated)]
        if let Err(e) = self.card.move_cursor(self.crtc, (x, y)) {
            tracing::trace!("DRM scanout: move_cursor failed: {e}");
        }
    }
}

/// Holds the primary card open as master and owns the scanout buffers.
pub struct DrmScanout {
    card: Card,
    crtc: crtc::Handle,
    connector: connector::Handle,
    mode: Mode,
    /// Double-buffered scanout targets. `front` is the one currently
    /// being displayed (or the target of the in-flight flip); the
    /// renderer draws into `1 - front`.
    buffers: [DumbBuffer; 2],
    fbs: [framebuffer::Handle; 2],
    front: usize,
    /// True between requesting a page flip and draining its completion
    /// event. Guards against queuing a second flip before the first
    /// retires (which the kernel rejects with `EBUSY`).
    flip_pending: bool,
    /// Whether the page-flip path is in use. Latched to `false` if a
    /// flip fails with a "not supported" errno, after which we fall
    /// back to the dirty_framebuffer / set_crtc flush.
    use_pageflip: bool,
    width: u32,
    height: u32,
    /// Set once `dirty_framebuffer` is known to be unsupported, so we
    /// stop retrying it and use `set_crtc` to force a flush instead.
    dirtyfb_unsupported: bool,
    /// Cursor bitmap buffer; kept alive so its GEM handle stays valid
    /// for `set_cursor2`. `None` when the hardware cursor isn't active.
    _cursor_buffer: Option<DumbBuffer>,
    /// True when the KMS cursor plane is driving the pointer; when
    /// false we composite the software cursor into the framebuffer.
    hw_cursor_active: bool,
}

impl DrmScanout {
    /// Open `card_path` via the session, pick a connected connector +
    /// mode + crtc, allocate two dumb framebuffers, modeset, and set up
    /// the hardware cursor plane. Returns `Err` (never panics) so the
    /// caller can run the compositor headless if scanout can't be set up.
    pub fn new<S: Session>(session: &mut S, card_path: &Path) -> Result<Self, String> {
        use smithay::backend::session::AsErrno;
        let fd = session
            .open(
                card_path,
                OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
            )
            .map_err(|e| format!("session.open({card_path:?}): errno={:?}", e.as_errno()))?;
        let card = Card(fd);
        // Become DRM master so set_crtc is permitted. libseat normally
        // grants it on the active session; acquire explicitly and log.
        match card.acquire_master_lock() {
            Ok(()) => tracing::info!("DRM scanout: acquired DRM master"),
            Err(e) => {
                tracing::warn!("DRM scanout: acquire_master_lock failed ({e}); set_crtc may EACCES")
            }
        }

        let res = card
            .resource_handles()
            .map_err(|e| format!("resource_handles: {e}"))?;

        // First connected connector that advertises a mode.
        let mut chosen: Option<(connector::Info, Mode)> = None;
        for &ch in res.connectors() {
            if let Ok(info) = card.get_connector(ch, true) {
                if info.state() == connector::State::Connected {
                    if let Some(m) = pick_mode(info.modes()) {
                        chosen = Some((info, m));
                        break;
                    }
                }
            }
        }
        let (conn_info, mode) = chosen.ok_or("no connected connector with a usable mode")?;
        let connector = conn_info.handle();

        // Prefer the connector's current encoder/crtc; fall back to the
        // first crtc (single-GPU/VM case).
        let crtc = conn_info
            .current_encoder()
            .and_then(|e| card.get_encoder(e).ok())
            .and_then(|e| e.crtc())
            .or_else(|| res.crtcs().first().copied())
            .ok_or("no crtc available for connector")?;

        let (mw, mh) = mode.size();
        let (width, height) = (mw as u32, mh as u32);

        // Two ping-pong dumb buffers, both cleared to black so the first
        // frames before paint aren't garbage.
        let mut buf0 = card
            .create_dumb_buffer((width, height), DrmFourcc::Xrgb8888, 32)
            .map_err(|e| format!("create_dumb_buffer[0]: {e}"))?;
        let mut buf1 = card
            .create_dumb_buffer((width, height), DrmFourcc::Xrgb8888, 32)
            .map_err(|e| format!("create_dumb_buffer[1]: {e}"))?;
        for b in [&mut buf0, &mut buf1] {
            if let Ok(mut map) = card.map_dumb_buffer(b) {
                for byte in map.as_mut() {
                    *byte = 0;
                }
            }
        }
        let fb0 = card
            .add_framebuffer(&buf0, 24, 32)
            .map_err(|e| format!("add_framebuffer[0]: {e}"))?;
        let fb1 = card
            .add_framebuffer(&buf1, 24, 32)
            .map_err(|e| format!("add_framebuffer[1]: {e}"))?;

        // Modeset onto the first buffer.
        card.set_crtc(crtc, Some(fb0), (0, 0), &[connector], Some(mode))
            .map_err(|e| format!("set_crtc (modeset): {e}"))?;

        tracing::info!(width, height, "DRM scanout: modeset complete — display is live");

        // Hardware cursor. Best-effort: a failure leaves
        // `hw_cursor_active = false` and the software cursor takes over.
        let (cursor_buffer, hw_cursor_active) = match Self::setup_hw_cursor(&card, crtc) {
            Ok(buf) => {
                // Centre it so a fresh boot shows the pointer before any
                // input arrives.
                #[allow(deprecated)]
                let _ = card.move_cursor(crtc, ((width / 2) as i32, (height / 2) as i32));
                tracing::info!("DRM scanout: hardware cursor plane active");
                (Some(buf), true)
            }
            Err(e) => {
                tracing::warn!("DRM scanout: hardware cursor unavailable ({e}); using software cursor");
                (None, false)
            }
        };

        Ok(Self {
            card,
            crtc,
            connector,
            mode,
            buffers: [buf0, buf1],
            fbs: [fb0, fb1],
            front: 0,
            flip_pending: false,
            use_pageflip: true,
            width,
            height,
            dirtyfb_unsupported: false,
            _cursor_buffer: cursor_buffer,
            hw_cursor_active,
        })
    }

    /// Allocate the 64×64 ARGB cursor buffer, paint the arrow into it,
    /// and bind it to the crtc's cursor plane via `set_cursor2` (hotspot
    /// at the top-left tip, matching the bitmap). On success returns the
    /// buffer (which the caller must keep alive).
    fn setup_hw_cursor(card: &Card, crtc: crtc::Handle) -> Result<DumbBuffer, String> {
        let mut cbuf = card
            .create_dumb_buffer((CURSOR_DIM, CURSOR_DIM), DrmFourcc::Argb8888, 32)
            .map_err(|e| format!("create cursor dumb buffer: {e}"))?;
        let pitch = cbuf.pitch() as usize;
        {
            let mut map = card
                .map_dumb_buffer(&mut cbuf)
                .map_err(|e| format!("map cursor buffer: {e}"))?;
            paint_cursor_argb(map.as_mut(), pitch);
        }
        // Hotspot (0,0): the arrow's tip is the buffer's top-left pixel,
        // so the plane position == the pointer position. Virtual GPUs
        // (virtio-gpu/QXL) use the hotspot to place the host pointer.
        #[allow(deprecated)]
        card.set_cursor2(crtc, Some(&cbuf), (0, 0))
            .map_err(|e| format!("set_cursor2: {e}"))?;
        Ok(cbuf)
    }

    /// Mode dimensions — callers size their paint buffer to this.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Build a [`HwCursor`] mover for the runtime's input handler, or
    /// `None` if the hardware cursor isn't active (the software-cursor
    /// fallback is driven from `present_into` instead). The mover holds
    /// a dup of the card fd so it can issue `move_cursor` ioctls
    /// independently of the paint path.
    pub fn cursor_mover(&self) -> Option<HwCursor> {
        if !self.hw_cursor_active {
            return None;
        }
        match self.card.0.try_clone() {
            Ok(fd) => Some(HwCursor {
                card: Card(fd),
                crtc: self.crtc,
            }),
            Err(e) => {
                tracing::warn!("DRM scanout: cursor fd dup failed ({e}); cursor moves via paint path");
                None
            }
        }
    }

    /// Present one frame. `fill` writes the frame into the back buffer in
    /// XRGB8888 (`[B, G, R, X]`), given the mapped destination slice and
    /// its row pitch — typically
    /// [`VelloDrmBridge::paint_into_xrgb8888`](super::paint::VelloDrmBridge::paint_into_xrgb8888).
    ///
    /// `cursor` is the pointer position; it is only consulted when the
    /// hardware cursor is **unavailable** (software-cursor fallback).
    ///
    /// Logged-and-skipped on any error so a bad frame never crashes the
    /// compositor.
    pub fn present_into<F>(&mut self, cursor: Option<(f64, f64)>, fill: F)
    where
        F: FnOnce(&mut [u8], usize),
    {
        // Drain any retired page-flip completion so we can flip again.
        self.drain_flip_events();

        if self.use_pageflip && self.flip_pending {
            // Previous flip not yet retired — drop this frame (natural
            // vsync throttle). The scene we would have shown is simply
            // superseded by the next one.
            return;
        }

        // Pick the render target: the back buffer when flipping, else the
        // single live buffer in the dirty_framebuffer fallback.
        let idx = if self.use_pageflip {
            1 - self.front
        } else {
            self.front
        };

        let pitch = self.buffers[idx].pitch() as usize;
        let (dw, dh) = (self.width as usize, self.height as usize);
        {
            let mut map = match self.card.map_dumb_buffer(&mut self.buffers[idx]) {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!("DRM scanout: map_dumb_buffer: {e}");
                    return;
                }
            };
            let dst = map.as_mut();
            fill(dst, pitch);
            // Software-cursor fallback only — the hardware cursor plane
            // composites itself and must not be drawn into the frame.
            if !self.hw_cursor_active {
                if let Some((px, py)) = cursor {
                    draw_cursor(dst, pitch, dw, dh, px as i32, py as i32);
                }
            }
        } // unmap here

        if self.use_pageflip {
            // Best-effort CPU→host transfer before flipping. On virtual
            // GPUs (virtio-gpu — the VM bring-up target) `dirty_framebuffer`
            // is what flushes a CPU dumb buffer's pixels into the host
            // resource; a bare page flip can otherwise present bytes the
            // host never received. On real GPUs this is a no-op or
            // unsupported, so the error is ignored — the flip alone
            // presents correctly there.
            let clip = ClipRect::new(0, 0, self.width as u16, self.height as u16);
            let _ = self.card.dirty_framebuffer(self.fbs[idx], &[clip]);
            self.flip_to(idx);
        } else {
            self.flush_dirty(idx);
        }
    }

    /// Queue a page flip to buffer `idx`. On success marks the flip
    /// pending and makes `idx` the new front. On a transient `EBUSY`/
    /// `EAGAIN` the frame is skipped (retry next call). On a
    /// "not supported" errno we latch off the page-flip path, force the
    /// rendered buffer onscreen with `set_crtc`, and fall back to
    /// dirty_framebuffer from then on.
    fn flip_to(&mut self, idx: usize) {
        match self
            .card
            .page_flip(self.crtc, self.fbs[idx], PageFlipFlags::EVENT, None)
        {
            Ok(()) => {
                self.flip_pending = true;
                self.front = idx;
            }
            Err(e) => {
                let code = e.raw_os_error().unwrap_or(0);
                if code == libc::EBUSY || code == libc::EAGAIN {
                    tracing::trace!("DRM scanout: page_flip busy; skipping frame");
                } else {
                    tracing::warn!(
                        "DRM scanout: page_flip unsupported (errno {code}: {e}); \
                         falling back to dirty_framebuffer"
                    );
                    self.use_pageflip = false;
                    let _ = self.card.set_crtc(
                        self.crtc,
                        Some(self.fbs[idx]),
                        (0, 0),
                        &[self.connector],
                        Some(self.mode),
                    );
                    self.front = idx;
                }
            }
        }
    }

    /// Flush the live buffer with `DRM_IOCTL_MODE_DIRTYFB` (how
    /// virtio-gpu transfers a CPU buffer to the host). Falls back to a
    /// forced `set_crtc` if the driver lacks dirtyfb support.
    fn flush_dirty(&mut self, idx: usize) {
        if self.dirtyfb_unsupported {
            self.force_flush_via_set_crtc();
            return;
        }
        let clip = ClipRect::new(0, 0, self.width as u16, self.height as u16);
        if let Err(e) = self.card.dirty_framebuffer(self.fbs[idx], &[clip]) {
            tracing::warn!("DRM scanout: dirty_framebuffer unsupported ({e}); using set_crtc");
            self.dirtyfb_unsupported = true;
            self.force_flush_via_set_crtc();
        }
    }

    /// Non-blocking drain of DRM events. Clears `flip_pending` when a
    /// page-flip completion arrives. The card fd is `O_NONBLOCK`, so the
    /// read returns `EAGAIN` (an `Err`) when nothing is queued — that's
    /// the no-events case, not a failure.
    fn drain_flip_events(&mut self) {
        if let Ok(events) = self.card.receive_events() {
            for ev in events {
                if let Event::PageFlip(_) = ev {
                    self.flip_pending = false;
                }
            }
        }
    }

    fn force_flush_via_set_crtc(&self) {
        let _ = self.card.set_crtc(
            self.crtc,
            Some(self.fbs[self.front]),
            (0, 0),
            &[self.connector],
            Some(self.mode),
        );
    }
}

/// Choose a scanout mode. The kernel lists the EDID-preferred mode first
/// (qemu's `xres`/`yres` set it), so we trust the first *reasonable* mode
/// (>= 1024 wide). virtio-gpu sometimes flags a tiny default (640x480) as
/// PREFERRED and advertises huge modes (up to 5120x2160), so we don't
/// trust the flag and we cap the software-render fallback at 1920x1080.
fn pick_mode(modes: &[Mode]) -> Option<Mode> {
    for m in modes {
        let (w, h) = m.size();
        tracing::debug!(
            w,
            h,
            preferred = m.mode_type().contains(ModeTypeFlags::PREFERRED),
            "DRM scanout: candidate mode"
        );
    }
    let first_reasonable = modes.first().filter(|m| m.size().0 >= 1024);
    first_reasonable
        .or_else(|| {
            modes
                .iter()
                .filter(|m| {
                    let (w, h) = m.size();
                    w <= 1920 && h <= 1080
                })
                .max_by_key(|m| {
                    let (w, h) = m.size();
                    (w as u32) * (h as u32)
                })
        })
        .or_else(|| modes.first())
        .copied()
}

/// Classic left-pointing arrow cursor. `'#'` = black outline, `'.'` =
/// white fill, space = transparent. The hot-spot is the top-left tip.
const CURSOR: [&str; 18] = [
    "#                ",
    "##               ",
    "#.#              ",
    "#..#             ",
    "#...#            ",
    "#....#           ",
    "#.....#          ",
    "#......#         ",
    "#.......#        ",
    "#........#       ",
    "#.........#      ",
    "#......#####      ",
    "#...#..#         ",
    "#..# #..#        ",
    "#.#  #..#        ",
    "##    #..#       ",
    "#     #..#       ",
    "       ##        ",
];

/// Paint the arrow into a 64×64 ARGB8888 cursor buffer (`[B, G, R, A]`,
/// little-endian). Transparent everywhere the bitmap is blank. `pitch`
/// is the buffer's row stride in bytes.
fn paint_cursor_argb(dst: &mut [u8], pitch: usize) {
    for b in dst.iter_mut() {
        *b = 0; // fully transparent
    }
    for (row, line) in CURSOR.iter().enumerate() {
        for (col, ch) in line.bytes().enumerate() {
            let (r, g, b) = match ch {
                b'#' => (0u8, 0u8, 0u8),
                b'.' => (0xFFu8, 0xFFu8, 0xFFu8),
                _ => continue,
            };
            let o = row * pitch + col * 4;
            if o + 4 <= dst.len() {
                dst[o] = b;
                dst[o + 1] = g;
                dst[o + 2] = r;
                dst[o + 3] = 0xFF;
            }
        }
    }
}

/// Composite the software cursor into an XRGB8888 buffer at `(px, py)`
/// (the hot-spot). Bytes are little-endian XRGB: `[B, G, R, X]`. Only
/// used when the hardware cursor plane is unavailable.
fn draw_cursor(dst: &mut [u8], pitch: usize, dw: usize, dh: usize, px: i32, py: i32) {
    for (row, line) in CURSOR.iter().enumerate() {
        let y = py + row as i32;
        if y < 0 || y as usize >= dh {
            continue;
        }
        for (col, ch) in line.bytes().enumerate() {
            let (r, g, b) = match ch {
                b'#' => (0u8, 0u8, 0u8),
                b'.' => (0xFFu8, 0xFFu8, 0xFFu8),
                _ => continue,
            };
            let x = px + col as i32;
            if x < 0 || x as usize >= dw {
                continue;
            }
            let o = y as usize * pitch + x as usize * 4;
            if o + 4 <= dst.len() {
                dst[o] = b;
                dst[o + 1] = g;
                dst[o + 2] = r;
                dst[o + 3] = 0xFF;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_bitmap_writes_opaque_and_transparent_pixels() {
        // 64×64 ARGB, tight pitch.
        let pitch = (CURSOR_DIM * 4) as usize;
        let mut buf = vec![0xCDu8; pitch * CURSOR_DIM as usize];
        paint_cursor_argb(&mut buf, pitch);

        // Top-left pixel is the arrow tip ('#'): opaque black [B,G,R,A].
        assert_eq!(&buf[0..4], &[0, 0, 0, 0xFF], "tip should be opaque black");

        // A blank area (far bottom-right) stays fully transparent.
        let o = 60 * pitch + 60 * 4;
        assert_eq!(&buf[o..o + 4], &[0, 0, 0, 0], "blank cursor area is transparent");

        // There is at least one white fill pixel somewhere.
        let any_white = buf
            .chunks_exact(4)
            .any(|p| p[0] == 0xFF && p[1] == 0xFF && p[2] == 0xFF && p[3] == 0xFF);
        assert!(any_white, "cursor must contain white fill pixels");
    }

    #[test]
    fn software_cursor_draws_into_xrgb_buffer() {
        // 32×32 XRGB destination, clamp-safe.
        let (w, h) = (32usize, 32usize);
        let pitch = w * 4;
        let mut dst = vec![0u8; pitch * h];
        draw_cursor(&mut dst, pitch, w, h, 4, 4);
        // The tip pixel at (4,4) is black (outline) and opaque.
        let o = 4 * pitch + 4 * 4;
        assert_eq!(&dst[o..o + 4], &[0, 0, 0, 0xFF]);
        // A pixel far from the cursor is untouched (still zero).
        assert_eq!(&dst[0..4], &[0, 0, 0, 0]);
    }

    #[test]
    fn software_cursor_clamps_at_edges_without_panic() {
        // Drawing partially off the right/bottom edge must not panic.
        let (w, h) = (20usize, 20usize);
        let pitch = w * 4;
        let mut dst = vec![0u8; pitch * h];
        draw_cursor(&mut dst, pitch, w, h, 18, 18); // most of it off-screen
        draw_cursor(&mut dst, pitch, w, h, -3, -3); // off the top-left
    }
}
