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

use drm_re::Device as BasicDevice;
use drm_re::buffer::{Buffer as _, DrmFourcc};
use drm_re::control::dumbbuffer::DumbBuffer;
use drm_re::control::{
    ClipRect, Device as ControlDevice, Event, Mode, ModeTypeFlags, PageFlipFlags, connector, crtc,
    framebuffer,
};

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

impl Card {
    /// Duplicate the card's file descriptor. The dup shares the same
    /// open file description, so it stays DRM master and queues into the
    /// **same** DRM event stream as the original — that's exactly why
    /// page-flip completions for every connector on this card arrive on
    /// a single fd and are routed by crtc (see
    /// [`DrmScanout::poll_flip_completions`]). Used to give each
    /// per-connector [`DrmScanout`] its own ioctl handle without a second
    /// `session.open` (which libseat refuses for an already-open node).
    fn try_clone(&self) -> Result<Card, String> {
        self.0
            .try_clone()
            .map(Card)
            .map_err(|e| format!("dup card fd: {e}"))
    }

    /// `dev_t` of the underlying `cardN` node, so the
    /// [`OutputRegistry`](super::outputs::OutputRegistry) can mint stable
    /// per-connector `OutputId`s. Best-effort: `0` on `fstat` failure
    /// (the connector id still disambiguates outputs on one card).
    fn dev_id(&self) -> u64 {
        match smithay::reexports::rustix::fs::fstat(&self.0) {
            Ok(st) => st.st_rdev as u64,
            Err(e) => {
                tracing::debug!("DRM scanout: fstat for dev_id failed: {e}");
                0
            }
        }
    }
}

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

    /// Park the cursor fully off this output's panel. Used by
    /// [`CursorPlanes`] when the pointer leaves this output for another:
    /// the KMS cursor plane is per-CRTC, so the plane on the output the
    /// pointer just left must be hidden or it would leave a frozen arrow
    /// behind. Moving it to `(-CURSOR_DIM, -CURSOR_DIM)` takes it entirely
    /// off-screen without touching the (still-bound) cursor buffer.
    pub fn hide(&self) {
        self.move_to(-(CURSOR_DIM as i32), -(CURSOR_DIM as i32));
    }
}

/// One output's hardware-cursor plane plus that output's rectangle in the
/// compositor's combined (global) coordinate space.
pub struct OutputCursor {
    /// `(x, y, w, h)` of the output in global coords.
    rect: (i32, i32, u32, u32),
    mover: HwCursor,
}

/// Routes a single global pointer position to the correct output's
/// hardware cursor plane (Track B3). The pointer lives in the
/// compositor's combined coordinate space; each KMS cursor plane is
/// per-CRTC and takes output-local coordinates, so on every motion we
/// find the output under the pointer, move *its* plane to the local
/// offset, and hide the plane on the output the pointer just left.
///
/// A single-output system collapses to one entry whose rect starts at
/// `(0, 0)`, so `move_to(gx, gy)` forwards `(gx, gy)` unchanged — the
/// single-output cursor path is identical to before B3.
pub struct CursorPlanes {
    outputs: Vec<OutputCursor>,
    /// Index of the output currently showing the cursor, if any.
    active: Option<usize>,
}

impl CursorPlanes {
    /// Build from `(rect, mover)` pairs — one per output whose hardware
    /// cursor plane is active. Outputs without a hardware cursor fall back
    /// to the per-output software cursor drawn in
    /// [`DrmScanout::present_no_drain`], so they're simply absent here.
    pub fn new(outputs: Vec<OutputCursor>) -> Self {
        Self {
            outputs,
            active: None,
        }
    }

    /// Construct one `OutputCursor`. Kept as a constructor so the rect
    /// field stays private to this module.
    pub fn output_cursor(rect: (i32, i32, u32, u32), mover: HwCursor) -> OutputCursor {
        OutputCursor { rect, mover }
    }

