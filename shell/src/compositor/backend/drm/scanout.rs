//! DRM scanout — modeset a connector and present GPU-rendered frames.
//!
//! KMS hardware cursor plane (`set_cursor2`/`move_cursor`, software cursor as fallback) plus
//! two dumb buffers ping-ponged via `page_flip`, falling back to `dirty_framebuffer/set_crtc`.
//! Not zero-copy (smithay's `DrmCompositor` drives GLES, not vello); pixel order: vello's
//! `Rgba8Unorm` swizzles to framebuffer `Xrgb8888` in [`super::paint::VelloDrmBridge::paint_into_xrgb8888`].

use std::collections::BTreeMap;
use std::io::Read as _;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::path::{Path, PathBuf};

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

/// 64×64 is the universally-supported legacy cursor size (virtio-gpu, i915 and amdgpu all advertise it).
const CURSOR_DIM: u32 = 64;

/// Minimal owned-fd wrapper implementing the `drm` crate's device traits; fd comes from libseat (`Session::open`), privileged + DRM master while the session is active.
struct Card(OwnedFd);

impl AsFd for Card {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}
impl BasicDevice for Card {}
impl ControlDevice for Card {}

impl Card {
    /// Duplicate the card's fd — shares one open file description, so it stays DRM master and every
    /// crtc's flip completions arrive on one event stream, routed by crtc (see [`DrmScanout::poll_flip_completions`]). libseat refuses a second `session.open` of an already-open node.
    pub(crate) fn try_clone(&self) -> Result<Card, String> {
        self.0
            .try_clone()
            .map(Card)
            .map_err(|e| format!("dup card fd: {e}"))
    }

    /// `dev_t` of the underlying `cardN` node for stable per-connector `OutputId`s; best-effort (`0` on `fstat` failure — connector id still disambiguates).
    fn dev_id(&self) -> u64 {
        match smithay::reexports::rustix::fs::fstat(&self.0) {
            Ok(st) => st.st_rdev,
            Err(e) => {
                tracing::debug!("DRM scanout: fstat for dev_id failed: {e}");
                0
            }
        }
    }
}

/// The loop thread's own handle on the primary card (held by the session-owning thread, not the
/// render thread): drops/retakes DRM master across a VT switch — releasing here releases for the
/// scanouts too, since every dup shares one open file description — and builds scanouts for new connectors on hotplug. Must outlive the scanouts; libseat refuses a second open of an already-open node.
pub struct CardHandle {
    card: Card,
    device_id: u64,
    card_path: PathBuf,
}

/// One connected connector as the hotplug path sees it: enough to decide whether it is new, gone, or re-moded, and to modeset it if new.
pub struct ConnectorSnapshot {
    pub connector_id: u32,
    pub handle: connector::Handle,
    pub mode: Mode,
    pub connector_name: String,
    /// Physical panel size in millimetres, when the connector reports one. `None` for virtual connectors, which report 0×0.
    pub physical_mm: Option<(u32, u32)>,
}

impl CardHandle {
    /// Open `card_path` through the session and become DRM master.
    pub fn open<S: Session>(session: &mut S, card_path: &Path) -> Result<Self, String> {
        let card = DrmScanout::open_card(session, card_path)?;
        let device_id = card.dev_id();
        Ok(Self {
            card,
            device_id,
            card_path: card_path.to_path_buf(),
        })
    }

    /// `dev_t` of the underlying node — the device half of an output's stable identity.
    pub fn device_id(&self) -> u64 {
        self.device_id
    }

    /// Give up DRM master. Called before a VT switch completes: the incoming session cannot modeset until we have.
    pub fn drop_master(&self) {
        match self.card.release_master_lock() {
            Ok(()) => tracing::info!("DRM scanout: released DRM master"),
            // Not fatal — libseat may already have taken it away on our behalf, so the release can be redundant.
            Err(e) => tracing::debug!("DRM scanout: release_master_lock: {e}"),
        }
    }

    /// Take DRM master back. Every `set_crtc` after a resume needs it, so this runs before the modeset.
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

