//! DRM scanout — modeset a connector and present GPU-rendered frames.
//!
//! [`runtime`](super::runtime) acquires the seat and wires input; this
//! module keeps the primary card open as DRM master, sets a mode, and
//! presents each frame. Presentation uses a KMS hardware cursor plane
//! (`set_cursor2` + `move_cursor`, so pointer motion never touches the
//! framebuffer; a composited software cursor is the fallback when a driver
//! rejects the ioctls) and two dumb buffers ping-ponged with legacy
//! `page_flip`, falling back to dirty_framebuffer / set_crtc where flips
//! are unsupported.
//!
//! Not zero-copy: smithay's `DrmCompositor` drives a GLES `Renderer`, not
//! vello, so exporting the wgpu Vulkan image as a DMA-BUF remains a follow-up.
//!
//! Pixel byte order: vello renders `Rgba8Unorm`, the framebuffer is
//! `Xrgb8888` (little-endian `[B, G, R, X]`); the swizzle happens in
//! [`super::paint::VelloDrmBridge::paint_into_xrgb8888`].

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

use crate::compositor::backend::OutputId;

/// 64×64 is the universally-supported legacy cursor size (virtio-gpu, i915
/// and amdgpu all advertise it).
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
    /// Duplicate the card's fd. The dup shares one open file description, so
    /// it stays DRM master and every connector's page-flip completions
    /// arrive on a single event stream, routed by crtc (see
    /// [`DrmScanout::poll_flip_completions`]). Needed because libseat
    /// refuses a second `session.open` of an already-open node.
    pub(crate) fn try_clone(&self) -> Result<Card, String> {
        self.0
            .try_clone()
            .map(Card)
            .map_err(|e| format!("dup card fd: {e}"))
    }

    /// `dev_t` of the underlying `cardN` node, for stable per-connector
    /// `OutputId`s. Best-effort: `0` on `fstat` failure, since the connector
    /// id still disambiguates outputs on one card.
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

/// The loop thread's own handle on the primary card, held on the thread
/// that owns the session rather than the render thread that owns the
/// scanouts: it drops and retakes DRM master across a VT switch (every dup
/// shares one open file description, so releasing here releases for the
/// scanouts too), re-enumerates on hotplug, and builds scanouts for newly
/// connected connectors. It must outlive those scanouts — libseat refuses a
/// second `open` of an already-open node, so this is the only handle there
/// will ever be.
pub struct CardHandle {
    card: Card,
    device_id: u64,
}

/// One connected connector as the hotplug path sees it: enough to decide
/// whether it is new, gone, or re-moded, and to modeset it if it is new.
pub struct ConnectorSnapshot {
    pub connector_id: u32,
    pub handle: connector::Handle,
    pub mode: Mode,
    /// Physical panel size in millimetres, when the connector reports one.
    /// `None` for virtual connectors, which report 0×0.
    pub physical_mm: Option<(u32, u32)>,
}

impl CardHandle {
    /// Open `card_path` through the session and become DRM master.
    pub fn open<S: Session>(session: &mut S, card_path: &Path) -> Result<Self, String> {
        let card = DrmScanout::open_card(session, card_path)?;
        let device_id = card.dev_id();
        Ok(Self { card, device_id })
    }

    /// `dev_t` of the underlying node — the device half of an output's
    /// stable identity.
    pub fn device_id(&self) -> u64 {
        self.device_id
    }

    /// Give up DRM master. Called before a VT switch completes: the
    /// incoming session cannot modeset until we have.
    pub fn drop_master(&self) {
        match self.card.release_master_lock() {
            Ok(()) => tracing::info!("DRM scanout: released DRM master"),
            // Not fatal — libseat may already have taken it away on our
            // behalf, in which case the release is redundant, not wrong.
            Err(e) => tracing::debug!("DRM scanout: release_master_lock: {e}"),
        }
    }

    /// Take DRM master back. Every `set_crtc` after a resume needs it, so
    /// this runs before the modeset, not after.
    pub fn acquire_master(&self) -> bool {
        match self.card.acquire_master_lock() {
            Ok(()) => {
                tracing::info!("DRM scanout: re-acquired DRM master");
                true
            }
            Err(e) => {
                tracing::warn!("DRM scanout: acquire_master_lock on resume failed: {e}");
                false
            }
        }
    }