    /// Move the cursor to global `(gx, gy)`: pick the output whose rect
    /// contains the point, move that plane to the local offset, and hide
    /// the previously-active plane if the pointer crossed to a different
    /// output. Outside every output (a gap between mismatched-height
    /// monitors) the last position is left untouched.
    pub fn move_to(&mut self, gx: f64, gy: f64) {
        let (xi, yi) = (gx as i32, gy as i32);
        let rects: Vec<_> = self.outputs.iter().map(|o| o.rect).collect();
        let Some(i) = output_index_at(&rects, xi, yi) else {
            return;
        };
        if self.active != Some(i) {
            if let Some(prev) = self.active {
                self.outputs[prev].mover.hide();
            }
            self.active = Some(i);
        }
        let (ox, oy, _, _) = self.outputs[i].rect;
        self.outputs[i].mover.move_to(xi - ox, yi - oy);
    }
}

/// Index of the first output rectangle (`(x, y, w, h)`, global coords)
/// that contains the point `(xi, yi)`, or `None` if it falls in a gap
/// between mismatched-size outputs. Pure so the cursor-routing decision is
/// unit-testable without a DRM device.
fn output_index_at(rects: &[(i32, i32, u32, u32)], xi: i32, yi: i32) -> Option<usize> {
    rects.iter().position(|&(ox, oy, ow, oh)| {
        xi >= ox && xi < ox + ow as i32 && yi >= oy && yi < oy + oh as i32
    })
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
    /// Open `card_path` via the session and become DRM master. Split out
    /// of [`Self::new`] so the multi-output path ([`open_all_scanouts`])
    /// can open the card **once** and hand each per-connector scanout a
    /// dup of the same fd (libseat refuses a second open of one node).
    fn open_card<S: Session>(session: &mut S, card_path: &Path) -> Result<Card, String> {
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
        Ok(card)
    }

    /// Open `card_path` via the session, pick the **first** connected
    /// connector + mode + crtc, allocate two dumb framebuffers, modeset,
    /// and set up the hardware cursor plane. Returns `Err` (never panics)
    /// so the caller can run the compositor headless if scanout can't be
    /// set up.
    ///
    /// This is the single-output convenience constructor; the runtime
    /// drives every connected output via [`open_all_scanouts`], which
    /// shares the same per-connector [`Self::from_card`] body so a
    /// one-output system is byte-identical to this path.
    pub fn new<S: Session>(session: &mut S, card_path: &Path) -> Result<Self, String> {
        let card = Self::open_card(session, card_path)?;
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

        Self::from_card(card, connector, crtc, mode)
    }

    /// Allocate framebuffers, modeset `connector` on `crtc` at `mode`, and
    /// set up the hardware cursor plane — the hardware-bring-up tail
    /// shared by [`Self::new`] (single output) and [`open_all_scanouts`]
    /// (one call per connected connector). `card` is this scanout's own
    /// handle (an independent fd for the single path, a dup of the shared
    /// card fd for the multi path).
    fn from_card(
        card: Card,
        connector: connector::Handle,
        crtc: crtc::Handle,
        mode: Mode,
    ) -> Result<Self, String> {
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

        tracing::info!(
            width,
            height,
            "DRM scanout: modeset complete — display is live"
        );

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
                tracing::warn!(
                    "DRM scanout: hardware cursor unavailable ({e}); using software cursor"
                );
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
    /// This output's presentation interval, derived from the active
    /// mode's raw timing (`pixel_clock / (htotal * vtotal)`).
    ///
    /// Preferred over `Mode::vrefresh()`, which is rounded to whole
    /// hertz: the very common 59.94 Hz modes report `60`, and pacing a
    /// deadline series at 60.000 Hz against a 59.94 Hz panel walks a full
    /// frame out of phase every ~16 seconds. Returns `None` when the mode
    /// advertises timings that can't be a display (virtual connectors
    /// report zeroes), so the caller falls back instead of adopting a
    /// nonsense cadence.
    pub fn refresh_interval(&self) -> Option<std::time::Duration> {
        let (_, _, htotal) = self.mode.hsync();
        let (_, _, vtotal) = self.mode.vsync();
        crate::compositor::pacing::interval_from_mode_timing(
            self.mode.clock(),
            htotal as u32,
            vtotal as u32,
        )
        // Fall back to the rounded rate if the raw timing is unusable but
        // the mode still advertises a plausible refresh.
        .or_else(|| crate::compositor::pacing::interval_from_hz(self.mode.vrefresh()))
    }

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
                tracing::warn!(
                    "DRM scanout: cursor fd dup failed ({e}); cursor moves via paint path"
                );
                None
            }
        }
    }

    /// Present one frame, draining this scanout's own page-flip
    /// completions first. This is the **single-output** entry point and
    /// is unchanged from before B3 (drain, then present).
    ///
    /// `fill` writes the frame into the back buffer in XRGB8888
    /// (`[B, G, R, X]`), given the mapped destination slice and its row
    /// pitch — typically
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
        self.present_no_drain(cursor, fill);
    }

    /// Present one frame **without** draining DRM events first. The
    /// multi-output backend drains the shared card fd once per frame and
    /// routes each completion to the owning scanout via
    /// [`Self::mark_flip_complete`] before calling this, so draining here
    /// (per scanout, on the shared fd) would steal sibling outputs'
    /// completion events. Otherwise identical to the body of
    /// [`Self::present_into`].
    pub fn present_no_drain<F>(&mut self, cursor: Option<(f64, f64)>, fill: F)
    where
        F: FnOnce(&mut [u8], usize),
    {
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
    ///
    /// Single-output only: with one scanout every completion on the fd is
    /// this scanout's, so attributing them all to `self` is correct. The
    /// multi-output backend uses [`Self::poll_flip_completions`] +
    /// [`Self::mark_flip_complete`] to route by crtc instead.
    fn drain_flip_events(&mut self) {
        if let Ok(events) = self.card.receive_events() {
            for ev in events {
                if let Event::PageFlip(_) = ev {
                    self.flip_pending = false;
                }
            }
        }
    }

    /// Drain the card fd's pending DRM events and return the crtc of each
    /// page-flip completion. Because every per-connector scanout dups the
    /// **same** open file description, all crtcs' completions arrive on
    /// any one fd — so the multi-output backend drains this **once** (on
    /// the primary scanout) and routes each crtc to the owning scanout
    /// with [`Self::mark_flip_complete`]. Draining per scanout instead
    /// would let the first scanout consume (and discard) its siblings'
    /// completions, wedging their `flip_pending` forever.
    pub fn poll_flip_completions(&mut self) -> Vec<crtc::Handle> {
        let mut done = Vec::new();
        if let Ok(events) = self.card.receive_events() {
            for ev in events {
                if let Event::PageFlip(pf) = ev {
                    done.push(pf.crtc);
                }
            }
        }
        done
    }

    /// Clear `flip_pending` iff `crtc` is this scanout's crtc. Paired with
    /// [`Self::poll_flip_completions`] for the routed multi-output drain.
    pub fn mark_flip_complete(&mut self, crtc: crtc::Handle) {
        if crtc == self.crtc {
            self.flip_pending = false;
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

    /// Power the connected display on (`true`) or off (`false`) for the
    /// idle / session-lock blanking path (Track D2).
    ///
    /// Two mechanisms are applied for robustness:
    ///
    /// 1. **Legacy DPMS property** — `0 = On`, `3 = Off` per
    ///    `drm_mode.h`. This is what asks real hardware to enter its
    ///    low-power state. virtio-gpu (the VM target) may not honour it.
    /// 2. **CRTC toggle** — disabling the CRTC (`set_crtc` with no
    ///    framebuffer / connector) stops scanout entirely; re-enabling
    ///    it re-modesets the saved framebuffer + mode. This is the
    ///    reliable "screen goes dark" path across drivers, including the
    ///    VM, and is what gates whether anything reaches the panel.
    ///
    /// Both steps are best-effort: any failure is logged, never panics,
    /// so a quirky driver can't crash the compositor on blank/restore.
    /// The backend additionally skips [`Self::present`] while blanked, so
    /// no frame races the disabled CRTC.
    pub fn set_dpms(&mut self, on: bool) {
        self.set_dpms_property(on);
        if on {
            // Restore: re-modeset the saved framebuffer + mode so the
            // last frame is back on screen; the next `present` resumes
            // normal blitting.
            if let Err(e) = self.card.set_crtc(
                self.crtc,
                Some(self.fbs[self.front]),
                (0, 0),
                &[self.connector],
                Some(self.mode),
            ) {
                tracing::warn!("DRM scanout: set_dpms(on) restore set_crtc failed: {e}");
            } else {
                tracing::info!("DRM scanout: display restored (DPMS on)");
            }
        } else {
            // Blank: detach the CRTC so nothing is scanned out.
            if let Err(e) = self.card.set_crtc(self.crtc, None, (0, 0), &[], None) {
                tracing::warn!("DRM scanout: set_dpms(off) disable set_crtc failed: {e}");
            } else {
                tracing::info!("DRM scanout: display blanked (DPMS off)");
            }
        }
    }

    /// Best-effort legacy DPMS connector property toggle. Walks the
    /// connector's properties for one named `"DPMS"` and sets it to
    /// `On` (0) / `Off` (3). Absence of the property (some atomic-only
    /// drivers) or any ioctl error is logged at debug level and ignored
    /// — the CRTC toggle in [`Self::set_dpms`] is the load-bearing path.
    fn set_dpms_property(&self, on: bool) {
        // `drm_mode.h`: DRM_MODE_DPMS_ON = 0, DRM_MODE_DPMS_OFF = 3.
        let value: u64 = if on { 0 } else { 3 };
        let props = match self.card.get_properties(self.connector) {
            Ok(p) => p,
            Err(e) => {
                tracing::debug!("DRM scanout: get_properties for DPMS failed: {e}");
                return;
            }
        };
        for (handle, _current) in props.iter() {
            match self.card.get_property(*handle) {
                Ok(info) if info.name().to_str() == Ok("DPMS") => {
                    if let Err(e) = self.card.set_property(self.connector, *handle, value) {
                        tracing::debug!("DRM scanout: set DPMS property failed: {e}");
                    }
                    return;
                }
                Ok(_) => {}
                Err(e) => tracing::debug!("DRM scanout: get_property failed: {e}"),
            }
        }
    }
}

/// One connected output's scanout plus the registry-key fields the
/// runtime needs to intern a stable [`OutputId`](super::OutputId) and lay
/// the output out side-by-side.
pub struct ScanoutBuild {
    /// `dev_t` of the `cardN` node this output is on.
    pub device_id: u64,
    /// DRM connector handle (raw u32) for this output.
    pub connector_id: u32,
    /// Live, modeset scanout bound to this connector's crtc.
    pub scanout: DrmScanout,
}

/// Open `card_path` **once** and build one [`DrmScanout`] per connected
/// connector that advertises a usable mode, each bound to a distinct crtc
/// and sharing a dup of the single card fd (Track B3 multi-monitor).
///
/// Sharing one open file description is required: libseat refuses a second
/// `open` of an already-open node, so the dups are the only way to give
/// each connector its own ioctl handle. The shared description also means
/// every crtc's page-flip completions arrive on one event stream, which
/// the backend drains once and routes by crtc.
///
/// Each connector gets a distinct crtc via [`pick_crtc`]; a connector for
/// which no free crtc / mode / modeset is available is logged and skipped
/// rather than aborting the others. Returns `Err` only when **no** output
/// could be brought up (so the caller can fall back to headless), matching
/// [`DrmScanout::new`]'s contract.
///
/// For a single connected connector this returns a one-element `Vec` whose
/// scanout went through the same [`DrmScanout::from_card`] body as
/// [`DrmScanout::new`], so single-output bring-up is unchanged.
///
/// **Scope:** drives every connector on the *primary* card. Connectors on
/// *secondary* GPUs need their own card fd + GPU-to-GPU buffer handling
/// and remain a documented follow-up; hot-plug after startup is likewise a
/// follow-up (we enumerate everything connected at startup).
pub fn open_all_scanouts<S: Session>(
    session: &mut S,
    card_path: &Path,
) -> Result<Vec<ScanoutBuild>, String> {
    let card = DrmScanout::open_card(session, card_path)?;
    let device_id = card.dev_id();
    let res = card
        .resource_handles()
        .map_err(|e| format!("resource_handles: {e}"))?;

    let mut used_crtcs: Vec<crtc::Handle> = Vec::new();
    let mut builds: Vec<ScanoutBuild> = Vec::new();

    for &ch in res.connectors() {
        let info = match card.get_connector(ch, true) {
            Ok(i) => i,
            Err(e) => {
                tracing::warn!(?ch, "DRM scanout: get_connector failed: {e}");
                continue;
            }
        };
        if info.state() != connector::State::Connected {
            continue;
        }
        let Some(mode) = pick_mode(info.modes()) else {
            tracing::warn!(
                ?ch,
                "DRM scanout: connected connector advertises no usable mode; skipping"
            );
            continue;
        };
        let Some(crtc) = pick_crtc(&card, &res, &info, &used_crtcs) else {
            tracing::warn!(?ch, "DRM scanout: no free crtc for connector; skipping");
            continue;
        };
        // Each scanout owns its own dup of the shared card fd.
        let card_dup = match card.try_clone() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(?ch, "DRM scanout: {e}; skipping connector");
                continue;
            }
        };
        match DrmScanout::from_card(card_dup, info.handle(), crtc, mode) {
            Ok(scanout) => {
                used_crtcs.push(crtc);
                builds.push(ScanoutBuild {
                    device_id,
                    connector_id: u32::from(ch),
                    scanout,
                });
            }
            Err(e) => {
                tracing::warn!(
                    ?ch,
                    "DRM scanout: modeset failed for connector: {e}; skipping"
                );
            }
        }
    }

    // The local `card` is dropped here; the per-scanout dups keep the open
    // file description (and DRM master) alive.
    if builds.is_empty() {
        return Err("no connected connector could be brought up for scanout".into());
    }
    tracing::info!(count = builds.len(), "DRM scanout: brought up outputs");
    Ok(builds)
}