    /// Ask the hardware what is connected now; `force_probe` is on since a hotplug event is precisely when cached connector state is stale.
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
            let connector_name = format!("{}-{}", info.interface().as_str(), info.interface_id());
            let lower_name = connector_name.to_ascii_lowercase();
            if lower_name.contains("writeback") || lower_name.contains("virtual") {
                continue;
            }
            let output_config = settings_store::hardware::DisplayConfig::load_effective();
            if output_config
                .outputs
                .get(&connector_name)
                .is_some_and(|c| !c.enabled)
            {
                continue;
            }
            let requested = output_config
                .outputs
                .get(&connector_name)
                .and_then(|c| c.mode);
            let Some(mode) = pick_configured_mode(info.modes(), requested) else {
                continue;
            };
            out.push(ConnectorSnapshot {
                connector_id: u32::from(ch),
                handle: ch,
                mode,
                connector_name,
                physical_mm: info.size().filter(|&(w, h)| w > 0 && h > 0),
            });
        }
        out
    }

    /// Describe every physically connected sink using the live KMS mode
    /// table. The result is published to Settings; it intentionally reports
    /// HDR/VRR as unavailable until this XRGB8888 scanout path can set the
    /// corresponding DRM properties. EDID support alone is not an applicable
    /// compositor capability.
    pub fn display_capabilities(
        &self,
        profiles: &BTreeMap<String, String>,
    ) -> Vec<settings_store::hardware::DisplayOutputSnapshot> {
        use settings_store::hardware::{DisplayOutputSnapshot, OutputDescriptor, OutputMode};

        let Ok(resources) = self.card.resource_handles() else {
            return Vec::new();
        };
        let config = settings_store::hardware::DisplayConfig::load_effective();
        let card_name = self
            .card_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("card0");
        let mut outputs = Vec::new();
        for &handle in resources.connectors() {
            let Ok(info) = self.card.get_connector(handle, true) else {
                continue;
            };
            if info.state() != connector::State::Connected {
                continue;
            }
            let connector = format!("{}-{}", info.interface().as_str(), info.interface_id());
            let lower_name = connector.to_ascii_lowercase();
            if lower_name.contains("writeback") || lower_name.contains("virtual") {
                continue;
            }
            let modes: Vec<OutputMode> = info
                .modes()
                .iter()
                .map(|mode| {
                    let (width, height) = mode.size();
                    OutputMode {
                        width: u32::from(width),
                        height: u32::from(height),
                        refresh_mhz: mode_refresh_mhz(mode),
                    }
                })
                .collect();
            let requested = config.outputs.get(&connector).and_then(|cfg| cfg.mode);
            let Some(current) = pick_configured_mode(info.modes(), requested) else {
                continue;
            };
            let (width, height) = current.size();
            let current_mode = OutputMode {
                width: u32::from(width),
                height: u32::from(height),
                refresh_mhz: mode_refresh_mhz(&current),
            };
            let sysfs = Path::new("/sys/class/drm").join(format!("{card_name}-{connector}"));
            let sink_hdr = std::fs::read(sysfs.join("edid"))
                .ok()
                .map(|edid| {
                    crate::wayland::edid::parse_hdr_display_info(&edid)
                        .supported_transfers
                        .iter()
                        .any(|transfer| transfer.is_hdr())
                })
                .unwrap_or(false);
            // `vrr_capable` is a standard DRM connector property exported by
            // modern amdgpu/i915 kernels. Absence is unknown/not usable, never
            // interpreted as support.
            let sink_vrr = std::fs::read_to_string(sysfs.join("vrr_capable"))
                .ok()
                .is_some_and(|value| value.trim() == "1");
            let upper = connector.to_ascii_uppercase();
            let builtin =
                upper.starts_with("EDP") || upper.starts_with("LVDS") || upper.starts_with("DSI");
            outputs.push(DisplayOutputSnapshot {
                descriptor: OutputDescriptor {
                    connector,
                    modes,
                    // This backend currently presents XRGB8888 dumb buffers
                    // and does not set HDR_OUTPUT_METADATA / Colorspace.
                    hdr: false,
                    // Likewise it does not set the CRTC VRR_ENABLED property.
                    vrr: false,
                    color_profiles: profiles.keys().cloned().collect(),
                },
                current_mode,
                builtin,
                sink_hdr,
                hdr_unavailable_reason: sink_hdr.then(|| {
                    "Display supports HDR, but the active XRGB8888 scanout path cannot signal HDR safely"
                        .into()
                }),
                sink_vrr,
                vrr_unavailable_reason: sink_vrr.then(|| {
                    "Display supports adaptive sync, but the active scanout path cannot enable the CRTC VRR property"
                        .into()
                }),
                color_profile_labels: profiles.clone(),
            });
        }
        outputs.sort_by(|a, b| a.descriptor.connector.cmp(&b.descriptor.connector));
        outputs
    }

    /// Modeset one connector onto a crtc not in `used`; `None` (logged, not fatal) when no free crtc or the modeset failed, so other outputs keep working.
    pub fn bring_up(
        &self,
        snapshot: &ConnectorSnapshot,
        used: &[crtc::Handle],
    ) -> Option<ScanoutBuild> {
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
                connector_name: snapshot.connector_name.clone(),
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

    /// Bring up every connected connector (startup path); hotplug uses [`Self::bring_up`] per connector instead so already-live displays aren't re-modeset (a visible flash).
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

/// Discover ICC profiles from the locations used by colord and the XDG base
/// directory spec. Files are bounded and checked for the ICC `acsp` signature
/// before their canonical paths cross the Settings/compositor boundary.
pub(super) fn discover_icc_profiles() -> BTreeMap<String, String> {
    let mut roots = vec![
        PathBuf::from("/usr/share/color/icc"),
        PathBuf::from("/usr/local/share/color/icc"),
        PathBuf::from("/var/lib/colord/icc"),
    ];
    if let Some(data) = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| dirs::home_dir().map(|home| home.join(".local/share")))
    {
        roots.push(data.join("icc"));
        roots.push(data.join("color/icc"));
    }
    let mut profiles = BTreeMap::new();
    'roots: for root in roots {
        let Ok(root) = root.canonicalize() else {
            continue;
        };
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            if profiles.len() >= 128 {
                break 'roots;
            }
            let path = entry.path();
            let extension = path
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or_default();
            if !extension.eq_ignore_ascii_case("icc") && !extension.eq_ignore_ascii_case("icm") {
                continue;
            }
            let Ok(path) = path.canonicalize() else {
                continue;
            };
            if !path.starts_with(&root) {
                continue;
            }
            let Ok(metadata) = path.metadata() else {
                continue;
            };
            if !metadata.is_file() || metadata.len() > 16 * 1024 * 1024 {
                continue;
            }
            let Ok(mut file) = std::fs::File::open(&path) else {
                continue;
            };
            let mut header = [0u8; 40];
            if file.read_exact(&mut header).is_err() || &header[36..40] != b"acsp" {
                continue;
            }
            // Parsing and constructing the transform here makes the
            // advertised list truthful. This function runs only on the ICC
            // worker, never the compositor loop or Settings UI thread.
            let Some(profile_path) = path.to_str() else {
                continue;
            };
            let Some(output) = qcms::Profile::new_from_path(profile_path) else {
                continue;
            };
            if qcms::Transform::new(
                &qcms::Profile::new_sRGB(),
                &output,
                qcms::DataType::RGBA8,
                qcms::Intent::Perceptual,
            )
            .is_none()
            {
                continue;
            }
            let label = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or("ICC profile")
                .replace(['_', '-'], " ");
            profiles.insert(path.to_string_lossy().into_owned(), label);
        }
    }
    profiles
}