    /// Ask the hardware what is connected right now. `force_probe` is on: a
    /// hotplug event is precisely when the cached connector state is stale,
    /// and a cached probe reports the just-plugged monitor as still absent.
    pub fn connected(&self) -> Vec<ConnectorSnapshot> {
        let res = match self.card.resource_handles() {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("DRM scanout: resource_handles during re-enumeration: {e}");
                return Vec::new();
            }
        };
        let mut out = Vec::new();
        for &ch in res.connectors() {
            let info = match self.card.get_connector(ch, true) {
                Ok(i) => i,
                Err(e) => {
                    tracing::warn!(?ch, "DRM scanout: get_connector during re-enumeration: {e}");
                    continue;
                }
            };
            if info.state() != connector::State::Connected {
                continue;
            }
            let Some(mode) = pick_mode(info.modes()) else {
                continue;
            };
            out.push(ConnectorSnapshot {
                connector_id: u32::from(ch),
                handle: ch,
                mode,
                physical_mm: info.size().filter(|&(w, h)| w > 0 && h > 0),
            });
        }
        out
    }

    /// Modeset one connector onto a crtc not in `used`. `None` when there is
    /// no free crtc or the modeset failed — both logged, neither fatal, so
    /// the other outputs keep working.
    pub fn bring_up(&self, snapshot: &ConnectorSnapshot, used: &[crtc::Handle]) -> Option<ScanoutBuild> {
        let res = self.card.resource_handles().ok()?;
        let info = self.card.get_connector(snapshot.handle, false).ok()?;
        let Some(crtc) = pick_crtc(&self.card, &res, &info, used) else {
            tracing::warn!(
                connector = snapshot.connector_id,
                "DRM scanout: no free crtc for hot-plugged connector; skipping"
            );
            return None;
        };
        let card_dup = match self.card.try_clone() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("DRM scanout: {e}; cannot bring up connector");
                return None;
            }
        };
        match DrmScanout::from_card(card_dup, snapshot.handle, crtc, snapshot.mode) {
            Ok(scanout) => Some(ScanoutBuild {
                device_id: self.device_id,
                connector_id: snapshot.connector_id,
                physical_mm: snapshot.physical_mm,
                crtc,
                scanout,
            }),
            Err(e) => {
                tracing::warn!(
                    connector = snapshot.connector_id,
                    "DRM scanout: modeset failed for connector: {e}; skipping"
                );
                None
            }
        }
    }

    /// Bring up every connected connector (startup path). Hotplug uses
    /// [`Self::bring_up`] per connector instead, so already-live displays
    /// are not re-modeset — a re-modeset is a visible flash.
    pub fn bring_up_all(&self) -> Vec<ScanoutBuild> {
        let mut used: Vec<crtc::Handle> = Vec::new();
        let mut builds = Vec::new();
        for snapshot in self.connected() {
            if let Some(b) = self.bring_up(&snapshot, &used) {
                used.push(b.crtc);
                builds.push(b);
            }
        }
        builds
    }
}

/// Repositions the hardware cursor plane without touching the framebuffer.
/// Lives in the runtime's dispatch state so the libinput motion handler can
/// move the cursor at input rate, decoupled from the paint/flip cadence.
pub struct HwCursor {
    card: Card,
    crtc: crtc::Handle,
}

impl HwCursor {
    /// Move the cursor so its top-left (the arrow's hot-spot) sits at
    /// `(x, y)` in output pixels. A failing ioctl is logged and ignored — a
    /// missed cursor move must never crash the input path.
    pub fn move_to(&self, x: i32, y: i32) {
        #[allow(deprecated)]
        if let Err(e) = self.card.move_cursor(self.crtc, (x, y)) {
            tracing::trace!("DRM scanout: move_cursor failed: {e}");
        }
    }