/// Pick a crtc for `conn_info` that isn't already in `used`.
///
/// Order: (1) the connector's current encoder's crtc if free (the common
/// already-lit case, and what [`DrmScanout::new`] uses); (2) walk the
/// connector's possible encoders and their `possible_crtcs` mask for a
/// free crtc (correct routing for multi-head GPUs); (3) any free crtc as a
/// last resort. Returns `None` when every crtc is taken (more connectors
/// than crtcs — rare, and the extra connector is skipped).
fn pick_crtc(
    card: &Card,
    res: &drm_re::control::ResourceHandles,
    conn_info: &connector::Info,
    used: &[crtc::Handle],
) -> Option<crtc::Handle> {
    // 1. Current encoder's crtc.
    if let Some(c) = conn_info
        .current_encoder()
        .and_then(|e| card.get_encoder(e).ok())
        .and_then(|e| e.crtc())
    {
        if !used.contains(&c) {
            return Some(c);
        }
    }
    // 2. Any crtc allowed by one of the connector's possible encoders.
    for &eh in conn_info.encoders() {
        if let Ok(enc) = card.get_encoder(eh) {
            for c in res.filter_crtcs(enc.possible_crtcs()) {
                if !used.contains(&c) {
                    return Some(c);
                }
            }
        }
    }
    // 3. Any free crtc.
    res.crtcs().iter().copied().find(|c| !used.contains(c))
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
        assert_eq!(
            &buf[o..o + 4],
            &[0, 0, 0, 0],
            "blank cursor area is transparent"
        );

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

    #[test]
    fn cursor_routing_single_output_is_global_coords() {
        // One output at the origin contains every in-bounds point, so the
        // routing always selects index 0 — identical to pre-B3 behaviour.
        let rects = [(0, 0, 1920u32, 1080u32)];
        assert_eq!(output_index_at(&rects, 0, 0), Some(0));
        assert_eq!(output_index_at(&rects, 1919, 1079), Some(0));
        assert_eq!(output_index_at(&rects, 1920, 0), None); // just past the edge
    }

    #[test]
    fn cursor_routing_picks_output_under_pointer() {
        // Two 1920x1080 outputs side by side: [0,1920) -> output 0,
        // [1920,3840) -> output 1. This is what decides which crtc's
        // cursor plane a pointer move drives.
        let rects = [(0, 0, 1920u32, 1080u32), (1920, 0, 1920u32, 1080u32)];
        assert_eq!(output_index_at(&rects, 10, 10), Some(0));
        assert_eq!(output_index_at(&rects, 1919, 500), Some(0));
        assert_eq!(output_index_at(&rects, 1920, 500), Some(1));
        assert_eq!(output_index_at(&rects, 3000, 500), Some(1));
        assert_eq!(output_index_at(&rects, 3840, 500), None); // off the right
    }

    #[test]
    fn cursor_routing_gap_between_mismatched_heights_is_none() {
        // A shorter second output leaves a gap below it; a point in that
        // gap belongs to no output and must not panic / mis-route.
        let rects = [(0, 0, 1920u32, 1080u32), (1920, 0, 1280u32, 720u32)];
        assert_eq!(output_index_at(&rects, 2000, 700), Some(1));
        assert_eq!(output_index_at(&rects, 2000, 800), None); // below output 1
    }
}