/// Exact milli-Hz from raw mode timing. DRM's `vrefresh()` is an integer Hz
/// convenience and collapses 59.94/60.00 or 23.976/24.00 into one setting.
fn mode_refresh_mhz(mode: &Mode) -> u32 {
    let (_, _, htotal) = mode.hsync();
    let (_, _, vtotal) = mode.vsync();
    let denominator = u64::from(htotal) * u64::from(vtotal);
    if denominator == 0 {
        return mode.vrefresh().saturating_mul(1_000);
    }
    // Pixel clock is kHz. kHz * 1_000_000 / pixels-per-frame = mHz.
    (u64::from(mode.clock())
        .saturating_mul(1_000_000)
        .saturating_add(denominator / 2)
        / denominator)
        .try_into()
        .unwrap_or(u32::MAX)
}

/// Repositions the hardware cursor plane without touching the framebuffer. Lives in the
/// runtime's dispatch state so the libinput motion handler can move the cursor at input rate,
/// decoupled from the paint/flip cadence.
pub struct HwCursor {
    card: Card,
    crtc: crtc::Handle,
}

impl HwCursor {
    /// Move the cursor so its top-left (hot-spot) sits at `(x, y)` in output pixels; a failing ioctl is logged and ignored — a missed move must never crash input.
    pub fn move_to(&self, x: i32, y: i32) {
        #[allow(deprecated)]
        if let Err(e) = self.card.move_cursor(self.crtc, (x, y)) {
            tracing::trace!("DRM scanout: move_cursor failed: {e}");
        }
    }

    /// Park the cursor off this output's panel; the KMS plane is per-CRTC, so a plane on an output the pointer left must be hidden or it leaves a frozen arrow.
    pub fn hide(&self) {
        self.move_to(-(CURSOR_DIM as i32), -(CURSOR_DIM as i32));
    }
}

/// One output's hardware-cursor plane plus that output's rectangle in the compositor's combined (global) coordinate space.
pub struct OutputCursor {
    /// Identity, not index: a hotplug reordering the list would otherwise move the cursor on the wrong monitor.
    id: OutputId,
    /// `(x, y, w, h)` of the output in global coords.
    rect: (i32, i32, u32, u32),
    transform: settings_store::hardware::OutputTransform,
    mover: HwCursor,
}

/// Routes a global pointer position to the right output's hardware cursor plane: each KMS plane
/// is per-CRTC and output-local, so motion moves the plane under the pointer and hides the one it left. Single-output collapses to one entry at `(0, 0)`.
#[derive(Default)]
pub struct CursorPlanes {
    outputs: Vec<OutputCursor>,
    /// The output currently showing the cursor, if any. By id rather than index, because a hotplug reorders the list.
    active: Option<OutputId>,
}