    /// Park the cursor fully off this output's panel. The KMS cursor plane
    /// is per-CRTC, so the plane on an output the pointer just left must be
    /// hidden or it leaves a frozen arrow behind.
    pub fn hide(&self) {
        self.move_to(-(CURSOR_DIM as i32), -(CURSOR_DIM as i32));
    }
}

/// One output's hardware-cursor plane plus that output's rectangle in the
/// compositor's combined (global) coordinate space.
pub struct OutputCursor {
    /// Identity, not index: a hotplug reordering the list would otherwise
    /// move the cursor on the wrong monitor.
    id: OutputId,
    /// `(x, y, w, h)` of the output in global coords.
    rect: (i32, i32, u32, u32),
    mover: HwCursor,
}

/// Routes a global pointer position to the right output's hardware cursor
/// plane: each KMS plane is per-CRTC and takes output-local coordinates, so
/// motion moves the plane under the pointer and hides the one it left. A
/// single-output system collapses to one entry at `(0, 0)`, forwarding
/// coordinates unchanged.
#[derive(Default)]
pub struct CursorPlanes {
    outputs: Vec<OutputCursor>,
    /// The output currently showing the cursor, if any. By id rather than
    /// by index, because a hotplug reorders the list.
    active: Option<OutputId>,
}

impl CursorPlanes {
    /// One `OutputCursor` per output with an active hardware cursor plane.
    /// Outputs without one fall back to the software cursor drawn in
    /// [`DrmScanout::present_no_drain`], so they are simply absent here.
    pub fn new(outputs: Vec<OutputCursor>) -> Self {
        Self {
            outputs,
            active: None,
        }
    }

    /// Constructor so the `rect` field stays private to this module.
    pub fn output_cursor(id: OutputId, rect: (i32, i32, u32, u32), mover: HwCursor) -> OutputCursor {
        OutputCursor { id, rect, mover }
    }

    /// Whether any output has a hardware cursor plane at all.
    pub fn is_empty(&self) -> bool {
        self.outputs.is_empty()
    }

    /// Adopt a newly-modeset output's cursor plane, replacing any already
    /// registered for that id (what a torn-down-and-rebuilt connector gives).
    pub fn insert(&mut self, cursor: OutputCursor) {
        self.outputs.retain(|o| o.id != cursor.id);
        self.outputs.push(cursor);
        self.active = None;
    }

    /// Drop every plane whose output is no longer live; the dropped
    /// `HwCursor` closes its dup of the card fd with it.
    pub fn retain_outputs(&mut self, live: &[OutputId]) {
        let before = self.outputs.len();
        self.outputs.retain(|o| live.contains(&o.id));
        if self.outputs.len() != before {
            // The plane the cursor was on may be the one that went away.
            self.active = None;
        }
    }

    /// Move an output's rectangle after a re-layout — a stale rect routes
    /// the pointer to the wrong monitor past the point the layout shifted.
    pub fn set_rect(&mut self, id: OutputId, rect: (i32, i32, u32, u32)) {
        if let Some(o) = self.outputs.iter_mut().find(|o| o.id == id) {
            o.rect = rect;
        }
    }

    /// Move the cursor to global `(gx, gy)`, hiding the previously-active
    /// plane if the pointer crossed outputs. Outside every output (a gap
    /// between mismatched-height monitors) the last position is left alone.
    pub fn move_to(&mut self, gx: f64, gy: f64) {
        let (xi, yi) = (gx as i32, gy as i32);
        let rects: Vec<_> = self.outputs.iter().map(|o| o.rect).collect();
        let Some(i) = output_index_at(&rects, xi, yi) else {
            return;
        };
        let id = self.outputs[i].id;
        if self.active != Some(id) {
            if let Some(prev) = self.active {
                if let Some(o) = self.outputs.iter().find(|o| o.id == prev) {
                    o.mover.hide();
                }
            }
            self.active = Some(id);
        }
        let (ox, oy, _, _) = self.outputs[i].rect;
        self.outputs[i].mover.move_to(xi - ox, yi - oy);
    }
}

