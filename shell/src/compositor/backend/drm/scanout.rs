//! DRM scanout — modeset a connector and present CPU RGBA frames.
//!
//! This is the "pixel-on-screen" half the Phase 5/5.2 skeleton left as a
//! follow-up. [`runtime`](super::runtime) acquires the seat, enumerates
//! connectors, and wires input; this module keeps the primary card open as
//! DRM master, sets a mode on a connected connector, and blits each frame's
//! CPU RGBA bytes (from [`super::paint::VelloDrmBridge`]) into a dumb
//! framebuffer that is scanned out.
//!
//! It uses a single CPU dumb buffer + `DRM_IOCTL_MODE_DIRTYFB` (which is how
//! virtio-gpu flushes a CPU buffer to the host). This is the simplest path
//! to a visible desktop; double-buffered page-flip and zero-copy DMA-BUF are
//! the perf follow-ups. RGBA (vello, `Rgba8Unorm`) is converted to the
//! framebuffer's XRGB8888 (little-endian `BB GG RR XX`) per pixel.

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::Path;

use smithay::backend::session::Session;
use smithay::reexports::drm as drm_re;
use smithay::reexports::rustix::fs::OFlags;

use drm_re::buffer::{Buffer as _, DrmFourcc};
use drm_re::control::dumbbuffer::DumbBuffer;
use drm_re::control::{
    connector, crtc, framebuffer, ClipRect, Device as ControlDevice, Mode, ModeTypeFlags,
};
use drm_re::Device as BasicDevice;

/// Minimal owned-fd wrapper implementing the `drm` crate's device traits.
/// The fd comes from libseat (`Session::open`) so it is privileged + becomes
/// DRM master while the session is active.
struct Card(OwnedFd);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}
impl BasicDevice for Card {}
impl ControlDevice for Card {}

/// Holds the primary card open as master and owns the scanout framebuffer.
pub struct DrmScanout {
    card: Card,
    crtc: crtc::Handle,
    connector: connector::Handle,
    mode: Mode,
    fb: framebuffer::Handle,
    buffer: DumbBuffer,
    width: u32,
    height: u32,
    /// Set once `dirty_framebuffer` is known to be unsupported, so we stop
    /// retrying it and use `set_crtc` to force a flush instead.
    dirtyfb_unsupported: bool,
}

impl DrmScanout {
    /// Open `card_path` via the session, pick a connected connector + mode +
    /// crtc, allocate a dumb framebuffer, and modeset. Returns `Err` (never
    /// panics) so the caller can run the compositor headless if scanout
    /// can't be set up.
    pub fn new<S: Session>(session: &mut S, card_path: &Path) -> Result<Self, String> {
        use smithay::backend::session::AsErrno;
        let fd = session
            .open(
                card_path,
                OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
            )
            .map_err(|e| format!("session.open({card_path:?}): errno={:?}", e.as_errno()))?;
        let card = Card(fd);
        // Become DRM master so set_crtc is permitted. libseat normally grants
        // it on the active session; acquire explicitly and log the outcome.
        match card.acquire_master_lock() {
            Ok(()) => tracing::info!("DRM scanout: acquired DRM master"),
            Err(e) => tracing::warn!("DRM scanout: acquire_master_lock failed ({e}); set_crtc may EACCES"),
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

        let mut buffer = card
            .create_dumb_buffer((width, height), DrmFourcc::Xrgb8888, 32)
            .map_err(|e| format!("create_dumb_buffer: {e}"))?;
        // Clear to black so the first frame before paint isn't garbage.
        if let Ok(mut map) = card.map_dumb_buffer(&mut buffer) {
            for b in map.as_mut() {
                *b = 0;
            }
        }
        let fb = card
            .add_framebuffer(&buffer, 24, 32)
            .map_err(|e| format!("add_framebuffer: {e}"))?;
        card.set_crtc(crtc, Some(fb), (0, 0), &[connector], Some(mode))
            .map_err(|e| format!("set_crtc (modeset): {e}"))?;

        tracing::info!(width, height, "DRM scanout: modeset complete — display is live");

        Ok(Self {
            card,
            crtc,
            connector,
            mode,
            fb,
            buffer,
            width,
            height,
            dirtyfb_unsupported: false,
        })
    }

    /// Mode dimensions — callers size their paint buffer to this.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Present a tight-packed RGBA8 frame (`src_w * src_h * 4`, from the
    /// vello bridge) by converting to the framebuffer's XRGB8888 and
    /// flushing. `cursor` is the pointer position (output coords); when
    /// `Some`, a software cursor is composited on top (the DRM backend has
    /// no host to draw it). Logged-and-skipped on any error so a bad frame
    /// never crashes the compositor.
    pub fn present(&mut self, rgba: &[u8], src_w: u32, src_h: u32, cursor: Option<(f64, f64)>) {
        let pitch = self.buffer.pitch() as usize;
        let dw = self.width as usize;
        let dh = self.height as usize;
        let cw = (src_w as usize).min(dw);
        let ch = (src_h as usize).min(dh);
        let src_pitch = src_w as usize * 4;
        if rgba.len() < src_pitch * ch {
            tracing::warn!(
                got = rgba.len(),
                want = src_pitch * ch,
                "DRM scanout: short frame buffer; skipping"
            );
            return;
        }

        match self.card.map_dumb_buffer(&mut self.buffer) {
            Ok(mut map) => {
                let dst = map.as_mut();
                for y in 0..ch {
                    let so = y * src_pitch;
                    let dofs = y * pitch;
                    let srow = &rgba[so..so + cw * 4];
                    let drow = &mut dst[dofs..dofs + cw * 4];
                    for x in 0..cw {
                        let s = &srow[x * 4..x * 4 + 4]; // R, G, B, A
                        let d = &mut drow[x * 4..x * 4 + 4]; // B, G, R, X
                        d[0] = s[2];
                        d[1] = s[1];
                        d[2] = s[0];
                        d[3] = 0xFF;
                    }
                }
                if let Some((px, py)) = cursor {
                    draw_cursor(dst, pitch, dw, dh, px as i32, py as i32);
                }
            }
            Err(e) => {
                tracing::warn!("DRM scanout: map_dumb_buffer: {e}");
                return;
            }
        }

        if self.dirtyfb_unsupported {
            self.force_flush_via_set_crtc();
            return;
        }
        let clip = ClipRect::new(0, 0, self.width as u16, self.height as u16);
        if let Err(e) = self.card.dirty_framebuffer(self.fb, &[clip]) {
            // Driver without dirtyfb (rare): switch to set_crtc flushes.
            tracing::warn!("DRM scanout: dirty_framebuffer unsupported ({e}); using set_crtc");
            self.dirtyfb_unsupported = true;
            self.force_flush_via_set_crtc();
        }
    }

    fn force_flush_via_set_crtc(&self) {
        let _ = self.card.set_crtc(
            self.crtc,
            Some(self.fb),
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

/// Composite the software cursor into an XRGB8888 buffer at `(px, py)`
/// (the hot-spot). Bytes are little-endian XRGB: `[B, G, R, X]`.
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