impl CursorPlanes {
    /// One `OutputCursor` per output with an active hardware cursor plane; outputs without one fall back to the software cursor in [`DrmScanout::present_no_drain`].
    pub fn new(outputs: Vec<OutputCursor>) -> Self {
        Self {
            outputs,
            active: None,
        }
    }

    /// Constructor so the `rect` field stays private to this module.
    pub fn output_cursor(
        id: OutputId,
        rect: (i32, i32, u32, u32),
        mover: HwCursor,
    ) -> OutputCursor {
        OutputCursor {
            id,
            rect,
            transform: settings_store::hardware::OutputTransform::Normal,
            mover,
        }
    }

    /// Whether any output has a hardware cursor plane at all.
    pub fn is_empty(&self) -> bool {
        self.outputs.is_empty()
    }

    /// Adopt a newly-modeset output's cursor plane, replacing any already registered for that id (what a torn-down-and-rebuilt connector gives).
    pub fn insert(&mut self, cursor: OutputCursor) {
        self.outputs.retain(|o| o.id != cursor.id);
        self.outputs.push(cursor);
        self.active = None;
    }

    /// Drop every plane whose output is no longer live; the dropped `HwCursor` closes its dup of the card fd with it.
    pub fn retain_outputs(&mut self, live: &[OutputId]) {
        let before = self.outputs.len();
        self.outputs.retain(|o| live.contains(&o.id));
        if self.outputs.len() != before {
            // The plane the cursor was on may be the one that went away.
            self.active = None;
        }
    }

    /// Move an output's rectangle after a re-layout — a stale rect routes the pointer to the wrong monitor.
    pub fn set_rect(&mut self, id: OutputId, rect: (i32, i32, u32, u32)) {
        if let Some(o) = self.outputs.iter_mut().find(|o| o.id == id) {
            o.rect = rect;
        }
    }

    pub fn set_transform(
        &mut self,
        id: OutputId,
        transform: settings_store::hardware::OutputTransform,
    ) {
        if let Some(output) = self.outputs.iter_mut().find(|output| output.id == id) {
            output.transform = transform;
        }
    }

    /// Move the cursor to global `(gx, gy)`, hiding the previously-active plane on an output crossing; outside every output (mismatched-height gap) the last position is left alone.
    pub fn move_to(&mut self, gx: f64, gy: f64) {
        let (xi, yi) = (gx as i32, gy as i32);
        let rects: Vec<_> = self.outputs.iter().map(|o| o.rect).collect();
        let Some(i) = output_index_at(&rects, xi, yi) else {
            return;
        };
        let id = self.outputs[i].id;
        if self.active != Some(id) {
            if let Some(prev) = self.active
                && let Some(o) = self.outputs.iter().find(|o| o.id == prev)
            {
                o.mover.hide();
            }
            self.active = Some(id);
        }
        let (ox, oy, width, height) = self.outputs[i].rect;
        let (x, y) =
            logical_to_physical_point(xi - ox, yi - oy, width, height, self.outputs[i].transform);
        self.outputs[i].mover.move_to(x, y);
    }
}

pub(crate) fn logical_to_physical_point(
    x: i32,
    y: i32,
    logical_width: u32,
    logical_height: u32,
    transform: settings_store::hardware::OutputTransform,
) -> (i32, i32) {
    use settings_store::hardware::OutputTransform as T;
    let (width, height) = (logical_width as i32, logical_height as i32);
    match transform {
        T::Normal => (x, y),
        T::Rotate90 => (height.saturating_sub(1) - y, x),
        T::Rotate180 => (width.saturating_sub(1) - x, height.saturating_sub(1) - y),
        T::Rotate270 => (y, width.saturating_sub(1) - x),
    }
}

/// Index of the first output rect containing `(xi, yi)`, or `None` in a gap between mismatched-size outputs; pure so cursor routing is unit-testable without a DRM device.
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
    /// Double-buffered scanout targets: `front` is displayed (or the in-flight flip target); the renderer draws into `1 - front`.
    buffers: [DumbBuffer; 2],
    fbs: [framebuffer::Handle; 2],
    front: usize,
    /// True between requesting a page flip and draining its completion; guards against queuing a second flip before the first retires (kernel rejects with `EBUSY`).
    flip_pending: bool,
    /// Whether the page-flip path is in use; latched `false` if a flip fails with a "not supported" errno, falling back to `dirty_framebuffer/set_crtc`.
    use_pageflip: bool,
    width: u32,
    height: u32,
    /// Set once `dirty_framebuffer` is known to be unsupported, so we stop retrying it and use `set_crtc` to force a flush instead.
    dirtyfb_unsupported: bool,
    /// Cursor bitmap, kept alive so its GEM handle stays valid for `set_cursor2`; re-bound by [`Self::restore`] after a VT switch/resume clears the plane. `None` when inactive.
    cursor_buffer: Option<DumbBuffer>,
    /// True when the KMS cursor plane is driving the pointer; when false we composite the software cursor into the framebuffer.
    hw_cursor_active: bool,
    cursor_size: u32,
}