/// Index of the first output rect (`(x, y, w, h)`, global) containing
/// `(xi, yi)`, or `None` in a gap between mismatched-size outputs. Pure so
/// cursor routing is unit-testable without a DRM device.
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
    /// Cursor bitmap; kept alive so its GEM handle stays valid for
    /// `set_cursor2`, and re-bound by [`Self::restore`] after a VT switch or
    /// resume clears the plane. `None` when the hardware cursor is inactive.
    cursor_buffer: Option<DumbBuffer>,
    /// True when the KMS cursor plane is driving the pointer; when
    /// false we composite the software cursor into the framebuffer.
    hw_cursor_active: bool,
}

impl DrmScanout {
    /// Open `card_path` via the session and become DRM master. Split out of
    /// [`Self::new`] so [`open_all_scanouts`] can open the card once and dup
    /// the fd per connector — libseat refuses a second open of one node.
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

    /// Single-output convenience constructor: opens `card_path`, picks the
    /// first connected connector + mode + crtc, and brings it up. Returns
    /// `Err` (never panics) so the caller can run headless if scanout fails.
    /// The runtime uses [`open_all_scanouts`] instead, which shares the same
    /// [`Self::from_card`] body.
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
    /// set up the hardware cursor — the bring-up tail shared by
    /// [`Self::new`] and [`open_all_scanouts`]. `card` is this scanout's own
    /// handle: an independent fd on the single path, a dup on the multi path.
    fn from_card(
        card: Card,
        connector: connector::Handle,
        crtc: crtc::Handle,
        mode: Mode,
    ) -> Result<Self, String> {
        let (mw, mh) = mode.size();
        let (width, height) = (mw as u32, mh as u32);

        // Cleared to black so the frames before the first paint aren't garbage.
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

        // Best-effort: failure leaves `hw_cursor_active = false` and the
        // software cursor takes over.
        let (cursor_buffer, hw_cursor_active) = match Self::setup_hw_cursor(&card, crtc) {
            Ok(buf) => {
                // Centre it so a fresh boot shows the pointer before input arrives.
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
            cursor_buffer,
            hw_cursor_active,
        })
    }

    /// Allocate the 64×64 ARGB cursor buffer, paint the arrow, and bind it
    /// via `set_cursor2` (hotspot at the top-left tip, matching the bitmap).
    /// The returned buffer must be kept alive by the caller.
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

    /// Presentation interval from the mode's raw timing
    /// (`pixel_clock / (htotal * vtotal)`). Preferred over `Mode::vrefresh()`,
    /// which rounds to whole hertz: a 59.94 Hz panel reports `60`, and pacing
    /// at 60.000 Hz against it walks a full frame out of phase every ~16 s.
    /// `None` when the timings can't be a display (virtual connectors report
    /// zeroes), so the caller falls back rather than adopting a nonsense cadence.
    pub fn refresh_interval(&self) -> Option<std::time::Duration> {
        let (_, _, htotal) = self.mode.hsync();
        let (_, _, vtotal) = self.mode.vsync();
        crate::compositor::pacing::interval_from_mode_timing(
            self.mode.clock(),
            htotal as u32,
            vtotal as u32,
        )
        // Rounded rate as a fallback when the raw timing is unusable.
        .or_else(|| crate::compositor::pacing::interval_from_hz(self.mode.vrefresh()))
    }

    /// Mode dimensions — callers size their paint buffer to this.
    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// The crtc this scanout drives, so hotplug can pick one still free.
    pub fn crtc(&self) -> crtc::Handle {
        self.crtc
    }