impl DrmScanout {
    /// Open `card_path` and become DRM master; split from [`Self::new`] so [`open_all_scanouts`] opens once and dups the fd per connector (libseat refuses a second open).
    fn open_card<S: Session>(session: &mut S, card_path: &Path) -> Result<Card, String> {
        use smithay::backend::session::AsErrno;
        let fd = session
            .open(
                card_path,
                OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
            )
            .map_err(|e| format!("session.open({card_path:?}): errno={:?}", e.as_errno()))?;
        let card = Card(fd);
        // Become DRM master so set_crtc is permitted. libseat normally grants it on the active session; acquire explicitly and log.
        match card.acquire_master_lock() {
            Ok(()) => tracing::info!("DRM scanout: acquired DRM master"),
            Err(e) => {
                tracing::warn!(
                    "DRM scanout: acquire_master_lock failed ({e}); set_crtc may EACCES"
                );
            }
        }
        Ok(card)
    }

    /// Single-output convenience: opens `card_path`, picks the first connected connector + mode +
    /// crtc, and brings it up. Returns `Err`, never panics; the runtime uses [`open_all_scanouts`] instead, sharing [`Self::from_card`].
    pub fn new<S: Session>(session: &mut S, card_path: &Path) -> Result<Self, String> {
        let card = Self::open_card(session, card_path)?;
        let res = card
            .resource_handles()
            .map_err(|e| format!("resource_handles: {e}"))?;

        // First connected connector that advertises a mode.
        let mut chosen: Option<(connector::Info, Mode)> = None;
        for &ch in res.connectors() {
            if let Ok(info) = card.get_connector(ch, true)
                && info.state() == connector::State::Connected
                && let Some(m) = pick_configured_mode(info.modes(), None)
            {
                chosen = Some((info, m));
                break;
            }
        }
        let (conn_info, mode) = chosen.ok_or("no connected connector with a usable mode")?;
        let connector = conn_info.handle();

        // Prefer the connector's current encoder/crtc; fall back to the first crtc (single-GPU/VM case).
        let crtc = conn_info
            .current_encoder()
            .and_then(|e| card.get_encoder(e).ok())
            .and_then(|e| e.crtc())
            .or_else(|| res.crtcs().first().copied())
            .ok_or("no crtc available for connector")?;

        Self::from_card(card, connector, crtc, mode)
    }

    /// Allocate framebuffers, modeset `connector` on `crtc` at `mode`, and set up the hardware
    /// cursor — the bring-up tail shared by [`Self::new`] and [`open_all_scanouts`]. `card` is an independent fd on the single path, a dup on the multi path.
    fn from_card(
        card: Card,
        connector: connector::Handle,
        crtc: crtc::Handle,
        mode: Mode,
    ) -> Result<Self, String> {
        let (mw, mh) = mode.size();
        let (width, height) = (u32::from(mw), u32::from(mh));

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

        // Best-effort: failure leaves `hw_cursor_active = false` and the software cursor takes over.
        let cursor_size =
            super::input_config::AccessibilityInputSettings::load().effective_cursor_size();
        let (cursor_buffer, hw_cursor_active) =
            match Self::setup_hw_cursor(&card, crtc, cursor_size) {
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
            cursor_size,
        })
    }

    /// Allocate the 64×64 ARGB cursor buffer, paint the arrow, bind via `set_cursor2` (hotspot at top-left, matching the bitmap); caller must keep the buffer alive.
    fn setup_hw_cursor(
        card: &Card,
        crtc: crtc::Handle,
        cursor_size: u32,
    ) -> Result<DumbBuffer, String> {
        let mut cbuf = card
            .create_dumb_buffer((CURSOR_DIM, CURSOR_DIM), DrmFourcc::Argb8888, 32)
            .map_err(|e| format!("create cursor dumb buffer: {e}"))?;
        let pitch = cbuf.pitch() as usize;
        {
            let mut map = card
                .map_dumb_buffer(&mut cbuf)
                .map_err(|e| format!("map cursor buffer: {e}"))?;
            paint_cursor_argb(map.as_mut(), pitch, cursor_size);
        }
        // Hotspot (0,0): the arrow's tip is the buffer's top-left pixel, so plane position == pointer position; virtual GPUs (virtio-gpu/QXL) use it to place the host pointer.
        #[allow(deprecated)]
        card.set_cursor2(crtc, Some(&cbuf), (0, 0))
            .map_err(|e| format!("set_cursor2: {e}"))?;
        Ok(cbuf)
    }

    pub fn set_cursor_size(&mut self, cursor_size: u32) {
        let cursor_size = cursor_size.clamp(24, CURSOR_DIM);
        self.cursor_size = cursor_size;
        if let Some(buffer) = self.cursor_buffer.as_mut() {
            let pitch = buffer.pitch() as usize;
            if let Ok(mut map) = self.card.map_dumb_buffer(buffer) {
                paint_cursor_argb(map.as_mut(), pitch, cursor_size);
            }
        }
    }

    /// Presentation interval from the mode's raw timing (`pixel_clock / (htotal * vtotal)`),
    /// preferred over `Mode::vrefresh()` which rounds to whole hertz — a 59.94 Hz panel reports `60`, drifting a full frame out of phase every ~16s. `None` when timings can't be a display.
    pub fn refresh_interval(&self) -> Option<std::time::Duration> {
        let (_, _, htotal) = self.mode.hsync();
        let (_, _, vtotal) = self.mode.vsync();
        crate::compositor::pacing::interval_from_mode_timing(
            self.mode.clock(),
            u32::from(htotal),
            u32::from(vtotal),
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

    /// Re-establish this output after the session regains hardware (VT switch/suspend): nothing
    /// survives — CRTC has no mode, framebuffer unbound, cursor plane reset, and the queued flip died with the master lock, leaving `flip_pending` latched forever. Best-effort throughout.
    pub fn restore(&mut self) {
        // The in-flight flip died with the master lock; clearing this first lets the next frame present at all.
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
        // Per-CRTC state, gone with the rest — without this the pointer is invisible after every VT switch.
        if self.hw_cursor_active
            && let Some(buf) = self.cursor_buffer.as_ref()
        {
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

    /// A [`HwCursor`] mover for the runtime's input handler, holding a dup of the card fd so
    /// `move_cursor` runs independent of the paint path. `None` when the hardware cursor is inactive — software fallback drives from `present_into` instead.
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

    /// Present one frame, draining this scanout's own page-flip completions first — the single-output
    /// entry point. `fill` writes the back buffer in XRGB8888 given the mapped slice and row pitch; `cursor` is consulted only when the hardware cursor is unavailable.
    pub fn present_into<F>(&mut self, cursor: Option<(f64, f64)>, fill: F)
    where
        F: FnOnce(&mut [u8], usize),
    {
        // Drain any retired completion so we can flip again.
        self.drain_flip_events();
        self.present_no_drain(cursor, fill);
    }

    /// Present without draining DRM events first: the multi-output backend drains the shared card fd
    /// once per frame and routes via [`Self::mark_flip_complete`] — draining per-scanout here would steal sibling outputs' events.
    pub fn present_no_drain<F>(&mut self, cursor: Option<(f64, f64)>, fill: F)
    where
        F: FnOnce(&mut [u8], usize),
    {
        if self.use_pageflip && self.flip_pending {
            // Previous flip not yet retired — drop this frame (natural vsync throttle); the next one supersedes it.
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
            // Software fallback only — the hardware plane composites itself and must not also be drawn into the frame.
            if !self.hw_cursor_active
                && let Some((px, py)) = cursor
            {
                draw_cursor(dst, pitch, dw, dh, px as i32, py as i32, self.cursor_size);
            }
        }

        if self.use_pageflip {
            // Best-effort CPU→host transfer before flipping: on virtual GPUs dirty_framebuffer flushes the
            // dumb buffer into the host resource (without it a bare flip presents unreceived bytes); no-op/unsupported on real GPUs.
            let clip = ClipRect::new(0, 0, self.width as u16, self.height as u16);
            let _ = self.card.dirty_framebuffer(self.fbs[idx], &[clip]);
            self.flip_to(idx);
        } else {
            self.flush_dirty(idx);
        }
    }

    /// Queue a page flip to buffer `idx`, making it the new front. A transient `EBUSY`/`EAGAIN` skips the frame; a "not supported" errno latches the page-flip path off, falling back to `dirty_framebuffer`.
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

    /// Flush the live buffer with `DRM_IOCTL_MODE_DIRTYFB` (how virtio-gpu transfers a CPU buffer to the host), falling back to a forced `set_crtc` where dirtyfb is unsupported.
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

    /// Non-blocking drain of DRM events, clearing `flip_pending` on completion; fd is `O_NONBLOCK` so
    /// `EAGAIN` means no events, not failure. Single-output only — multi-output routes by crtc via [`Self::poll_flip_completions`] + [`Self::mark_flip_complete`].
    fn drain_flip_events(&mut self) {
        if let Ok(events) = self.card.receive_events() {
            for ev in events {
                if let Event::PageFlip(_) = ev {
                    self.flip_pending = false;
                }
            }
        }
    }

    /// Drain pending DRM events, returning the crtc of each flip completion. Every per-connector
    /// scanout dups the same open file description, so completions for all crtcs arrive on any one fd — must drain **once** and route via [`Self::mark_flip_complete`], or the first scanout wedges its siblings' `flip_pending` forever.
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

    /// Clear `flip_pending` iff `crtc` is this scanout's crtc.
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

    /// Power the display for idle/lock blanking, via the legacy DPMS property (`0 = On`, `3 = Off`
    /// per `drm_mode.h`; virtio-gpu may ignore it) and a CRTC toggle (the reliable cross-driver path). Both best-effort; the backend skips [`Self::present`] while blanked.
    pub fn set_dpms(&mut self, on: bool) {
        self.set_dpms_property(on);
        if on {
            // Re-modeset the saved framebuffer + mode so the last frame is back on screen; the next `present` resumes normal blitting.
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

    /// Best-effort legacy DPMS toggle: a missing property (atomic-only drivers) or ioctl error is ignored — the CRTC toggle in [`Self::set_dpms`] is the load-bearing path.
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

/// One output's scanout plus the fields the runtime needs to intern a stable [`OutputId`](super::OutputId) and lay outputs out side-by-side.
pub struct ScanoutBuild {
    /// `dev_t` of the `cardN` node this output is on.
    pub device_id: u64,
    /// DRM connector handle (raw u32) for this output.
    pub connector_id: u32,
    /// Stable DRM connector name (`eDP-1`, `HDMI-A-1`) used by Settings.
    pub connector_name: String,
    /// Physical panel size in mm, when reported — the one input to scale derivation that resolution alone cannot supply (see [`super::scale`]).
    pub physical_mm: Option<(u32, u32)>,
    /// The crtc this output was modeset onto, so a later hotplug can pick one still free.
    pub crtc: crtc::Handle,
    /// Live, modeset scanout bound to this connector's crtc.
    pub scanout: DrmScanout,
}

/// Open `card_path` once and build one [`DrmScanout`] per connected connector with a usable mode,
/// each on a distinct crtc sharing a dup of the single card fd — required since libseat refuses a
/// second `open` of an already-open node, and it means every crtc's flip completions arrive on one
/// event stream, drained once and routed by crtc. A connector with no free crtc/mode is logged and
/// skipped; `Err` only when nothing came up. Primary card only — secondary-GPU connectors need their own fd and GPU-to-GPU buffer handling.
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

/// Pick a crtc for `conn_info` not already in `used`: the current encoder's crtc if free, else one
/// allowed by the connector's possible encoders (multi-head routing), else any free crtc. `None` when every crtc is taken.
fn pick_crtc(
    card: &Card,
    res: &drm_re::control::ResourceHandles,
    conn_info: &connector::Info,
    used: &[crtc::Handle],
) -> Option<crtc::Handle> {
    if let Some(c) = conn_info
        .current_encoder()
        .and_then(|e| card.get_encoder(e).ok())
        .and_then(|e| e.crtc())
        && !used.contains(&c)
    {
        return Some(c);
    }
    for &eh in conn_info.encoders() {
        if let Ok(enc) = card.get_encoder(eh) {
            for c in res.filter_crtcs(enc.possible_crtcs()) {
                if !used.contains(&c) {
                    return Some(c);
                }
            }
        }
    }
    res.crtcs().iter().copied().find(|c| !used.contains(c))
}

/// Choose a scanout mode: trust the first *reasonable* mode (>= 1024 wide), since the kernel lists
/// the EDID-preferred one first. The PREFERRED flag itself is not trusted — virtio-gpu flags a 640x480 default while advertising up to 5120x2160; fallback caps at 1920x1080.
fn pick_configured_mode(
    modes: &[Mode],
    requested: Option<settings_store::hardware::OutputMode>,
) -> Option<Mode> {
    for m in modes {
        let (w, h) = m.size();
        tracing::debug!(
            w,
            h,
            preferred = m.mode_type().contains(ModeTypeFlags::PREFERRED),
            "DRM scanout: candidate mode"
        );
    }
    if let Some(wanted) = requested {
        if let Some(found) = modes.iter().find(|m| {
            let (w, h) = m.size();
            u32::from(w) == wanted.width
                && u32::from(h) == wanted.height
                // Sysfs exposes geometry but not refresh. A zero refresh in
                // the typed contract therefore means "any advertised rate"
                // rather than inventing a value in Settings.
                && (wanted.refresh_mhz == 0 || mode_refresh_mhz(m) == wanted.refresh_mhz)
        }) {
            return Some(*found);
        }
        tracing::warn!(
            ?wanted,
            "configured DRM mode disappeared; retaining a safe advertised mode"
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
                    u32::from(w) * u32::from(h)
                })
        })
        .or_else(|| modes.first())
        .copied()
}

/// Classic left-pointing arrow cursor. `'#'` = black outline, `'.'` = white fill, space = transparent. The hot-spot is the top-left tip.
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

/// Paint the arrow into a 64×64 ARGB8888 cursor buffer (`[B, G, R, A]`, little-endian), transparent where blank; `pitch` is the row stride in bytes.
fn paint_cursor_argb(dst: &mut [u8], pitch: usize, cursor_size: u32) {
    for b in dst.iter_mut() {
        *b = 0;
    }
    for (row, line) in CURSOR.iter().enumerate() {
        for (col, ch) in line.bytes().enumerate() {
            let (r, g, b) = match ch {
                b'#' => (0u8, 0u8, 0u8),
                b'.' => (0xFFu8, 0xFFu8, 0xFFu8),
                _ => continue,
            };
            let scale = (cursor_size.clamp(24, CURSOR_DIM) as usize).div_ceil(24);
            for yy in row * scale..(row + 1) * scale {
                for xx in col * scale..(col + 1) * scale {
                    let o = yy * pitch + xx * 4;
                    if o + 4 <= dst.len() {
                        dst[o] = b;
                        dst[o + 1] = g;
                        dst[o + 2] = r;
                        dst[o + 3] = 0xFF;
                    }
                }
            }
        }
    }
}

/// Composite the software cursor into an XRGB8888 buffer at `(px, py)` (the hot-spot), little-endian `[B, G, R, X]`; used only when the hardware cursor plane is unavailable.
fn draw_cursor(
    dst: &mut [u8],
    pitch: usize,
    dw: usize,
    dh: usize,
    px: i32,
    py: i32,
    cursor_size: u32,
) {
    let scale = (cursor_size.clamp(24, CURSOR_DIM) as usize).div_ceil(24);
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
            for yy in 0..scale {
                for xx in 0..scale {
                    let x = px + (col * scale + xx) as i32;
                    let y = py + (row * scale + yy) as i32;
                    if x < 0 || y < 0 || x as usize >= dw || y as usize >= dh {
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
        paint_cursor_argb(&mut buf, pitch, 24);

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
        draw_cursor(&mut dst, pitch, w, h, 4, 4, 24);
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
        draw_cursor(&mut dst, pitch, w, h, 18, 18, 24);
        draw_cursor(&mut dst, pitch, w, h, -3, -3, 24);
    }

    #[test]
    fn cursor_routing_single_output_is_global_coords() {
        // One output at the origin contains every in-bounds point, so routing always selects index 0.
        let rects = [(0, 0, 1920u32, 1080u32)];
        assert_eq!(output_index_at(&rects, 0, 0), Some(0));
        assert_eq!(output_index_at(&rects, 1919, 1079), Some(0));
        assert_eq!(output_index_at(&rects, 1920, 0), None);
    }

    #[test]
    fn cursor_routing_picks_output_under_pointer() {
        // Two 1920x1080 outputs side by side: [0,1920) -> output 0, [1920,3840) -> output 1 — decides which crtc's cursor plane a pointer move drives.
        let rects = [(0, 0, 1920u32, 1080u32), (1920, 0, 1920u32, 1080u32)];
        assert_eq!(output_index_at(&rects, 10, 10), Some(0));
        assert_eq!(output_index_at(&rects, 1919, 500), Some(0));
        assert_eq!(output_index_at(&rects, 1920, 500), Some(1));
        assert_eq!(output_index_at(&rects, 3000, 500), Some(1));
        assert_eq!(output_index_at(&rects, 3840, 500), None);
    }

    #[test]
    fn cursor_routing_gap_between_mismatched_heights_is_none() {
        // A shorter second output leaves a gap below it; a point in that gap belongs to no output and must not panic / mis-route.
        let rects = [(0, 0, 1920u32, 1080u32), (1920, 0, 1280u32, 720u32)];
        assert_eq!(output_index_at(&rects, 2000, 700), Some(1));
        assert_eq!(output_index_at(&rects, 2000, 800), None);
    }

    #[test]
    fn cursor_coordinates_follow_output_rotation() {
        use settings_store::hardware::OutputTransform as T;
        // Portrait logical 1080×1920 on a physical 1920×1080 panel.
        assert_eq!(
            logical_to_physical_point(0, 0, 1080, 1920, T::Rotate90),
            (1919, 0)
        );
        assert_eq!(
            logical_to_physical_point(1079, 1919, 1080, 1920, T::Rotate90),
            (0, 1079)
        );
        assert_eq!(
            logical_to_physical_point(0, 0, 1080, 1920, T::Rotate270),
            (0, 1079)
        );
    }
}