    /// Re-establish this output after the session regains the hardware (VT
    /// switch or suspend). Nothing survives either: the CRTC has no mode,
    /// the framebuffer is unbound, the cursor plane is reset, and the queued
    /// page flip died with the master lock, leaving `flip_pending` latched
    /// forever. Best-effort throughout — every failure is logged and the
    /// rest of the sequence still runs.
    pub fn restore(&mut self) {
        // The in-flight flip died with the master lock; clearing this first
        // is what lets the next frame be presented at all.
        self.flip_pending = false;
        // Re-binding the mode is what the kernel needs to scan out again.
        if let Err(e) = self.card.set_crtc(
            self.crtc,
            Some(self.fbs[self.front]),
            (0, 0),
            &[self.connector],
            Some(self.mode),
        ) {
            tracing::warn!("DRM scanout: modeset on resume failed: {e}");
        } else {
            tracing::info!(
                width = self.width,
                height = self.height,
                "DRM scanout: re-modeset after session resume"
            );
        }
        // Per-CRTC state, gone with the rest — without this the pointer is
        // invisible after every VT switch.
        if self.hw_cursor_active {
            if let Some(buf) = self.cursor_buffer.as_ref() {
                #[allow(deprecated)]
                if let Err(e) = self.card.set_cursor2(self.crtc, Some(buf), (0, 0)) {
                    tracing::warn!(
                        "DRM scanout: re-binding the cursor plane on resume failed ({e}); \
                         falling back to the software cursor"
                    );
                    self.hw_cursor_active = false;
                }
            }
        }
    }

    /// A [`HwCursor`] mover for the runtime's input handler, holding a dup of
    /// the card fd so it can issue `move_cursor` independently of the paint
    /// path. `None` when the hardware cursor is inactive — the software
    /// fallback is driven from `present_into` instead.
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

    /// Present one frame, draining this scanout's own page-flip completions
    /// first — the **single-output** entry point. `fill` writes the frame
    /// into the back buffer in XRGB8888 (`[B, G, R, X]`) given the mapped
    /// slice and its row pitch; `cursor` is consulted only when the hardware
    /// cursor is unavailable. Errors are logged and skipped.
    pub fn present_into<F>(&mut self, cursor: Option<(f64, f64)>, fill: F)
    where
        F: FnOnce(&mut [u8], usize),
    {
        // Drain any retired completion so we can flip again.
        self.drain_flip_events();
        self.present_no_drain(cursor, fill);
    }

    /// Present one frame **without** draining DRM events first: the
    /// multi-output backend drains the shared card fd once per frame and
    /// routes completions via [`Self::mark_flip_complete`], so draining
    /// per-scanout here would steal sibling outputs' events.
    pub fn present_no_drain<F>(&mut self, cursor: Option<(f64, f64)>, fill: F)
    where
        F: FnOnce(&mut [u8], usize),
    {
        if self.use_pageflip && self.flip_pending {
            // Previous flip not yet retired — drop this frame (natural vsync
            // throttle); the next one supersedes it.
            return;
        }

        // Back buffer when flipping, else the single live buffer.
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
            // Software fallback only — the hardware plane composites itself
            // and must not also be drawn into the frame.
            if !self.hw_cursor_active {
                if let Some((px, py)) = cursor {
                    draw_cursor(dst, pitch, dw, dh, px as i32, py as i32);
                }
            }
        }

        if self.use_pageflip {
            // Best-effort CPU→host transfer before flipping: on virtual GPUs
            // dirty_framebuffer flushes the dumb buffer into the host
            // resource, without which a bare flip presents bytes the host
            // never received. A no-op or unsupported on real GPUs, hence ignored.
            let clip = ClipRect::new(0, 0, self.width as u16, self.height as u16);
            let _ = self.card.dirty_framebuffer(self.fbs[idx], &[clip]);
            self.flip_to(idx);
        } else {
            self.flush_dirty(idx);
        }
    }

    /// Queue a page flip to buffer `idx`, making it the new front. A
    /// transient `EBUSY`/`EAGAIN` skips the frame; a "not supported" errno
    /// latches the page-flip path off and falls back to dirty_framebuffer.
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

    /// Flush the live buffer with `DRM_IOCTL_MODE_DIRTYFB` (how virtio-gpu
    /// transfers a CPU buffer to the host), falling back to a forced
    /// `set_crtc` where dirtyfb is unsupported.
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

    /// Non-blocking drain of DRM events, clearing `flip_pending` on a
    /// page-flip completion. The fd is `O_NONBLOCK`, so `EAGAIN` is the
    /// no-events case, not a failure.
    ///
    /// Single-output only: attributing every completion to `self` is correct
    /// with one scanout. The multi-output backend routes by crtc via
    /// [`Self::poll_flip_completions`] + [`Self::mark_flip_complete`].
    fn drain_flip_events(&mut self) {
        if let Ok(events) = self.card.receive_events() {
            for ev in events {
                if let Event::PageFlip(_) = ev {
                    self.flip_pending = false;
                }
            }
        }
    }

    /// Drain pending DRM events, returning the crtc of each page-flip
    /// completion. Every per-connector scanout dups the same open file
    /// description, so all crtcs' completions arrive on any one fd: the
    /// backend must drain this **once** and route with
    /// [`Self::mark_flip_complete`]. Draining per scanout would let the
    /// first consume its siblings' completions, wedging `flip_pending` forever.
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

    /// Power the display on/off for idle and session-lock blanking, via both
    /// the legacy DPMS property (`0 = On`, `3 = Off` per `drm_mode.h`, which
    /// virtio-gpu may ignore) and a CRTC toggle, the reliable path across
    /// drivers. Both best-effort; the backend skips [`Self::present`] while
    /// blanked so no frame races the disabled CRTC.
    pub fn set_dpms(&mut self, on: bool) {
        self.set_dpms_property(on);
        if on {
            // Re-modeset the saved framebuffer + mode so the last frame is
            // back on screen; the next `present` resumes normal blitting.
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

    /// Best-effort legacy DPMS property toggle. A missing property
    /// (atomic-only drivers) or ioctl error is ignored — the CRTC toggle in
    /// [`Self::set_dpms`] is the load-bearing path.
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

/// One output's scanout plus the fields the runtime needs to intern a stable
/// [`OutputId`](super::OutputId) and lay outputs out side-by-side.
pub struct ScanoutBuild {
    /// `dev_t` of the `cardN` node this output is on.
    pub device_id: u64,
    /// DRM connector handle (raw u32) for this output.
    pub connector_id: u32,
    /// Physical panel size in mm, when reported — the one input to scale
    /// derivation that resolution alone cannot supply (see [`super::scale`]).
    pub physical_mm: Option<(u32, u32)>,
    /// The crtc this output was modeset onto, so a later hotplug can pick
    /// one still free.
    pub crtc: crtc::Handle,
    /// Live, modeset scanout bound to this connector's crtc.
    pub scanout: DrmScanout,
}

/// Open `card_path` once and build one [`DrmScanout`] per connected
/// connector with a usable mode, each on a distinct crtc and sharing a dup
/// of the single card fd.
///
/// Sharing one open file description is required — libseat refuses a second
/// `open` of an already-open node — and it also means every crtc's
/// page-flip completions arrive on one event stream, drained once and
/// routed by crtc. A connector with no free crtc / mode is logged and
/// skipped; `Err` only when no output at all could be brought up.
///
/// Primary card only: connectors on secondary GPUs need their own card fd
/// and GPU-to-GPU buffer handling. The returned [`CardHandle`] stays open
/// on the loop thread so later hotplugs need no second `session.open`.
pub fn open_all_scanouts<S: Session>(
    session: &mut S,
    card_path: &Path,
) -> Result<(CardHandle, Vec<ScanoutBuild>), String> {
    let card = CardHandle::open(session, card_path)?;
    let builds = card.bring_up_all();
    if builds.is_empty() {
        return Err("no connected connector could be brought up for scanout".into());
    }
    tracing::info!(count = builds.len(), "DRM scanout: brought up outputs");
    Ok((card, builds))
}

/// Pick a crtc for `conn_info` not already in `used`: the current encoder's
/// crtc if free, else one allowed by the connector's possible encoders
/// (correct routing on multi-head GPUs), else any free crtc. `None` when
/// every crtc is taken, and the extra connector is skipped.
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

/// Choose a scanout mode: trust the first *reasonable* mode (>= 1024 wide),
/// since the kernel lists the EDID-preferred one first. The PREFERRED flag
/// itself is not trusted — virtio-gpu flags a 640x480 default while
/// advertising modes up to 5120x2160 — and the fallback caps at 1920x1080.
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
        // One output at the origin contains every in-bounds point, so
        // routing always selects index 0.
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
