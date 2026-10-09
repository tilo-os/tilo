//! DRM scanout — modeset a connector and present GPU-rendered frames.
//!
//! KMS hardware cursor plane (`set_cursor2`/`move_cursor`, software cursor as fallback) plus
//! two buffer rings per output, each ping-ponged via `page_flip`:
//!
//! - **Direct** ([`DirectScanoutTargets`]): two GBM buffers the size of the output, imported as
//!   Vulkan render targets. Vello renders the output's part of the scene straight into the back
//!   slot ([`super::paint::VelloDrmBridge::paint_output_until`]); nothing crosses the CPU. Each
//!   output is judged alone (`direct_scanout_rejection` in `drm/mod.rs`): a normal transform, no
//!   ICC profile, a hardware cursor plane, page-flip events, and the renderer on this card.
//! - **Readback**: two dumb buffers allocated at modeset, falling back to
//!   `dirty_framebuffer/set_crtc` without page flips. The output's pixels are read back from
//!   the GPU and written on the CPU (rotated, colour-managed, swizzled from vello's
//!   `Rgba8Unorm` to `Xrgb8888`). Only outputs that fail a direct check, or whose direct
//!   frame failed, present this way.
//!
//! smithay's `DrmCompositor` drives GLES, not vello, so neither ring uses it.
//!
//! HDR: an output driven in HDR scans out `XBGR2101010` (or `ABGR16161616F`) buffers holding a
//! PQ BT.2020 signal, with the connector's HDR properties set by [`super::output_signal`]. The
//! direct path renders into imported GBM buffers; the readback path swaps in a second pair of
//! `XBGR2101010` dumb buffers and encodes on the CPU. Any refusal falls back to 8-bit SDR.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use smithay::backend::session::Session;
use smithay::reexports::drm as drm_re;
use smithay::reexports::rustix::fs::OFlags;

use drm_re::Device as BasicDevice;
use drm_re::buffer::{Buffer as _, DrmFourcc, DrmModifier, PlanarBuffer};
use drm_re::control::dumbbuffer::DumbBuffer;
use drm_re::control::{
    ClipRect, Device as ControlDevice, Event, FbCmd2Flags, Mode, ModeTypeFlags, PageFlipFlags,
    connector, crtc, framebuffer,
};
use smithay::backend::allocator::gbm::{GbmBufferFlags, GbmDevice};

use super::paint::{BridgeError, SceneDamage, VelloDrmBridge};
use super::xrgb_history::{
    XrgbHistoryCommit, XrgbHistoryIdentity, XrgbTargetHistory, XrgbWritePlan,
};
use crate::compositor::backend::OutputId;
use crate::compositor::cursor::{self, CursorBitmap, CursorShape, PointerImage};
use vello::{FineDamageRect, Scene};

use super::output_signal::{
    AdaptiveSync, HdrRequest, HdrSignal, OutputSignalStatus, adaptive_sync_capability,
    device_has_adaptive_sync, find_property, hdr_capability,
};
use super::outputs::DrmOutputKey;
use crate::shell::color_pipeline::SdrToPqEncoder;
use settings_store::hardware::HdrUnavailable;

/// 64×64 is the universally-supported legacy cursor size (virtio-gpu, i915 and amdgpu all advertise it).
/// Smallest cursor plane every driver offers; the plane actually used is the
/// driver's preferred size (`DRM_CAP_CURSOR_WIDTH`), up to
/// [`cursor::MAX_PIXEL_SIZE`].
const CURSOR_DIM: u32 = cursor::PLANE_SIZE;

/// The pixel formats an output's scanout buffers can use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanoutFormat {
    /// 8-bit SDR, the default.
    Xrgb8888,
    /// 10-bit PQ BT.2020, the preferred HDR format: every HDR-capable
    /// display engine scans it out and it costs no more memory than SDR.
    Xbgr2101010,
    /// Half-float, used for HDR only when a 10-bit buffer cannot be made.
    Abgr16161616f,
}

impl ScanoutFormat {
    /// HDR formats in order of preference.
    pub const HDR_PREFERENCE: [Self; 2] = [Self::Xbgr2101010, Self::Abgr16161616f];

    pub fn fourcc(self) -> DrmFourcc {
        match self {
            Self::Xrgb8888 => DrmFourcc::Xrgb8888,
            Self::Xbgr2101010 => DrmFourcc::Xbgr2101010,
            Self::Abgr16161616f => DrmFourcc::Abgr16161616f,
        }
    }

    pub fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::Xrgb8888 | Self::Xbgr2101010 => 4,
            Self::Abgr16161616f => 8,
        }
    }

    pub fn is_hdr(self) -> bool {
        !matches!(self, Self::Xrgb8888)
    }
}

/// Whether a connected DRM connector is a display sink. `DisplayPort` Alt Mode
/// over USB-C is reported by KMS as [`connector::Interface::DisplayPort`],
/// while USB display adapters may use [`connector::Interface::USB`]; both are
/// ordinary scanout targets. Only DRM writeback is an internal capture sink.
fn is_display_connector(interface: connector::Interface) -> bool {
    !matches!(
        interface,
        connector::Interface::Unknown | connector::Interface::Writeback
    )
}

/// Outcome of [`Card::reclaim_master`].
#[derive(Debug)]
enum MasterReclaim {
    /// The seat manager had already granted master; nothing was asked.
    AlreadyHeld,
    /// The fd was not master and `DRM_IOCTL_SET_MASTER` succeeded.
    Acquired,
    /// The fd is not master and the kernel refused to make it so.
    NotGranted(std::io::Error),
}

/// Decide how to end up holding DRM master, given whether the fd already
/// holds it. `acquire` runs only when the probe could not confirm master,
/// so an fd the seat manager already made master never sees a
/// `SET_MASTER` it is not permitted to issue.
fn reclaim_master(
    probe: std::io::Result<bool>,
    acquire: impl FnOnce() -> std::io::Result<()>,
) -> MasterReclaim {
    match probe {
        Ok(true) => return MasterReclaim::AlreadyHeld,
        Ok(false) => {}
        Err(e) => tracing::debug!("DRM scanout: could not probe DRM master: {e}"),
    }
    match acquire() {
        Ok(()) => MasterReclaim::Acquired,
        Err(e) => MasterReclaim::NotGranted(e),
    }
}

/// Whether `fd` currently holds DRM master, asked the way libdrm's
/// `drmIsMaster` does. `DRM_IOCTL_AUTH_MAGIC` is a master-only ioctl: the
/// kernel answers a non-master with `EACCES` before it reads the token,
/// and a master with `EINVAL` because token 0 is never issued.
fn fd_is_drm_master(fd: BorrowedFd<'_>) -> std::io::Result<bool> {
    // `_IOW('d', 0x11, struct drm_auth { __u32 magic; })`, identical on every Linux arch.
    const DRM_IOCTL_AUTH_MAGIC: libc::Ioctl = 0x4004_6411;
    let mut auth: u32 = 0;
    // SAFETY: `fd` is a live borrowed descriptor and `auth` is a valid,
    // writable `struct drm_auth` for the duration of the call.
    let rc = unsafe {
        libc::ioctl(
            fd.as_raw_fd(),
            DRM_IOCTL_AUTH_MAGIC,
            std::ptr::from_mut(&mut auth),
        )
    };
    if rc == 0 {
        return Ok(true);
    }
    let err = std::io::Error::last_os_error();
    match err.raw_os_error() {
        Some(libc::EINVAL) => Ok(true),
        Some(libc::EACCES) => Ok(false),
        _ => Err(err),
    }
}

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

    /// Make sure this fd holds DRM master, asking the kernel for it only
    /// when it does not already.
    ///
    /// The seat manager owns master, not the compositor: logind opens the
    /// node in its own process and sets master before `TakeDevice`/
    /// `ResumeDevice` hand it over. The kernel then refuses
    /// `DRM_IOCTL_SET_MASTER` from any other process without
    /// `CAP_SYS_ADMIN`, with `EACCES` and even when the fd already is
    /// master. Probing first keeps an unprivileged session from issuing
    /// that doomed ioctl after every suspend and VT switch; the fallback
    /// remains for seat backends that open the node in this process.
    fn reclaim_master(&self) -> MasterReclaim {
        reclaim_master(fd_is_drm_master(self.as_fd()), || {
            self.acquire_master_lock()
        })
    }

    /// `dev_t` of the underlying `cardN` node for stable per-connector `OutputId`s; best-effort (`0` on `fstat` failure — connector id still disambiguates).
    fn dev_id(&self) -> u64 {
        match smithay::reexports::rustix::fs::fstat(&self.0) {
            Ok(st) => device_id_from_dev_t(st.st_rdev),
            Err(e) => {
                tracing::debug!("DRM scanout: fstat for dev_id failed: {e}");
                0
            }
        }
    }
}

/// Two compositor-owned GPU allocations that can be both rendered into and
/// scanned out.  This is constructed as a capability probe before the Vulkan
/// aliases exist; ordinary presentation continues using dumb buffers until
/// the complete direct path is ready.
struct GbmScanoutBuffers {
    device: GbmDevice<Card>,
    buffers: [gbm::BufferObject<()>; 2],
    dma_fds: [OwnedFd; 2],
    fbs: [framebuffer::Handle; 2],
}

/// Immutable description borrowed by the Vulkan importer.  Ownership stays in
/// `GbmScanoutBuffers`, which outlives every alias and KMS flip.
pub struct GbmScanoutSlot<'a> {
    pub fd: BorrowedFd<'a>,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub offset: u32,
    pub modifier: u64,
    pub framebuffer: framebuffer::Handle,
}

impl GbmScanoutBuffers {
    fn new(card: Card, width: u32, height: u32, format: ScanoutFormat) -> Result<Self, String> {
        let device = GbmDevice::new(card).map_err(|error| format!("gbm_create_device: {error}"))?;
        let usage = GbmBufferFlags::SCANOUT | GbmBufferFlags::RENDERING;
        let implicit_linear_usage = usage | GbmBufferFlags::LINEAR;
        let fourcc = format.fourcc();
        if !device.is_format_supported(fourcc, implicit_linear_usage) {
            return Err(format!(
                "GBM does not support linear {fourcc:?} scanout+rendering"
            ));
        }
        let allocate = |index: usize| {
            // With an explicit modifier, the modifier list owns the layout
            // contract. Passing GBM_BO_USE_LINEAR as well is redundant and
            // rejected with EINVAL by Mesa/AMD. Prefer the explicit-modifier
            // API, then support older GBM stacks through the implicit linear
            // allocator. The common validator below still rejects anything
            // tiled or otherwise unsuitable before Vulkan/KMS sees it.
            match device.create_buffer_object_with_modifiers2::<()>(
                width,
                height,
                fourcc,
                std::iter::once(gbm::Modifier::Linear),
                usage,
            ) {
                Ok(buffer) => Ok(buffer),
                Err(explicit_error) => device
                    .create_buffer_object::<()>(width, height, fourcc, implicit_linear_usage)
                    .map_err(|implicit_error| {
                        format!(
                            "GBM scanout buffer[{index}]: explicit linear allocation: \
                             {explicit_error}; implicit linear allocation: {implicit_error}"
                        )
                    }),
            }
        };
        let buffers = [allocate(0)?, allocate(1)?];
        for (index, buffer) in buffers.iter().enumerate() {
            validate_gbm_scanout_buffer(buffer, width, height, format)
                .map_err(|error| format!("GBM scanout buffer[{index}]: {error}"))?;
        }
        let dma_fds = [
            buffers[0]
                .fd_for_plane(0)
                .map_err(|error| format!("export GBM scanout buffer[0]: {error}"))?,
            buffers[1]
                .fd_for_plane(0)
                .map_err(|error| format!("export GBM scanout buffer[1]: {error}"))?,
        ];
        let fb0 = device
            .add_planar_framebuffer(&buffers[0], FbCmd2Flags::MODIFIERS)
            .map_err(|error| format!("add GBM framebuffer[0]: {error}"))?;
        let fb1 = match device.add_planar_framebuffer(&buffers[1], FbCmd2Flags::MODIFIERS) {
            Ok(fb) => fb,
            Err(error) => {
                let _ = device.destroy_framebuffer(fb0);
                return Err(format!("add GBM framebuffer[1]: {error}"));
            }
        };
        let fbs = [fb0, fb1];
        Ok(Self {
            device,
            buffers,
            dma_fds,
            fbs,
        })
    }

    fn slot(&self, index: usize) -> Option<GbmScanoutSlot<'_>> {
        let buffer = self.buffers.get(index)?;
        let fd = self.dma_fds.get(index)?.as_fd();
        let framebuffer = *self.fbs.get(index)?;
        Some(GbmScanoutSlot {
            fd,
            width: buffer.width(),
            height: buffer.height(),
            stride: buffer.stride_for_plane(0),
            offset: buffer.offset(0),
            modifier: u64::from(buffer.modifier()),
            framebuffer,
        })
    }
}

impl Drop for GbmScanoutBuffers {
    fn drop(&mut self) {
        for fb in self.fbs {
            if let Err(error) = self.device.destroy_framebuffer(fb) {
                tracing::debug!(?fb, %error, "failed to destroy GBM framebuffer during teardown");
            }
        }
    }
}

/// Ownership of two renderer/KMS-shared scanout images.  `front` is the image
/// the CRTC is known to be reading; `pending` is owned by KMS until its page
/// flip event arrives.  Consequently there is no renderable image while a
/// flip is pending on a two-slot swapchain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct DirectScanoutState {
    front: usize,
    pending: Option<usize>,
}

/// Imported Vulkan aliases must drop before their GBM allocation and KMS
/// framebuffer IDs.  Field order makes that teardown relationship explicit.
pub struct DirectScanoutTargets {
    imported: [ui::dmabuf_import::ImportedRenderTarget; 2],
    buffers: GbmScanoutBuffers,
    format: ScanoutFormat,
    /// The CRTC was modeset onto one of these buffers. A legacy page flip
    /// cannot change the framebuffer format, so HDR targets become visible
    /// through one `set_crtc` before flips take over.
    crtc_bound: bool,
    /// KMS accepted a frame rendered into one of these targets; until then the
    /// CRTC still shows the dumb front.
    presented: bool,
    state: DirectScanoutState,
    /// When the pending flip was queued. An output whose flip nothing waits for (it is
    /// slower than the output pacing the frames) is demoted once that flip is overdue.
    flip_queued_at: Option<Instant>,
}

/// Result of handing a completed imported target to KMS.
#[derive(Debug)]
pub enum DirectSubmit {
    Queued,
    /// Shown by a modeset rather than a flip: no page-flip event follows.
    Presented,
    Busy,
    Failed(String),
    /// The first HDR frame could not be shown; the output must return to SDR.
    HdrFailed(HdrUnavailable, String),
}

/// A successfully queued direct flip is not retired when the ioctl returns:
/// KMS still owns both double-buffered targets until its page-flip event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectRetirement {
    Complete,
    TimedOut,
}

fn retirement_after_poll(
    awaited: crtc::Handle,
    completed: &[crtc::Handle],
    deadline_reached: bool,
) -> Option<DirectRetirement> {
    if completed.contains(&awaited) {
        Some(DirectRetirement::Complete)
    } else if deadline_reached {
        Some(DirectRetirement::TimedOut)
    } else {
        None
    }
}

impl DirectScanoutTargets {
    /// Build both targets transactionally, the size of the output they show
    /// (`output_size`, its extent in the scene). No CRTC state changes here.
    ///
    /// # Errors
    ///
    /// Fails when the output's extent is not the scanout mode, GBM cannot
    /// allocate the scanout buffers, or the renderer cannot import either of
    /// them.
    pub fn new(
        scanout: &DrmScanout,
        bridge: &VelloDrmBridge,
        format: ScanoutFormat,
        output_size: (u32, u32),
    ) -> Result<Self, String> {
        let mode = scanout.size();
        if mode != output_size {
            return Err(format!(
                "scanout mode {}x{} is not the output's {}x{} extent",
                mode.0, mode.1, output_size.0, output_size.1
            ));
        }
        let buffers = scanout.allocate_gbm_scanout_buffers(format)?;
        let fourcc = match format {
            ScanoutFormat::Xrgb8888 => media::dmabuf::DRM_FORMAT_XRGB8888,
            ScanoutFormat::Xbgr2101010 => media::dmabuf::DRM_FORMAT_XBGR2101010,
            ScanoutFormat::Abgr16161616f => media::dmabuf::DRM_FORMAT_ABGR16161616F,
        };
        let import = |index: usize| -> Result<ui::dmabuf_import::ImportedRenderTarget, String> {
            let slot = buffers
                .slot(index)
                .ok_or_else(|| format!("GBM scanout slot[{index}] is unavailable"))?;
            let plane = ui::dmabuf_import::PlaneRef {
                fd: slot.fd,
                offset: slot.offset,
                stride: slot.stride,
            };
            bridge
                .import_scanout_target(&ui::dmabuf_import::DmabufRef {
                    planes: &[plane],
                    fourcc,
                    modifier: slot.modifier,
                    width: slot.width,
                    height: slot.height,
                    colorimetry: media::dmabuf::Colorimetry::RGB,
                })
                .map_err(|error| format!("import GBM scanout slot[{index}]: {error}"))
        };
        let imported = [import(0)?, import(1)?];
        Ok(Self {
            imported,
            buffers,
            format,
            crtc_bound: false,
            presented: false,
            // The dumb-buffer front remains visible until the first complete
            // imported target is submitted, so index zero is merely the
            // initial ownership anchor; the first render uses index one.
            state: DirectScanoutState::new(0),
            flip_queued_at: None,
        })
    }

    pub fn format(&self) -> ScanoutFormat {
        self.format
    }

    /// A queued flip onto one of these targets has not retired yet.
    pub fn flip_pending(&self) -> bool {
        self.state.pending.is_some()
    }

    /// When the pending flip was queued, while one is.
    pub fn flip_queued_at(&self) -> Option<Instant> {
        self.state.pending.and(self.flip_queued_at)
    }

    /// KMS has accepted a frame from these targets, so the CRTC no longer shows
    /// the dumb front.
    pub fn presented(&self) -> bool {
        self.presented
    }

    pub fn render_slot(&self) -> Option<(usize, &ui::dmabuf_import::ImportedRenderTarget)> {
        let index = self.state.render_slot()?;
        self.imported.get(index).map(|target| (index, target))
    }

    fn framebuffer(&self, index: usize) -> Option<framebuffer::Handle> {
        self.buffers.slot(index).map(|slot| slot.framebuffer)
    }
}

impl DirectScanoutState {
    pub(super) const fn new(front: usize) -> Self {
        Self {
            front,
            pending: None,
        }
    }

    /// Reserve the only slot which is not currently owned by scanout.  The
    /// caller must render successfully before handing it to [`Self::submit`].
    pub(super) fn render_slot(self) -> Option<usize> {
        self.pending
            .is_none()
            .then_some(1usize.saturating_sub(self.front))
    }

    /// Transfer a successfully rendered slot to KMS.  This records ownership
    /// only after `page_flip` itself succeeds; a rejected flip remains safely
    /// renderable on the next attempt.
    pub(super) fn submit(&mut self, slot: usize) -> bool {
        if self.render_slot() != Some(slot) {
            return false;
        }
        self.pending = Some(slot);
        true
    }

    /// A modeset showed `slot` directly: it is the front at once, with no
    /// flip event to wait for.
    fn present_immediately(&mut self, slot: usize) -> bool {
        if self.render_slot() != Some(slot) {
            return false;
        }
        self.front = slot;
        true
    }

    /// Retire the queued flip.  Only now is the old front released back to the
    /// renderer and the queued target promoted to the visible front.
    pub(super) fn complete(&mut self) -> bool {
        let Some(front) = self.pending.take() else {
            return false;
        };
        self.front = front;
        true
    }
}

fn validate_gbm_scanout_buffer(
    buffer: &gbm::BufferObject<()>,
    width: u32,
    height: u32,
    format: ScanoutFormat,
) -> Result<(), String> {
    if buffer.width() != width || buffer.height() != height {
        return Err(format!(
            "extent {}x{} does not match requested {width}x{height}",
            buffer.width(),
            buffer.height()
        ));
    }
    if buffer.format() != format.fourcc() {
        return Err(format!(
            "format {:?} is not {:?}",
            buffer.format(),
            format.fourcc()
        ));
    }
    if buffer.plane_count() != 1 || buffer.offset(0) != 0 {
        return Err(format!(
            "expected one plane at offset zero, got {} plane(s), offset {}",
            buffer.plane_count(),
            buffer.offset(0)
        ));
    }
    let minimum_stride = width
        .checked_mul(format.bytes_per_pixel())
        .ok_or_else(|| "row size overflows u32".to_owned())?;
    if buffer.stride_for_plane(0) < minimum_stride {
        return Err(format!(
            "stride {} is shorter than {minimum_stride}",
            buffer.stride_for_plane(0)
        ));
    }
    if buffer.modifier() != gbm::Modifier::Linear {
        return Err(format!(
            "modifier {:?} is not explicit linear layout",
            buffer.modifier()
        ));
    }
    Ok(())
}

/// Convert the kernel's `dev_t` into the identifier used by the DRM output
/// registry and card map. Smithay's `UdevEvent::{Added, Removed}` carries this
/// exact `dev_t`, while `fstat(card_fd).st_rdev` supplies the value for an
/// already-open [`CardHandle`]. Keeping the conversion here makes those two
/// independently-produced identities deliberately identical.
pub(crate) fn device_id_from_dev_t(device: libc::dev_t) -> u64 {
    device
}

fn valid_config_identity(identity: &str) -> bool {
    !identity.is_empty()
        && identity
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'.' | b'-' | b'_'))
}

/// The last component of `/sys/class/drm/cardN/device` is normally a PCI BDF
/// (and a stable physical USB/platform address for non-PCI devices). It is a
/// stronger fallback than the kernel-assigned `dev_t` when DRM lacks a bus id,
/// and can be queried before opening a re-enumerated card node through libseat.
pub(crate) fn physical_card_identity(card_path: &Path) -> Option<String> {
    let node = card_path.file_name()?.to_str()?;
    let canonical =
        std::fs::canonicalize(Path::new("/sys/class/drm").join(node).join("device")).ok()?;
    let component = canonical.file_name()?.to_str()?;
    let identity = format!("sysfs-{component}");
    valid_config_identity(&identity).then_some(identity)
}

/// Exact PCI identity for a DRM card node, derived from the canonical sysfs device path when
/// `drmGetBusid` is unavailable (notably amdgpu through some seat/session fd stacks). The final
/// component must itself be a complete PCI BDF; platform/USB identities never become an unsafe
/// same-GPU claim.
fn sysfs_pci_bus_id_at(
    sys_dev_char: &Path,
    device: smithay::reexports::rustix::fs::Dev,
) -> Option<String> {
    let major = smithay::reexports::rustix::fs::major(device);
    let minor = smithay::reexports::rustix::fs::minor(device);
    let canonical =
        std::fs::canonicalize(sys_dev_char.join(format!("{major}:{minor}")).join("device")).ok()?;
    let component = canonical.file_name()?.to_str()?;
    parse_pci_bdf(component).map(|_| format!("pci:{component}"))
}

fn sysfs_pci_bus_id_for_dev(device: smithay::reexports::rustix::fs::Dev) -> Option<String> {
    sysfs_pci_bus_id_at(Path::new("/sys/dev/char"), device)
}

fn card_pci_bus_id(card: &Card) -> Option<String> {
    card.get_bus_id()
        .ok()
        .and_then(|identity| identity.into_string().ok())
        .filter(|identity| parse_pci_bdf(identity).is_some())
        .or_else(|| {
            smithay::reexports::rustix::fs::fstat(&card.0)
                .ok()
                .and_then(|stat| sysfs_pci_bus_id_for_dev(stat.st_rdev))
        })
}

fn same_physical_card_identity(existing: Option<&str>, candidate: Option<&str>) -> bool {
    matches!((existing, candidate), (Some(existing), Some(candidate)) if existing == candidate)
}

/// Canonical PCI address shared by Vulkan's `VK_EXT_pci_bus_info` identity
/// and DRM's `pci:dddd:bb:ss.f` bus id. Direct DMA-BUF scanout must fail
/// closed when either side cannot prove this identity: importing memory from
/// an unrelated GPU may succeed initially but is not a valid scanout path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PciBdf {
    domain: u16,
    bus: u8,
    device: u8,
    function: u8,
}

fn parse_pci_bdf(identity: &str) -> Option<PciBdf> {
    let value = identity.strip_prefix("pci:").unwrap_or(identity);
    let (domain, remainder) = value.split_once(':')?;
    let (bus, slot_function) = remainder.split_once(':')?;
    let (device, function) = slot_function.split_once('.')?;
    if domain.len() != 4
        || bus.len() != 2
        || device.len() != 2
        || function.len() != 1
        || function.as_bytes().first().is_some_and(|byte| *byte > b'7')
    {
        return None;
    }
    Some(PciBdf {
        domain: u16::from_str_radix(domain, 16).ok()?,
        bus: u8::from_str_radix(bus, 16).ok()?,
        device: u8::from_str_radix(device, 16).ok()?,
        function: u8::from_str_radix(function, 16).ok()?,
    })
}

/// True only when the renderer and KMS card positively identify the same PCI
/// function. Kept pure so the safety boundary is testable without a GPU.
pub(crate) fn renderer_matches_drm_card(renderer_pci_bus_id: &str, drm_bus_id: &str) -> bool {
    matches!(
        (parse_pci_bdf(renderer_pci_bus_id), parse_pci_bdf(drm_bus_id)),
        (Some(renderer), Some(drm)) if renderer == drm
    )
}

pub(crate) fn renderer_drm_node_matches_card(
    renderer: Option<ui::dmabuf_import::VulkanDrmDeviceIdentity>,
    card_device: u64,
) -> bool {
    let Some(renderer) = renderer else {
        return false;
    };
    [renderer.primary, renderer.render]
        .into_iter()
        .flatten()
        .any(|(major, minor)| smithay::reexports::rustix::fs::makedev(major, minor) == card_device)
}

#[cfg(test)]
mod renderer_drm_identity_tests {
    use super::*;

    #[test]
    fn matches_either_vulkan_drm_node_and_rejects_other_nodes() {
        let primary = smithay::reexports::rustix::fs::makedev(226, 0);
        let identity = ui::dmabuf_import::VulkanDrmDeviceIdentity {
            primary: Some((226, 0)),
            render: Some((226, 128)),
        };
        assert!(renderer_drm_node_matches_card(Some(identity), primary));
        assert!(!renderer_drm_node_matches_card(
            Some(identity),
            smithay::reexports::rustix::fs::makedev(226, 1)
        ));
        assert!(!renderer_drm_node_matches_card(None, primary));
    }
}

/// `dev_t` is the final fallback only: it identifies one active attachment,
/// but is not promised to survive a reboot or eGPU re-enumeration.
fn config_identity(
    drm_bus_id: Option<String>,
    sysfs_identity: Option<String>,
    device_id: u64,
) -> String {
    drm_bus_id
        .filter(|identity| valid_config_identity(identity))
        .or(sysfs_identity)
        .unwrap_or_else(|| format!("drm-dev-{device_id}"))
}

/// The loop thread's own handle on one KMS card (held by the session-owning thread, not the
/// render thread): drops/retakes DRM master across a VT switch — releasing here releases for the
/// scanouts on this card too, since every dup shares one open file description — and builds
/// scanouts for new connectors on hotplug. The runtime retains one handle per usable card; each
/// must outlive its scanouts because libseat refuses a second open of an already-open node.
pub struct CardHandle {
    card: Card,
    device_id: u64,
    card_path: PathBuf,
    /// Stable DRM bus id captured from the open card. It is used to qualify
    /// a connector only when another card exposes that same connector label.
    config_identity: String,
    /// Sysfs identity retained separately from the persistence key, so a
    /// future `cardN` path can be recognised as this physical GPU before a
    /// second libseat open is attempted.
    physical_identity: Option<String>,
    pci_bus_id: Option<String>,
}

/// One connected connector as the hotplug path sees it: enough to decide whether it is new, gone, or re-moded, and to modeset it if new.
///
/// `Clone` because the runtime keeps the last forced probe's snapshots, so
/// a settings change re-applies the arrangement without probing again.
#[derive(Clone)]
pub struct ConnectorSnapshot {
    pub connector_id: u32,
    pub handle: connector::Handle,
    pub mode: Mode,
    /// The live KMS mode table. Configuration is selected only after the
    /// runtime has compared connectors on every card.
    modes: Vec<Mode>,
    /// Raw KMS connector label, e.g. `HDMI-A-1`.
    pub connector_name: String,
    /// Collision-resolved persisted configuration key. It is raw when the
    /// label is unique, and card-qualified only when it collides.
    pub config_key: String,
    /// Disabled connectors remain in capability snapshots but are not
    /// brought up for scanout.
    pub enabled: bool,
    /// Physical panel size in millimetres, when the connector reports one. `None` for virtual connectors, which report 0×0.
    pub physical_mm: Option<(u32, u32)>,
    /// What the sink's EDID says it is; `None` when it exposes none.
    pub monitor: Option<settings_store::hardware::MonitorIdentity>,
    /// The mode the display configuration asks for, if any. Remembered with
    /// a lit output so a later rescan can tell a changed request from an
    /// unchanged one (see [`super::hotplug::retained_mode`]).
    pub mode_intent: Option<settings_store::hardware::OutputMode>,
}

impl ConnectorSnapshot {
    /// Every mode the connector advertises, in the kernel's order.
    pub fn advertised_modes(&self) -> Vec<settings_store::hardware::OutputMode> {
        self.modes.iter().map(output_mode).collect()
    }

    /// Drive the connector at `wanted` rather than the mode configuration
    /// picked, when the connector still advertises exactly that mode.
    pub fn retain_mode(&mut self, wanted: settings_store::hardware::OutputMode) -> bool {
        let Some(found) = self.modes.iter().find(|m| output_mode(m) == wanted) else {
            return false;
        };
        self.mode = *found;
        true
    }
}

/// A KMS mode as the display configuration names it.
fn output_mode(mode: &Mode) -> settings_store::hardware::OutputMode {
    let (width, height) = mode.size();
    settings_store::hardware::OutputMode {
        width: u32::from(width),
        height: u32::from(height),
        refresh_mhz: mode_refresh_mhz(mode),
    }
}

/// A connector before the whole-seat topology resolves its persisted key.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ConnectorKeyCandidate<'a> {
    pub key: DrmOutputKey,
    pub connector_name: &'a str,
    pub card_identity: &'a str,
}

/// Keep legacy connector keys for labels which have always been unique, but
/// qualify duplicates with the physical DRM card's bus id. Once a user has
/// saved a qualified key, retain it while that physical connector is briefly
/// unique after a peer unplug; otherwise its confirmed mode/layout would be
/// lost until the peer returns. An old raw key never applies to two cards at
/// once. If a driver exposes neither a DRM bus id nor a usable sysfs device
/// path, its `dev_t` qualifier is unique only for this session and cannot
/// promise cross-boot persistence.
pub(crate) fn resolve_connector_config_keys<'a>(
    candidates: impl IntoIterator<Item = ConnectorKeyCandidate<'a>>,
    config: &settings_store::hardware::DisplayConfig,
) -> BTreeMap<DrmOutputKey, String> {
    let candidates: Vec<_> = candidates.into_iter().collect();
    let mut label_counts = BTreeMap::<&str, usize>::new();
    for candidate in &candidates {
        let count = label_counts.entry(candidate.connector_name).or_default();
        *count = count.saturating_add(1);
    }
    candidates
        .into_iter()
        .map(|candidate| {
            let qualified = format!("{}/{}", candidate.card_identity, candidate.connector_name);
            let key = if label_counts.get(candidate.connector_name).copied() == Some(1)
                && config.output_for(&qualified).is_none()
            {
                candidate.connector_name.to_owned()
            } else {
                qualified
            };
            (candidate.key, key)
        })
        .collect()
}

/// Whether a connector drives a panel built into the machine (a laptop or
/// tablet screen) rather than a monitor plugged into a port.
pub(crate) fn is_builtin_connector(connector_name: &str) -> bool {
    let upper = connector_name.to_ascii_uppercase();
    upper.starts_with("EDP") || upper.starts_with("LVDS") || upper.starts_with("DSI")
}

fn connector_is_scanout_eligible(
    state: connector::State,
    interface: connector::Interface,
    has_usable_mode: bool,
) -> bool {
    // `Virtual` is a real scanout sink for virtual GPUs (including the
    // Parallels display used by Tilo). `Writeback` captures a CRTC rather than
    // displaying one, and `Unknown` supplies no typed evidence that it can be
    // used for scanout.
    state == connector::State::Connected && has_usable_mode && is_display_connector(interface)
}

impl CardHandle {
    /// Open `card_path` through the session and become DRM master.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the session fails to open `card_path` (e.g. libseat denies access or the
    /// node doesn't exist). Failing to *acquire* DRM master afterward is logged but not fatal
    /// here — a later `set_crtc` surfaces that as its own error instead.
    pub fn open<S: Session>(session: &mut S, card_path: &Path) -> Result<Self, String> {
        let card = DrmScanout::open_card(session, card_path)?;
        let device_id = card.dev_id();
        let drm_bus_id = card
            .get_bus_id()
            .ok()
            .and_then(|id| id.into_string().ok())
            .filter(|id| valid_config_identity(id));
        let physical_identity = physical_card_identity(card_path);
        let pci_bus_id = card_pci_bus_id(&card);
        let config_identity = config_identity(drm_bus_id, physical_identity.clone(), device_id);
        Ok(Self {
            card,
            device_id,
            card_path: card_path.to_path_buf(),
            config_identity,
            physical_identity,
            pci_bus_id,
        })
    }

    /// `dev_t` of the underlying node — the device half of an output's stable
    /// identity. This is normalized through `device_id_from_dev_t`, the
    /// same conversion the udev add/remove path uses.
    pub fn device_id(&self) -> u64 {
        self.device_id
    }

    /// The libseat-opened `cardN` path.  Used to deduplicate a udev `Added`
    /// edge before attempting a second open of the same node.
    pub fn path(&self) -> &Path {
        &self.card_path
    }

    /// Return the exact fd that [`Self::open`] obtained from [`Session::open`].
    ///
    /// A normal `OwnedFd` drop is not enough for libseat: its session keeps a
    /// `libseat::Device` keyed by this raw fd until [`Session::close`] is
    /// called.  The udev removal path consumes a handle through this method
    /// only after every presenter/cursor duplicate has been dropped.
    pub(crate) fn into_session_fd(self) -> OwnedFd {
        self.card.0
    }

    /// Persistent card portion of a collision-resolved output key.
    pub fn config_identity(&self) -> &str {
        &self.config_identity
    }

    /// Whether a prospective udev node is this physical card. A new `cardN`
    /// path and `dev_t` can still resolve to the same canonical sysfs device.
    pub fn same_physical_node(&self, path: &Path) -> bool {
        self.card_path == path
            || same_physical_card_identity(
                self.physical_identity.as_deref(),
                physical_card_identity(path).as_deref(),
            )
    }

    pub fn physical_identity(&self) -> Option<&str> {
        self.physical_identity.as_deref()
    }

    /// PCI bus identity reported by DRM for the exact card fd.  Direct
    /// dma-buf scanout uses this as a positive same-device proof; a missing or
    /// non-PCI identity deliberately leaves the established CPU path active.
    pub fn drm_bus_id(&self) -> Option<String> {
        self.pci_bus_id.clone()
    }

    /// Give up DRM master. Called before a VT switch completes: the incoming session cannot modeset until we have.
    pub fn drop_master(&self) {
        match self.card.release_master_lock() {
            Ok(()) => tracing::info!("DRM scanout: released DRM master"),
            // Not fatal — libseat may already have taken it away on our behalf, so the release can be redundant.
            Err(e) => tracing::debug!("DRM scanout: release_master_lock: {e}"),
        }
    }

    /// Make sure DRM master is held again. Every `set_crtc` after a resume
    /// needs it, so this runs before the modeset. The seat manager restores
    /// master itself before announcing the session active, and suspend never
    /// takes it away, so this is normally a probe that finds it held.
    pub fn acquire_master(&self) -> bool {
        match self.card.reclaim_master() {
            MasterReclaim::AlreadyHeld => {
                tracing::debug!("DRM scanout: DRM master held on resume");
                true
            }
            MasterReclaim::Acquired => {
                tracing::info!("DRM scanout: re-acquired DRM master");
                true
            }
            MasterReclaim::NotGranted(e) => {
                tracing::warn!(
                    "DRM scanout: session does not hold DRM master on resume and the seat \
                     manager has not granted it ({e}); set_crtc may EACCES"
                );
                false
            }
        }
    }

    /// Ask the hardware what is connected now; `force_probe` is on since a hotplug event is precisely when cached connector state is stale.
    ///
    /// A forced probe of every connector, plus an EDID read each, is slow
    /// (hundreds of milliseconds per DP-MST sink) and runs on the input
    /// thread. Only startup and udev hotplug call this; everything else
    /// reads the runtime's copy of the last result.
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
            let connector_name = format!("{}-{}", info.interface().as_str(), info.interface_id());
            let mode = pick_configured_mode(info.modes(), None);
            if !connector_is_scanout_eligible(info.state(), info.interface(), mode.is_some()) {
                continue;
            }
            let Some(mode) = mode else {
                continue;
            };
            let monitor = self.read_monitor_identity(&connector_name);
            out.push(ConnectorSnapshot {
                connector_id: u32::from(ch),
                handle: ch,
                mode,
                modes: info.modes().to_vec(),
                config_key: connector_name.clone(),
                connector_name,
                enabled: true,
                physical_mm: info.size().filter(|&(w, h)| w > 0 && h > 0),
                monitor,
                mode_intent: None,
            });
        }
        out
    }

    /// The connector's EDID identity, read from its sysfs node. The kernel
    /// refreshes that blob on every probe, so it describes the monitor on
    /// the port now rather than the one first seen there.
    fn read_monitor_identity(
        &self,
        connector_name: &str,
    ) -> Option<settings_store::hardware::MonitorIdentity> {
        let edid = std::fs::read(self.connector_sysfs(connector_name).join("edid")).ok()?;
        crate::wayland::edid::parse_monitor_identity(&edid)
    }

    /// The sink's HDR description, when its EDID advertises the PQ transfer
    /// Tilo signals. `None` for an SDR sink or an unreadable EDID.
    pub fn read_hdr_display_info(
        &self,
        connector_name: &str,
    ) -> Option<crate::wayland::edid::HdrDisplayInfo> {
        let edid = std::fs::read(self.connector_sysfs(connector_name).join("edid")).ok()?;
        let info = crate::wayland::edid::parse_hdr_display_info(&edid);
        info.supported_transfers
            .contains(&crate::collab::colorimetry::TransferCharacteristics::SmpteSt2084)
            .then_some(info)
    }

    /// `/sys/class/drm/<cardN>-<connector>`.
    pub fn connector_sysfs(&self, connector_name: &str) -> std::path::PathBuf {
        let card_name = self
            .card_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("card0");
        Path::new("/sys/class/drm").join(format!("{card_name}-{connector_name}"))
    }

    /// Apply the key selected for the complete multi-card topology.  This
    /// intentionally does not discard disabled connectors: Settings must
    /// still learn that they are present and may be turned back on.
    pub fn apply_output_config(
        snapshot: &mut ConnectorSnapshot,
        config_key: String,
        config: &settings_store::hardware::DisplayConfig,
    ) {
        let output_config = config.output_for(&config_key);
        snapshot.enabled = output_config.is_none_or(|output| output.enabled);
        snapshot.mode_intent = output_config.and_then(|output| output.mode);
        snapshot.mode = pick_configured_mode(&snapshot.modes, snapshot.mode_intent)
            .or_else(|| pick_configured_mode(&snapshot.modes, None))
            // `connected` has already rejected an empty KMS mode table; retaining
            // the old value here is defensive rather than inventing a mode.
            .unwrap_or(snapshot.mode);
        snapshot.config_key = config_key;
    }

    /// Describe every physically connected sink using the live KMS mode
    /// table. The result is published to Settings. Sink support alone is not
    /// an applicable compositor capability: HDR also needs the connector's
    /// HDR properties and adaptive sync the driver's CRTC property, and a
    /// feature the driver refused at runtime is reported unavailable, with
    /// the reason.
    pub fn display_capabilities(
        &self,
        connectors: &[ConnectorSnapshot],
        profiles: &BTreeMap<String, String>,
        signal_status: &BTreeMap<String, OutputSignalStatus>,
        scale_override: Option<f64>,
    ) -> Vec<settings_store::hardware::DisplayOutputSnapshot> {
        use settings_store::hardware::{DisplayOutputSnapshot, OutputDescriptor, OutputMode};

        let driver_adaptive_sync = device_has_adaptive_sync(&self.card);
        let mut outputs = Vec::new();
        for snapshot in connectors {
            let connector = &snapshot.connector_name;
            let modes: Vec<OutputMode> = snapshot
                .modes
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
            let (width, height) = snapshot.mode.size();
            let current_mode = OutputMode {
                width: u32::from(width),
                height: u32::from(height),
                refresh_mhz: mode_refresh_mhz(&snapshot.mode),
            };
            let sink_hdr = self.read_hdr_display_info(connector).is_some();
            let (hdr, hdr_unavailable_reason) = hdr_capability(
                sink_hdr,
                HdrSignal::probe(&self.card, snapshot.handle).missing(),
                signal_status.get(&snapshot.config_key),
            );
            // `vrr_capable` is a standard DRM connector property exported by
            // modern amdgpu/i915 kernels. Absence is unknown/not usable, never
            // interpreted as support.
            let sink_vrr = find_property(&self.card, snapshot.handle, "vrr_capable")
                .is_some_and(|(_, value, _)| value == 1);
            let (vrr, vrr_unavailable_reason) = adaptive_sync_capability(
                sink_vrr,
                driver_adaptive_sync,
                signal_status.get(&snapshot.config_key),
            );
            let builtin = is_builtin_connector(connector);
            outputs.push(DisplayOutputSnapshot {
                descriptor: OutputDescriptor {
                    // The configuration key becomes card-qualified only for
                    // a duplicate raw connector label. Settings receives the
                    // raw label separately for presentation.
                    connector: snapshot.config_key.clone(),
                    connector_label: connector.clone(),
                    modes,
                    hdr,
                    vrr,
                    color_profiles: profiles.keys().cloned().collect(),
                    monitor: snapshot.monitor.clone(),
                },
                current_mode,
                // What the compositor picks with no saved entry; the
                // runtime puts a live output's actual scale over it.
                scale: super::scale::resolve_scale(
                    (u32::from(width), u32::from(height)),
                    snapshot.physical_mm,
                    scale_override,
                ),
                builtin,
                sink_hdr,
                hdr_unavailable_reason,
                sink_vrr,
                vrr_unavailable_reason,
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
        match DrmScanout::from_card(
            card_dup,
            snapshot.handle,
            crtc,
            snapshot.mode,
            self.pci_bus_id.clone(),
        ) {
            Ok(scanout) => Some(ScanoutBuild {
                device_id: self.device_id,
                connector_id: snapshot.connector_id,
                connector_name: snapshot.connector_name.clone(),
                config_key: snapshot.config_key.clone(),
                physical_mm: snapshot.physical_mm,
                monitor: snapshot.monitor.clone(),
                hdr_display: self.read_hdr_display_info(&snapshot.connector_name),
                mode_intent: snapshot.mode_intent,
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
        let connected = self.connected();
        let set = settings_store::hardware::DisplaySet::of(connected.iter().map(|snapshot| {
            settings_store::hardware::AttachedOutput {
                connector: &snapshot.connector_name,
                connector_label: &snapshot.connector_name,
                monitor: snapshot.monitor.as_ref(),
            }
        }));
        let config = settings_store::hardware::DisplayConfig::load_effective(&set);
        let mut used: Vec<crtc::Handle> = Vec::new();
        let mut builds = Vec::new();
        for mut snapshot in connected {
            let config_key = snapshot.connector_name.clone();
            Self::apply_output_config(&mut snapshot, config_key, &config);
            if !snapshot.enabled {
                continue;
            }
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
    if let Some(data) = tilo_paths::xdg::data_home() {
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
    let denominator = u64::from(htotal).saturating_mul(u64::from(vtotal));
    if denominator == 0 {
        return mode.vrefresh().saturating_mul(1_000);
    }
    // Pixel clock is kHz. kHz * 1_000_000 / pixels-per-frame = mHz.
    let numerator = u64::from(mode.clock())
        .saturating_mul(1_000_000)
        .saturating_add(denominator / 2);
    numerator
        .checked_div(denominator)
        .and_then(|mhz| u32::try_from(mhz).ok())
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
        #[allow(
            deprecated,
            reason = "The legacy KMS cursor ioctl moves this cursor plane at input rate independently of framebuffer page flips."
        )]
        if let Err(e) = self.card.move_cursor(self.crtc, (x, y)) {
            tracing::trace!("DRM scanout: move_cursor failed: {e}");
        }
    }

    /// Park the cursor off this output's panel; the KMS plane is per-CRTC, so a plane on an output the pointer left must be hidden or it leaves a frozen arrow.
    pub fn hide(&self) {
        // Past the largest plane any driver gets; `try_from`/`saturating_neg`
        // keep it defensive instead of relying on that.
        let offset = i32::try_from(cursor::MAX_PIXEL_SIZE)
            .unwrap_or(i32::MAX)
            .saturating_neg();
        self.move_to(offset, offset);
    }
}

/// One output's hardware-cursor plane plus that output's rectangle in the compositor's combined (global) coordinate space.
pub struct OutputCursor {
    /// Identity, not index: a hotplug reordering the list would otherwise move the cursor on the wrong monitor.
    id: OutputId,
    /// `(x, y, w, h)` of the output in global coords.
    rect: (i32, i32, u32, u32),
    transform: settings_store::hardware::OutputTransform,
    /// The output's scale: the plane's image, and so its hotspot, is drawn
    /// at this density.
    scale: f64,
    mover: HwCursor,
}

/// Client cursor bitmaps have already been resampled to output pixels;
/// compositor shapes retain logical coordinates until the output is chosen.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum HotspotSpace {
    #[default]
    Logical,
    OutputPixels,
}

/// Routes a global pointer position to the right output's hardware cursor plane: each KMS plane
/// is per-CRTC and output-local, so motion moves the plane under the pointer and hides the one it left. Single-output collapses to one entry at `(0, 0)`.
#[derive(Default)]
pub struct CursorPlanes {
    outputs: Vec<OutputCursor>,
    /// The output currently showing the cursor, if any. By id rather than index, because a hotplug reorders the list.
    active: Option<OutputId>,
    /// Pixel offset of the pointer position within the cursor image: the
    /// plane's top-left sits this far up-left of the pointer.
    hotspot: (i32, i32),
    hotspot_space: HotspotSpace,
    /// Last global position, so a hotspot change re-places a still pointer.
    last: Option<(f64, f64)>,
    /// The global position, shared with the presenter, which draws the
    /// pointer into the frame on outputs without a cursor plane.
    shared: std::sync::Arc<std::sync::Mutex<(f64, f64)>>,
}

impl CursorPlanes {
    /// One `OutputCursor` per output with an active hardware cursor plane; outputs without one fall back to the software cursor in [`DrmScanout::present_no_drain`].
    pub fn new(outputs: Vec<OutputCursor>) -> Self {
        Self {
            outputs,
            active: None,
            hotspot: (0, 0),
            hotspot_space: HotspotSpace::Logical,
            last: None,
            shared: std::sync::Arc::default(),
        }
    }

    /// The pointer position as the presenter reads it.
    pub fn shared_position(&self) -> std::sync::Arc<std::sync::Mutex<(f64, f64)>> {
        self.shared.clone()
    }

    /// Adopt the newly shown image's hotspot in its own coordinate space.
    pub fn set_image_hotspot(&mut self, image: &PointerImage, size: u32) {
        let hotspot = image.bitmap(size).map_or((0, 0), |bitmap| {
            (
                i32::try_from(bitmap.hotspot.0).unwrap_or(0),
                i32::try_from(bitmap.hotspot.1).unwrap_or(0),
            )
        });
        let space = match image {
            PointerImage::Client(_) => HotspotSpace::OutputPixels,
            PointerImage::Shape(_) | PointerImage::Hidden => HotspotSpace::Logical,
        };
        if self.hotspot == hotspot && self.hotspot_space == space {
            return;
        }
        self.hotspot = hotspot;
        self.hotspot_space = space;
        if let Some((x, y)) = self.last {
            self.move_to(x, y);
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
            scale: 1.0,
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

    /// Follow an output's scale, which sizes the image its plane shows.
    pub fn set_scale(&mut self, id: OutputId, scale: f64) {
        if let Some(output) = self.outputs.iter_mut().find(|output| output.id == id) {
            output.scale = scale;
        }
        if let Some((x, y)) = self.last {
            self.move_to(x, y);
        }
    }

    /// The pixel offset applied by a cursor plane at this output density.
    pub(crate) fn hotspot_pixels(&self, scale: f64) -> (i32, i32) {
        match self.hotspot_space {
            HotspotSpace::Logical => scaled_hotspot(self.hotspot, scale),
            HotspotSpace::OutputPixels => self.hotspot,
        }
    }

    /// Move the cursor to global `(gx, gy)`, hiding the previously-active plane on an output crossing; outside every output (mismatched-height gap) the last position is left alone.
    pub fn move_to(&mut self, gx: f64, gy: f64) {
        self.last = Some((gx, gy));
        *self
            .shared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = (gx, gy);
        let rects: Vec<_> = self.outputs.iter().map(|o| o.rect).collect();
        let (xi, yi) = (pixel_coord(gx), pixel_coord(gy));
        let Some(i) = output_index_at(&rects, xi, yi) else {
            return;
        };
        let Some(target) = self.outputs.get(i) else {
            return;
        };
        let id = target.id;
        if self.active != Some(id) {
            if let Some(prev) = self.active
                && let Some(o) = self.outputs.iter().find(|o| o.id == prev)
            {
                o.mover.hide();
            }
            self.active = Some(id);
        }
        let Some(target) = self.outputs.get(i) else {
            return;
        };
        let (ox, oy, width, height) = target.rect;
        let (x, y) = logical_to_physical_point(
            xi.saturating_sub(ox),
            yi.saturating_sub(oy),
            width,
            height,
            target.transform,
        );
        let (hx, hy) = self.hotspot_pixels(target.scale);
        target
            .mover
            .move_to(x.saturating_sub(hx), y.saturating_sub(hy));
    }
}

/// The hotspot of the 1x image at an output's `scale`, matching where the
/// scaled rasterisation puts it.
fn scaled_hotspot(hotspot: (i32, i32), scale: f64) -> (i32, i32) {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a cursor hotspot is a few pixels; the rounded product is far inside i32"
    )]
    let scaled = |value: i32| (f64::from(value) * scale).round() as i32;
    (scaled(hotspot.0), scaled(hotspot.1))
}

pub(crate) fn logical_to_physical_point(
    x: i32,
    y: i32,
    logical_width: u32,
    logical_height: u32,
    transform: settings_store::hardware::OutputTransform,
) -> (i32, i32) {
    use settings_store::hardware::OutputTransform as T;
    // Output dimensions never approach i32::MAX in practice; a saturated fallback just clamps
    // rotation math rather than panicking or wrapping if that ever changes.
    let width = i32::try_from(logical_width).unwrap_or(i32::MAX);
    let height = i32::try_from(logical_height).unwrap_or(i32::MAX);
    match transform {
        T::Normal => (x, y),
        T::Rotate90 => (height.saturating_sub(1).saturating_sub(y), x),
        T::Rotate180 => (
            width.saturating_sub(1).saturating_sub(x),
            height.saturating_sub(1).saturating_sub(y),
        ),
        T::Rotate270 => (y, width.saturating_sub(1).saturating_sub(x)),
    }
}

/// Truncate a sub-pixel logical coordinate to a whole pixel. Rust's `as` cast from `f64` to
/// `i32` already saturates to `i32::MIN`/`MAX` and maps NaN to 0 (no panic, no wraparound), so an
/// out-of-range or NaN pointer position clamps rather than misbehaving; dropping the fractional
/// part is the intended pixel-snapping behaviour, not a bug.
#[allow(
    clippy::cast_possible_truncation,
    reason = "float-to-int `as` saturates and maps NaN to 0; truncating the fractional pixel is intentional"
)]
fn pixel_coord(value: f64) -> i32 {
    value as i32
}

/// Index of the first output rect containing `(xi, yi)`, or `None` in a gap between mismatched-size outputs; pure so cursor routing is unit-testable without a DRM device.
fn output_index_at(rects: &[(i32, i32, u32, u32)], xi: i32, yi: i32) -> Option<usize> {
    rects.iter().position(|&(ox, oy, ow, oh)| {
        let ow = i32::try_from(ow).unwrap_or(i32::MAX);
        let oh = i32::try_from(oh).unwrap_or(i32::MAX);
        let x_end = ox.saturating_add(ow);
        let y_end = oy.saturating_add(oh);
        xi >= ox && xi < x_end && yi >= oy && yi < y_end
    })
}

/// Without event cookies, a late completion after cancellation cannot be
/// distinguished from a newer flip on the same CRTC. Once ownership becomes
/// uncertain, only a newly established scanout can supply physical proof.
#[derive(Clone, Copy, Debug)]
struct PhysicalFlipIdentity {
    eligible: bool,
}

impl Default for PhysicalFlipIdentity {
    fn default() -> Self {
        Self { eligible: true }
    }
}

impl PhysicalFlipIdentity {
    fn cancel_pending(&mut self, pending: bool) {
        if pending {
            self.eligible = false;
        }
    }

    fn uncertain_completion(&mut self) {
        self.eligible = false;
    }
}

/// Holds one card fd as master and owns one output's scanout buffers.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool is independent hardware/fallback state (flip in flight, pageflip vs dirtyfb, dirtyfb support, hw cursor active); combining them into an enum would obscure which combinations are actually reachable without changing behaviour, and is out of scope for a lint pass"
)]
pub struct DrmScanout {
    card: Card,
    /// Positive PCI identity captured from the exact session-opened device before this fd is cloned.
    pci_bus_id: Option<String>,
    crtc: crtc::Handle,
    connector: connector::Handle,
    mode: Mode,
    /// Double-buffered scanout targets: `front` is displayed (or the in-flight flip target); the renderer draws into `1 - front`.
    buffers: [DumbBuffer; 2],
    fbs: [framebuffer::Handle; 2],
    front: usize,
    /// True between requesting a page flip and draining its completion; guards against queuing a second flip before the first retires (kernel rejects with `EBUSY`).
    flip_pending: bool,
    /// Monotonic instant at which the currently pending page flip was queued.
    /// Diagnostic only: completion clears it and logs only anomalously slow
    /// retirements, keeping the normal 144 Hz path silent.
    flip_submission: u64,
    physical_identity: PhysicalFlipIdentity,
    flip_queued_at: Option<std::time::Instant>,
    /// `CLOCK_MONOTONIC` when the pending flip was queued, the clock the
    /// kernel stamps page-flip events with.
    flip_queued_mono: Option<Duration>,
    /// Whether the page-flip path is in use. Virtual connectors start `false`
    /// because they never signal vblank; a flip there blocks in the kernel
    /// until that wait times out and holds the DRM file description, so the
    /// next frame cannot start. A physical connector latches this `false`
    /// only when a flip fails as unsupported, then uses `dirty_framebuffer`.
    use_pageflip: bool,
    /// Output-local rectangles to flush. Empty means the whole mode.
    /// A transition that only moves the overview panel must not flush both
    /// full scanouts: on virtio that upload is the multi-frame stall.
    damage_clips: Vec<ClipRect>,
    width: u32,
    height: u32,
    /// Set once `dirty_framebuffer` is known to be unsupported, so we stop retrying it and use `set_crtc` to force a flush instead.
    dirtyfb_unsupported: bool,
    /// Cursor bitmap, kept alive so its GEM handle stays valid for `set_cursor2`; re-bound by [`Self::restore`] after a VT switch/resume clears the plane. `None` when inactive.
    cursor_buffer: Option<DumbBuffer>,
    /// True when the KMS cursor plane is driving the pointer; when false we composite the software cursor into the framebuffer.
    hw_cursor_active: bool,
    /// Logical cursor size (the accessibility setting).
    cursor_size: u32,
    /// The output's scale: the cursor is drawn `cursor_size × scale` pixels.
    cursor_scale: f64,
    /// Side of the cursor plane buffer, in pixels.
    cursor_dim: u32,
    /// What the pointer shows; resolved at `cursor_size` for drawn shapes.
    pointer_image: PointerImage,
    /// Opt-in bounded CPU destination-age tracker. It owns metadata only, never pixel storage.
    xrgb_history: XrgbTargetHistory,
    xrgb_scanout_epoch: Option<u64>,
    xrgb_software_cursor: [Option<FineDamageRect>; 2],
    xrgb_damage_requested: bool,
    /// Dumb buffers kept mapped for the CPU renderer, by buffer slot.
    cpu_maps: [Option<CpuMapping>; 2],
    /// The CRTC's adaptive-sync property and what was last written to it.
    adaptive_sync: AdaptiveSync,
    /// Pixel format of `buffers`. HDR swaps in a 10-bit pair.
    format: ScanoutFormat,
    /// The dumb-buffer pair not in use: the SDR pair while HDR dumb buffers
    /// are scanned out.
    standby: Option<DumbPair>,
    /// The connector's HDR properties and whether the HDR signal is on.
    hdr_signal: HdrSignal,
    /// HDR is wanted on this output, with the sink's metadata.
    hdr_request: Option<HdrRequest>,
    /// CPU encoder for the readback path and the software cursor in HDR.
    hdr_encoder: Option<std::sync::Arc<SdrToPqEncoder>>,
}

/// Two dumb buffers and their framebuffers, in one pixel format.
struct DumbPair {
    buffers: [DumbBuffer; 2],
    fbs: [framebuffer::Handle; 2],
}

/// A dumb buffer described for `ADDFB2`, which takes an explicit fourcc:
/// legacy `ADDFB` maps depth 30 to `XRGB2101010`, not the `XBGR2101010`
/// layout the renderer writes.
struct PlanarDumb<'a>(&'a DumbBuffer);

impl PlanarBuffer for PlanarDumb<'_> {
    fn size(&self) -> (u32, u32) {
        self.0.size()
    }
    fn format(&self) -> DrmFourcc {
        self.0.format()
    }
    fn modifier(&self) -> Option<DrmModifier> {
        None
    }
    fn pitches(&self) -> [u32; 4] {
        [self.0.pitch(), 0, 0, 0]
    }
    fn handles(&self) -> [Option<drm_re::buffer::Handle>; 4] {
        [Some(self.0.handle()), None, None, None]
    }
    fn offsets(&self) -> [u32; 4] {
        [0; 4]
    }
}

impl DrmScanout {
    /// `dev_t` of the exact KMS card fd backing this scanout. Vulkan's
    /// `VK_EXT_physical_device_drm` identity is compared against this value
    /// before a renderer-owned buffer may be handed to KMS.
    pub fn drm_device_id(&self) -> u64 {
        smithay::reexports::rustix::fs::fstat(&self.card)
            .map(|stat| stat.st_rdev)
            .unwrap_or_default()
    }

    pub(crate) fn invalidate_xrgb_history(&mut self) {
        if !self.xrgb_damage_requested {
            return;
        }
        self.xrgb_history.invalidate();
        self.xrgb_software_cursor = [None, None];
        self.xrgb_scanout_epoch = self
            .xrgb_scanout_epoch
            .and_then(|epoch| epoch.checked_add(1));
    }

    /// Whether the explicit CPU destination-age experiment was requested at scanout setup.
    #[doc(hidden)]
    pub fn xrgb_damage_requested(&self) -> bool {
        self.xrgb_damage_requested
    }

    /// Whether the current scanout has a non-wrapping destination-history epoch.
    #[doc(hidden)]
    pub fn xrgb_damage_available(&self) -> bool {
        self.xrgb_damage_requested && self.xrgb_scanout_epoch.is_some() && !self.format.is_hdr()
    }

    /// Open `card_path` and become DRM master; split from [`Self::new`] so a
    /// [`CardHandle`] can open once and dup the fd per connector (libseat
    /// refuses a second open).
    fn open_card<S: Session>(session: &mut S, card_path: &Path) -> Result<Card, String> {
        use smithay::backend::session::AsErrno;
        let fd = session
            .open(
                card_path,
                OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
            )
            .map_err(|e| {
                format!(
                    "session.open({}): errno={:?}",
                    card_path.display(),
                    e.as_errno()
                )
            })?;
        let card = Card(fd);
        // set_crtc needs DRM master. The seat manager grants it with the fd on an active session; ask the kernel only if it did not.
        match card.reclaim_master() {
            MasterReclaim::AlreadyHeld => tracing::info!("DRM scanout: DRM master held"),
            MasterReclaim::Acquired => tracing::info!("DRM scanout: acquired DRM master"),
            MasterReclaim::NotGranted(e) => {
                tracing::warn!(
                    "DRM scanout: session does not hold DRM master ({e}); set_crtc may EACCES"
                );
            }
        }
        Ok(card)
    }

    /// Single-output convenience: opens `card_path`, picks the first connected connector + mode +
    /// crtc, and brings it up. Returns `Err`, never panics; the runtime uses
    /// [`CardHandle::bring_up_all`] instead, sharing `Self::from_card`.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the card can't be opened, if `resource_handles` fails, if no connected
    /// connector advertises a usable mode, if no crtc is available for the chosen connector, or
    /// if the modeset/framebuffer setup in `Self::from_card` fails.
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

        let pci_bus_id = card_pci_bus_id(&card);
        Self::from_card(card, connector, crtc, mode, pci_bus_id)
    }

    /// Allocate framebuffers, modeset `connector` on `crtc` at `mode`, and set up the hardware
    /// cursor — the bring-up tail shared by [`Self::new`] and
    /// [`CardHandle::bring_up_all`]. `card` is an independent fd on the
    /// single path, a dup on the multi-output path.
    fn from_card(
        card: Card,
        connector: connector::Handle,
        crtc: crtc::Handle,
        mode: Mode,
        pci_bus_id: Option<String>,
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

        // Keep one stable, machine-readable line in the persistent flight
        // record. Structured tracing's pretty formatter is human-friendly but
        // not a durable interchange format, and a physical performance gate
        // must distinguish a real 144 Hz KMS mode from TILO_REFRESH_HZ merely
        // pacing a 60 Hz panel at 144 callbacks per second.
        eprintln!(
            "tilo compositor: DRM scanout: modeset complete connector={} width={width} height={height} refresh_mhz={}",
            u32::from(connector),
            mode_refresh_mhz(&mode),
        );

        let adaptive_sync = AdaptiveSync::probe(&card, crtc, connector);
        let hdr_signal = HdrSignal::probe(&card, connector);

        // Best-effort: failure leaves `hw_cursor_active = false` and the software cursor takes over.
        let cursor_size =
            super::input_config::AccessibilityInputSettings::load().effective_cursor_size();
        let cursor_dim = Self::cursor_plane_dim(&card);
        let (cursor_buffer, hw_cursor_active) =
            match Self::setup_hw_cursor(&card, crtc, cursor_size, cursor_dim) {
                Ok(buf) => {
                    // Centre it so a fresh boot shows the pointer before input arrives.
                    let center_x = i32::try_from(width / 2).unwrap_or(i32::MAX);
                    let center_y = i32::try_from(height / 2).unwrap_or(i32::MAX);
                    #[allow(
                        deprecated,
                        reason = "Legacy KMS centers the new cursor before input arrives."
                    )]
                    let _ = card.move_cursor(crtc, (center_x, center_y));
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

        let use_pageflip = connector_uses_pageflip(&card, connector);
        Ok(Self {
            card,
            pci_bus_id,
            crtc,
            connector,
            mode,
            buffers: [buf0, buf1],
            fbs: [fb0, fb1],
            front: 0,
            flip_pending: false,
            flip_submission: 0,
            physical_identity: PhysicalFlipIdentity::default(),
            flip_queued_at: None,
            flip_queued_mono: None,
            use_pageflip,
            damage_clips: Vec::new(),
            width,
            height,
            dirtyfb_unsupported: false,
            cursor_buffer,
            hw_cursor_active,
            cursor_size,
            cursor_scale: 1.0,
            cursor_dim,
            pointer_image: PointerImage::default(),
            xrgb_history: XrgbTargetHistory::new(2),
            xrgb_scanout_epoch: Some(1),
            xrgb_software_cursor: [None, None],
            cpu_maps: [None, None],
            // The CPU renderer always copies through the destination
            // history: a full copy per frame is most of its frame time.
            xrgb_damage_requested: std::env::var("TILO_DRM_XRGB_DAMAGE")
                .is_ok_and(|value| value == "1")
                || super::cpu_present::cpu_render_reason().is_some(),
            adaptive_sync,
            format: ScanoutFormat::Xrgb8888,
            standby: None,
            hdr_signal,
            hdr_request: None,
            hdr_encoder: None,
        })
    }

    /// The cursor plane size the driver prefers, which is also the largest
    /// it takes: 64 where it says nothing, never more than the compositor
    /// rasterises.
    fn cursor_plane_dim(card: &Card) -> u32 {
        use smithay::reexports::drm::{Device as _, DriverCapability};
        let dim = |cap| {
            card.get_driver_capability(cap)
                .ok()
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or(CURSOR_DIM)
        };
        dim(DriverCapability::CursorWidth)
            .min(dim(DriverCapability::CursorHeight))
            .clamp(CURSOR_DIM, cursor::MAX_PIXEL_SIZE)
    }

    /// Allocate the ARGB cursor buffer, paint the default arrow, bind via `set_cursor2` (its hotspot is the top-left tip); caller must keep the buffer alive.
    fn setup_hw_cursor(
        card: &Card,
        crtc: crtc::Handle,
        cursor_size: u32,
        cursor_dim: u32,
    ) -> Result<DumbBuffer, String> {
        let mut cbuf = card
            .create_dumb_buffer((cursor_dim, cursor_dim), DrmFourcc::Argb8888, 32)
            .map_err(|e| format!("create cursor dumb buffer: {e}"))?;
        let pitch = cbuf.pitch() as usize;
        {
            let mut map = card
                .map_dumb_buffer(&mut cbuf)
                .map_err(|e| format!("map cursor buffer: {e}"))?;
            let arrow = cursor::bitmap(CursorShape::Default, cursor_size.min(cursor_dim));
            paint_cursor_argb(map.as_mut(), pitch, Some(&arrow));
        }
        // Hotspot (0,0): the arrow's tip is the buffer's top-left pixel, so plane position == pointer position; virtual GPUs (virtio-gpu/QXL) use it to place the host pointer.
        #[allow(
            deprecated,
            reason = "This legacy KMS scanout binds its ARGB cursor buffer with an explicit top-left hotspot through set_cursor2."
        )]
        card.set_cursor2(crtc, Some(&cbuf), (0, 0))
            .map_err(|e| format!("set_cursor2: {e}"))?;
        Ok(cbuf)
    }

    pub fn set_cursor_size(&mut self, cursor_size: u32) {
        let cursor_size = cursor_size.clamp(24, cursor::PLANE_SIZE);
        if self.cursor_size != cursor_size {
            self.invalidate_xrgb_history();
        }
        self.cursor_size = cursor_size;
        self.repaint_hw_cursor();
    }

    /// Draw the cursor for this output's scale.
    pub fn set_cursor_scale(&mut self, scale: f64) {
        if (self.cursor_scale - scale).abs() < f64::EPSILON {
            return;
        }
        self.cursor_scale = scale;
        self.invalidate_xrgb_history();
        self.repaint_hw_cursor();
    }

    /// The cursor's side in this output's pixels; the plane bounds it.
    fn cursor_pixel_size(&self) -> u32 {
        cursor::pixel_size(self.cursor_size, self.cursor_scale).min(self.cursor_dim)
    }

    /// Show `image` as the pointer: repaint the cursor plane (and re-bind it
    /// when the hotspot moved), or let the next software frame draw it.
    pub fn set_pointer_image(&mut self, image: PointerImage) {
        if self.pointer_image == image {
            return;
        }
        self.pointer_image = image;
        self.invalidate_xrgb_history();
        self.repaint_hw_cursor();
    }

    fn cursor_bitmap(&self) -> Option<std::sync::Arc<CursorBitmap>> {
        self.pointer_image.bitmap(self.cursor_pixel_size())
    }

    fn cursor_hotspot(&self) -> (i32, i32) {
        self.cursor_bitmap().map_or((0, 0), |bitmap| {
            (
                i32::try_from(bitmap.hotspot.0).unwrap_or(0),
                i32::try_from(bitmap.hotspot.1).unwrap_or(0),
            )
        })
    }

    fn repaint_hw_cursor(&mut self) {
        let bitmap = self.cursor_bitmap();
        let hotspot = self.cursor_hotspot();
        let Some(buffer) = self.cursor_buffer.as_mut() else {
            return;
        };
        let pitch = buffer.pitch() as usize;
        if let Ok(mut map) = self.card.map_dumb_buffer(buffer) {
            paint_cursor_argb(map.as_mut(), pitch, bitmap.as_deref());
        }
        if self.hw_cursor_active {
            #[allow(
                deprecated,
                reason = "The legacy KMS cursor plane carries the hotspot virtual GPUs use to place the host pointer."
            )]
            if let Err(error) = self.card.set_cursor2(self.crtc, Some(&*buffer), hotspot) {
                tracing::debug!("DRM scanout: re-binding the cursor hotspot failed: {error}");
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

    /// The mode this scanout was modeset at.
    pub fn output_mode(&self) -> settings_store::hardware::OutputMode {
        output_mode(&self.mode)
    }

    /// PCI bus identity of the KMS device which owns this output.
    pub fn drm_bus_id(&self) -> Option<String> {
        self.pci_bus_id.clone()
    }

    /// Direct presentation currently relies on the hardware cursor plane;
    /// otherwise cursor motion would require modifying the compositor image.
    pub fn hardware_cursor_active(&self) -> bool {
        self.hw_cursor_active
    }

    /// Whether event-driven flips are still available. Direct scanout does
    /// not activate on the legacy `dirtyfb`/`set_crtc` fallback because a second
    /// renderer-owned buffer could otherwise be reused without a retirement
    /// event.
    pub fn event_pageflips_available(&self) -> bool {
        self.use_pageflip
    }

    /// Allocate the two off-screen GBM targets used by the direct presenter.
    /// The current dumb-buffer CRTC remains untouched until a completed frame
    /// is explicitly submitted.
    fn allocate_gbm_scanout_buffers(
        &self,
        format: ScanoutFormat,
    ) -> Result<GbmScanoutBuffers, String> {
        GbmScanoutBuffers::new(self.card.try_clone()?, self.width, self.height, format)
    }

    /// Pixel format of the dumb buffers scanned out on the readback path.
    pub fn format(&self) -> ScanoutFormat {
        self.format
    }

    /// The driver piece HDR signalling lacks on this connector, if any.
    pub fn hdr_driver_missing(&self) -> Option<HdrUnavailable> {
        self.hdr_signal.missing()
    }

    /// The HDR signal is on: the connector announces PQ BT.2020.
    pub fn hdr_active(&self) -> bool {
        self.hdr_signal.enabled()
    }

    pub fn hdr_request(&self) -> Option<&HdrRequest> {
        self.hdr_request.as_ref()
    }

    /// The CPU SDR → PQ encoder for the current request.
    pub fn hdr_encoder(&self) -> Option<std::sync::Arc<SdrToPqEncoder>> {
        self.hdr_encoder.clone()
    }

    /// Ask for HDR (with the sink's metadata and SDR white) or for SDR.
    /// Turning HDR off returns the output to 8-bit SDR at once; turning it
    /// on takes effect with the next frame the presenter renders.
    pub fn set_hdr_request(&mut self, request: Option<HdrRequest>) {
        if self.hdr_request == request {
            return;
        }
        let white_changed = self.hdr_request.as_ref().map(|r| r.sdr_white_nits)
            != request.as_ref().map(|r| r.sdr_white_nits);
        self.hdr_request = request;
        if white_changed {
            self.hdr_encoder = self
                .hdr_request
                .as_ref()
                .map(|request| std::sync::Arc::new(SdrToPqEncoder::new(request.sdr_white_nits)));
        }
        if self.hdr_request.is_none() {
            self.leave_hdr();
        }
    }

    /// Allocate one pair of dumb buffers in `format`, cleared to black
    /// (which is also PQ black), with `ADDFB2` framebuffers.
    fn allocate_dumb_pair(&self, format: ScanoutFormat) -> Result<DumbPair, String> {
        let bpp = format.bytes_per_pixel().saturating_mul(8);
        let make = |index: usize| -> Result<(DumbBuffer, framebuffer::Handle), String> {
            let mut buffer = self
                .card
                .create_dumb_buffer((self.width, self.height), format.fourcc(), bpp)
                .map_err(|error| format!("create {format:?} dumb buffer[{index}]: {error}"))?;
            if let Ok(mut map) = self.card.map_dumb_buffer(&mut buffer) {
                map.as_mut().fill(0);
            }
            let fb = self
                .card
                .add_planar_framebuffer(&PlanarDumb(&buffer), FbCmd2Flags::empty())
                .map_err(|error| format!("add {format:?} framebuffer[{index}]: {error}"))?;
            Ok((buffer, fb))
        };
        let (buf0, fb0) = make(0)?;
        let (buf1, fb1) = match make(1) {
            Ok(second) => second,
            Err(error) => {
                let _ = self.card.destroy_framebuffer(fb0);
                return Err(error);
            }
        };
        Ok(DumbPair {
            buffers: [buf0, buf1],
            fbs: [fb0, fb1],
        })
    }

    /// Readback-path HDR: scan out a 10-bit dumb pair with the HDR signal
    /// on. The SDR pair waits in standby; every failure leaves the output as
    /// it was, in SDR.
    ///
    /// # Errors
    ///
    /// The reason HDR could not be shown, for Settings, and the detail.
    pub fn enter_hdr_readback(&mut self) -> Result<(), (HdrUnavailable, String)> {
        if self.format.is_hdr() {
            return Ok(());
        }
        let Some(request) = self.hdr_request.clone() else {
            return Err((
                HdrUnavailable::DriverRejected,
                "no HDR request for this output".to_owned(),
            ));
        };
        if let Some(missing) = self.hdr_signal.missing() {
            return Err((missing, "connector lacks HDR properties".to_owned()));
        }
        let pair = self
            .allocate_dumb_pair(ScanoutFormat::Xbgr2101010)
            .map_err(|error| (HdrUnavailable::FormatUnsupported, error))?;
        if let Err(error) = self
            .hdr_signal
            .enable(&self.card, self.connector, &request.display)
        {
            self.destroy_pair(&pair);
            return Err((HdrUnavailable::DriverRejected, error));
        }
        if let Err(error) = self.card.set_crtc(
            self.crtc,
            pair.fbs.first().copied(),
            (0, 0),
            &[self.connector],
            Some(self.mode),
        ) {
            let _ = self.hdr_signal.disable(&self.card, self.connector);
            self.destroy_pair(&pair);
            self.force_flush_via_set_crtc();
            return Err((
                HdrUnavailable::FormatUnsupported,
                format!("set_crtc (XBGR2101010): {error}"),
            ));
        }
        let mut pair = pair;
        std::mem::swap(&mut self.buffers, &mut pair.buffers);
        std::mem::swap(&mut self.fbs, &mut pair.fbs);
        self.standby = Some(pair);
        self.physical_identity.cancel_pending(self.flip_pending);
        self.format = ScanoutFormat::Xbgr2101010;
        self.front = 0;
        self.flip_pending = false;
        self.flip_queued_at = None;
        self.invalidate_xrgb_history();
        eprintln!(
            "tilo compositor: DRM HDR on connector={} path=readback format=XBGR2101010",
            u32::from(self.connector),
        );
        Ok(())
    }

    /// Return to 8-bit SDR: signal off, SDR dumb pair back on the CRTC.
    pub fn leave_hdr(&mut self) {
        self.physical_identity.cancel_pending(self.flip_pending);
        let was_active = self.hdr_signal.enabled() || self.format.is_hdr();
        if let Err(error) = self.hdr_signal.disable(&self.card, self.connector) {
            tracing::warn!(%error, "DRM scanout: restoring SDR signalling was refused");
        }
        if let Some(mut pair) = self.standby.take() {
            std::mem::swap(&mut self.buffers, &mut pair.buffers);
            std::mem::swap(&mut self.fbs, &mut pair.fbs);
            self.format = ScanoutFormat::Xrgb8888;
            self.front = 0;
            self.flip_pending = false;
            self.flip_queued_at = None;
            self.force_flush_via_set_crtc();
            self.destroy_pair(&pair);
            self.invalidate_xrgb_history();
        }
        if was_active {
            eprintln!(
                "tilo compositor: DRM HDR off connector={}",
                u32::from(self.connector),
            );
        }
    }

    fn destroy_pair(&self, pair: &DumbPair) {
        for fb in pair.fbs {
            let _ = self.card.destroy_framebuffer(fb);
        }
    }

    /// The dumb-buffer front is what the CRTC shows: make the connector's
    /// signal match its encoding.
    fn match_signal_to_dumb_front(&mut self) {
        if self.format.is_hdr() {
            if self.hdr_signal.needs(true)
                && let Some(request) = self.hdr_request.clone()
                && let Err(error) =
                    self.hdr_signal
                        .enable(&self.card, self.connector, &request.display)
            {
                tracing::warn!(%error, "DRM scanout: re-enabling the HDR signal was refused");
            }
        } else if self.hdr_signal.needs(false)
            && let Err(error) = self.hdr_signal.disable(&self.card, self.connector)
        {
            tracing::warn!(%error, "DRM scanout: restoring SDR signalling was refused");
        }
    }

    /// Sink and driver both support adaptive sync on this output.
    pub fn adaptive_sync_supported(&self) -> bool {
        self.adaptive_sync.supported()
    }

    /// Adaptive sync is enabled on the CRTC right now.
    pub fn adaptive_sync_active(&self) -> bool {
        self.adaptive_sync.active()
    }

    /// The driver refused to enable adaptive sync.
    pub fn adaptive_sync_rejected(&self) -> bool {
        self.adaptive_sync.rejected()
    }

    /// Ask for adaptive sync on or off. The property is written between
    /// flips: now when none is queued, otherwise just before the next one.
    pub fn set_adaptive_sync(&mut self, on: bool) {
        self.adaptive_sync.want(on);
        if !self.flip_pending {
            self.apply_adaptive_sync();
        }
    }

    fn apply_adaptive_sync(&mut self) {
        let (Some(value), Some(property)) = (
            self.adaptive_sync.pending_write(),
            self.adaptive_sync.property(),
        ) else {
            return;
        };
        let result = self
            .card
            .set_property(self.crtc, property, u64::from(value));
        if let Err(error) = &result {
            tracing::warn!(%error, crtc = ?self.crtc, value, "DRM scanout: VRR_ENABLED write refused");
        }
        // Durable and machine-readable, like the modeset line: a hardware
        // check must tell a refused property from one never requested.
        eprintln!(
            "tilo compositor: DRM adaptive sync crtc={} enabled={value} accepted={}",
            u32::from(self.crtc),
            result.is_ok(),
        );
        self.adaptive_sync.record_write(value, result.is_ok());
    }

    /// The crtc this scanout drives, so hotplug can pick one still free.
    pub fn crtc(&self) -> crtc::Handle {
        self.crtc
    }

    /// Re-establish this output after the session regains hardware (VT switch/suspend): nothing
    /// survives — CRTC has no mode, framebuffer unbound, cursor plane reset, and the queued flip died with the master lock, leaving `flip_pending` latched forever. Best-effort throughout.
    pub fn restore(&mut self) {
        self.physical_identity.cancel_pending(self.flip_pending);
        self.invalidate_xrgb_history();
        // The in-flight flip died with the master lock; clearing this first lets the next frame present at all.
        self.flip_pending = false;
        self.flip_queued_at = None;
        // Re-binding the mode is what the kernel needs to scan out again.
        if let Some(fb) = self.fb_at(self.front) {
            if let Err(e) = self.card.set_crtc(
                self.crtc,
                Some(fb),
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
        } else {
            tracing::warn!(
                "DRM scanout: front buffer index out of range on resume; skipping re-modeset"
            );
        }
        // Another DRM master may have left adaptive sync and the HDR signal
        // either way.
        self.adaptive_sync.forget_applied();
        self.apply_adaptive_sync();
        self.hdr_signal.forget();
        self.match_signal_to_dumb_front();
        // Per-CRTC state, gone with the rest — without this the pointer is invisible after every VT switch.
        if self.hw_cursor_active
            && let Some(buf) = self.cursor_buffer.as_ref()
        {
            #[allow(
                deprecated,
                reason = "Session resume restores the legacy KMS cursor plane and hotspot after its binding was lost with DRM master."
            )]
            if let Err(e) = self
                .card
                .set_cursor2(self.crtc, Some(buf), self.cursor_hotspot())
            {
                tracing::warn!(
                    "DRM scanout: re-binding the cursor plane on resume failed ({e}); \
                         falling back to the software cursor"
                );
                self.hw_cursor_active = false;
                self.invalidate_xrgb_history();
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
    ///
    /// # Errors
    ///
    /// Returns the error `fill` reports; the frame is then not presented.
    pub fn present_into<F, E>(&mut self, cursor: Option<(f64, f64)>, fill: F) -> Result<bool, E>
    where
        F: FnOnce(&mut [u8], usize) -> Result<(), E>,
    {
        // Drain any retired completion so we can flip again.
        self.drain_flip_events();
        self.present_no_drain_result(cursor, fill)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the mapping lifetime, bridge transaction, cursor write, and presentation boundary stay ordered together"
    )]
    /// Present one opt-in versioned scene through the bounded two-target XRGB history.
    ///
    /// This is intentionally separate from [`Self::present_into`]: arbitrary callers retain the
    /// established complete-destination contract, while this path can prove both dumb-buffer
    /// identity and every skipped scene delta.
    ///
    /// # Errors
    ///
    /// Returns a bridge error if rendering, retained readback, or the complete tracked XRGB
    /// write fails. Such a failure invalidates the destination-age history before it is returned.
    pub fn present_scene_damage(
        &mut self,
        cursor: Option<(f64, f64)>,
        bridge: &mut VelloDrmBridge,
        scene: &Scene,
        damage: SceneDamage<'_>,
    ) -> Result<(), BridgeError> {
        // This is the single-output entry point, just like `present_into`: retire a previous
        // event before deciding whether a new history transaction may target the back buffer.
        self.drain_flip_events();
        if !self.xrgb_damage_available()
            || pending_pageflip_refuses_frame(self.use_pageflip, self.flip_pending)
        {
            return Ok(());
        }
        if !bridge.xrgb_history_available() {
            // Identity exhaustion disables only the experiment. Retain the old complete-output
            // presenter rather than turning a safe full frame into a dropped frame.
            self.invalidate_xrgb_history();
            let mut result = Ok(());
            self.present_no_drain(cursor, |dst, pitch| {
                result = bridge.paint_damage_into_xrgb8888(scene, damage, dst, pitch);
            });
            return result;
        }
        let idx = if self.use_pageflip {
            1usize.saturating_sub(self.front)
        } else {
            self.front
        };
        let (dw, dh) = (self.width as usize, self.height as usize);
        let hw_cursor_active = self.hw_cursor_active;
        let cursor_bitmap = self.cursor_bitmap();
        let scanout_epoch = self.xrgb_scanout_epoch;
        let Some(pitch) = self.buffers.get(idx).map(|buffer| buffer.pitch() as usize) else {
            self.invalidate_xrgb_history();
            return Ok(());
        };
        let identity = bridge
            .xrgb_history_identity(pitch, 2)
            .zip(scanout_epoch)
            .map(|(identity, epoch)| identity.with_scanout_epoch(epoch));
        let previous_cursor = (!hw_cursor_active)
            .then(|| self.xrgb_software_cursor.get(idx).copied().flatten())
            .flatten();
        let current_cursor = (!hw_cursor_active)
            .then(|| software_cursor_damage_rect(cursor, dw, dh, cursor_bitmap.as_deref()))
            .flatten();
        let extra = previous_cursor.as_slice();
        // Keep the mapping and every borrow derived from it inside this closure. In particular,
        // the `map_dumb_buffer` result can own a mapping tied to `self.buffers`; no whole-scanout
        // state may be invalidated until that result and any mapping have been dropped.
        let write_result = (|| -> Result<Option<_>, BridgeError> {
            let Some(buffer) = self.buffers.get_mut(idx) else {
                tracing::warn!("DRM scanout: back-buffer index {idx} is unavailable");
                return Ok(None);
            };
            let mut map = match self.card.map_dumb_buffer(buffer) {
                Ok(map) => map,
                Err(error) => {
                    tracing::warn!("DRM scanout: map_dumb_buffer: {error}");
                    return Ok(None);
                }
            };
            let transaction = self.xrgb_history.prepare(
                idx,
                identity,
                damage.revision,
                damage.previous_revision,
                damage.regions,
                extra,
            );
            let result = bridge.paint_damage_into_xrgb8888_with_history(
                scene,
                damage,
                &transaction,
                map.as_mut(),
                pitch,
            )?;
            if !hw_cursor_active {
                if let Some((px, py)) = cursor
                    && let Some(bitmap) = cursor_bitmap.as_deref()
                {
                    draw_cursor(
                        map.as_mut(),
                        pitch,
                        dw,
                        dh,
                        pixel_coord(px),
                        pixel_coord(py),
                        bitmap,
                    );
                }
                let Some(slot) = self.xrgb_software_cursor.get_mut(idx) else {
                    transaction.abort();
                    return Err(BridgeError::InvalidSize(
                        "software cursor target index is outside history slots".to_owned(),
                    ));
                };
                *slot = current_cursor;
            }
            transaction.commit(if result.damage_admitted {
                XrgbHistoryCommit::DamageAccepted
            } else {
                XrgbHistoryCommit::FullBoundary
            });
            Ok(Some(result))
        })();
        match write_result {
            Ok(Some(_)) => {}
            Ok(None) => {
                self.invalidate_xrgb_history();
                return Ok(());
            }
            Err(error) => {
                self.invalidate_xrgb_history();
                return Err(error);
            }
        }

        if self.use_pageflip {
            // Keep the established page-flip transfer hint. Failure does not suppress the
            // flip, but it is a presentation boundary for destination-age tracking.
            let (cw, ch) = self.clip_dims();
            let clip = ClipRect::new(0, 0, cw, ch);
            let dirty_ok = self
                .fb_at(idx)
                .is_some_and(|fb| self.card.dirty_framebuffer(fb, &[clip]).is_ok());
            let flip_ok = self.flip_to(idx);
            self.note_publish(dirty_ok, flip_ok);
            if !pageflip_history_commit_is_safe(dirty_ok, flip_ok) {
                self.invalidate_xrgb_history();
            }
        } else if !self.flush_dirty(idx) {
            // `flush_dirty` has performed its established set_crtc fallback. Its result is not
            // proof that the previous destination-age chain survived that mode transition.
            self.invalidate_xrgb_history();
        }
        Ok(())
    }

    /// Wait for this CRTC's pending page flip. Returns how long the wait
    /// took and the flip's queue-to-vblank latency, or `None` for the
    /// latency when no flip was pending. `Err` when the flip did not
    /// complete in `timeout`.
    pub(crate) fn wait_cpu_flip(
        &mut self,
        timeout: Duration,
    ) -> Result<(Duration, Option<Duration>), Duration> {
        let started = Instant::now();
        let deadline = started.checked_add(timeout).unwrap_or(started);
        let mut latency = None;
        loop {
            if let Ok(events) = self.card.receive_events() {
                for event in events {
                    if let Event::PageFlip(flip) = event
                        && flip.crtc == self.crtc
                    {
                        latency = self
                            .flip_queued_mono
                            .take()
                            .map(|queued| flip.duration.saturating_sub(queued));
                        self.log_flip_completion();
                        self.flip_pending = false;
                    }
                }
            }
            if !self.use_pageflip || !self.flip_pending {
                return Ok((started.elapsed(), latency));
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(started.elapsed());
            }
            let timeout_ms = i32::try_from(deadline.saturating_duration_since(now).as_millis())
                .unwrap_or(i32::MAX)
                .max(1);
            let mut fd = libc::pollfd {
                fd: self.card.as_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: `fd` points to one valid pollfd for the duration of the
            // call and the borrowed card descriptor outlives it.
            let ready = unsafe { libc::poll(&raw mut fd, 1, timeout_ms) };
            if ready < 0
                && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
            {
                return Err(started.elapsed());
            }
        }
    }

    /// Present the CPU renderer's screen: copy what the back buffer is
    /// missing, by the per-buffer destination history, and queue a flip.
    /// The caller has waited for the previous flip.
    #[allow(
        clippy::too_many_lines,
        reason = "the mapping, history transaction, cursor write and flip stay ordered together, as in present_scene_damage"
    )]
    pub(crate) fn present_cpu(
        &mut self,
        cpu: &mut super::cpu_present::CpuRenderer,
        cursor: Option<(f64, f64)>,
    ) {
        if self.use_pageflip && self.flip_pending {
            return;
        }
        let idx = if self.use_pageflip {
            1usize.saturating_sub(self.front)
        } else {
            self.front
        };
        let (dw, dh) = (self.width as usize, self.height as usize);
        let hw_cursor_active = self.hw_cursor_active;
        cpu.record_cursor(!hw_cursor_active);
        let cursor_bitmap = self.cursor_bitmap();
        let current_cursor = (!hw_cursor_active)
            .then(|| software_cursor_damage_rect(cursor, dw, dh, cursor_bitmap.as_deref()))
            .flatten();
        let previous_cursor = (!hw_cursor_active)
            .then(|| self.xrgb_software_cursor.get(idx).copied().flatten())
            .flatten();
        if current_cursor != previous_cursor {
            for rect in [previous_cursor, current_cursor].into_iter().flatten() {
                cpu.add_pending(rect);
            }
        }
        if !cpu.has_pending() {
            return;
        }
        let Some(pitch) = self.buffers.get(idx).map(|buffer| buffer.pitch() as usize) else {
            return;
        };
        let identity = self
            .xrgb_scanout_epoch
            .map(|scanout_epoch| XrgbHistoryIdentity {
                bridge_generation: u64::MAX,
                bridge_epoch: cpu.generation(),
                width: self.width,
                height: self.height,
                pitch,
                targets: 2,
                scanout_epoch,
            });
        let revision = cpu.presented_revision().wrapping_add(1);
        let pending: Option<Vec<FineDamageRect>> = cpu.pending().map(<[_]>::to_vec);
        let extra = previous_cursor.as_slice();
        let started = Instant::now();
        let mut fresh_mapping = false;
        let written = (|| -> Option<(u64, usize, bool)> {
            let handle = u32::from(self.buffers.get(idx)?.handle());
            let len = pitch.checked_mul(dh)?;
            let slot = self.cpu_maps.get_mut(idx)?;
            if slot
                .as_ref()
                .is_none_or(|mapping| mapping.handle != handle || mapping.len != len)
            {
                *slot = None;
                fresh_mapping = true;
                match CpuMapping::new(self.card.as_fd(), handle, len) {
                    Ok(mapping) => *slot = Some(mapping),
                    Err(error) => {
                        tracing::warn!("DRM scanout: mapping a dumb buffer: {error}");
                        return None;
                    }
                }
            }
            let dst = slot.as_mut()?.bytes();
            let transaction = self.xrgb_history.prepare(
                idx,
                identity,
                revision,
                Some(cpu.presented_revision()),
                pending.as_deref(),
                extra,
            );
            let full_rect = FineDamageRect {
                x: 0,
                y: 0,
                width: self.width,
                height: self.height,
            };
            let (pixels, rects, full) = match transaction.write_plan() {
                XrgbWritePlan::Full => (cpu.copy_to_xrgb(dst, pitch, full_rect), 1, true),
                XrgbWritePlan::Rects(rects) => (
                    rects
                        .iter()
                        .map(|rect| cpu.copy_to_xrgb(dst, pitch, *rect))
                        .sum(),
                    rects.len(),
                    false,
                ),
            };
            if !hw_cursor_active {
                if let Some((px, py)) = cursor
                    && let Some(bitmap) = cursor_bitmap.as_deref()
                {
                    draw_cursor(dst, pitch, dw, dh, pixel_coord(px), pixel_coord(py), bitmap);
                }
                if let Some(slot) = self.xrgb_software_cursor.get_mut(idx) {
                    *slot = current_cursor;
                }
            }
            transaction.commit(XrgbHistoryCommit::DamageAccepted);
            Some((pixels, rects, full))
        })();
        let Some((pixels, rects, full)) = written else {
            self.invalidate_xrgb_history();
            return;
        };
        let copy = started.elapsed();
        // A buffer mapped just now pays its page faults in this copy.
        if full && !fresh_mapping {
            cpu.measure_cached_copy_once(copy);
        }
        cpu.record_present(copy, pixels, rects);
        let accepted = if self.use_pageflip {
            let (cw, ch) = self.clip_dims();
            let clip = ClipRect::new(0, 0, cw, ch);
            let dirty_ok = self
                .fb_at(idx)
                .is_some_and(|fb| self.card.dirty_framebuffer(fb, &[clip]).is_ok());
            let flip_ok = self.flip_to(idx);
            pageflip_history_commit_is_safe(dirty_ok, flip_ok)
        } else {
            self.flush_dirty(idx)
        };
        if !accepted {
            self.invalidate_xrgb_history();
        }
        cpu.finish_present(revision, accepted);
    }

    /// Present without draining DRM events first: the multi-output backend drains the shared card fd
    /// once per frame and routes via [`Self::mark_flip_complete`] — draining per-scanout here would steal sibling outputs' events.
    pub fn present_no_drain<F>(&mut self, cursor: Option<(f64, f64)>, fill: F)
    where
        F: FnOnce(&mut [u8], usize),
    {
        let _: Result<bool, std::convert::Infallible> =
            self.present_no_drain_result(cursor, |dst, pitch| {
                fill(dst, pitch);
                Ok(())
            });
    }

    /// Present only after `fill` has completely produced the new back buffer.
    ///
    /// Returning `Err` is a frame drop, not a presentation request. This is
    /// load-bearing for GPU readback: flipping after a failed map would expose
    /// the untouched previous contents and make a responsive compositor look
    /// frozen even though its logical state kept advancing.
    fn present_no_drain_result<F, E>(
        &mut self,
        cursor: Option<(f64, f64)>,
        fill: F,
    ) -> Result<bool, E>
    where
        F: FnOnce(&mut [u8], usize) -> Result<(), E>,
    {
        if pending_pageflip_refuses_frame(self.use_pageflip, self.flip_pending) {
            // Previous flip not yet retired — drop this frame (natural vsync throttle); the next one supersedes it.
            return Ok(false);
        }

        // Back buffer when flipping, else the single live buffer. `front` is always 0 or 1, so
        // this is equivalent to `1 - self.front`, just without unchecked subtraction.
        let idx = if self.use_pageflip {
            1usize.saturating_sub(self.front)
        } else {
            self.front
        };

        // `fill` is an arbitrary caller-owned write, so no retained destination claim can
        // survive this legacy entry point. Invalidate before borrowing the dumb buffer; a map
        // failure merely makes this conservative boundary earlier.
        self.invalidate_xrgb_history();

        let (dw, dh) = (self.width as usize, self.height as usize);
        let cursor_bitmap = self.cursor_bitmap();
        let Some(buffer) = self.buffers.get_mut(idx) else {
            tracing::warn!("DRM scanout: back-buffer index {idx} out of range; skipping frame");
            return Ok(false);
        };
        let pitch = buffer.pitch() as usize;
        {
            let mut map = match self.card.map_dumb_buffer(buffer) {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!("DRM scanout: map_dumb_buffer: {e}");
                    return Ok(false);
                }
            };
            let dst = map.as_mut();
            if !paint_completed(fill(dst, pitch))? {
                return Ok(false);
            }
            // Software fallback only — the hardware plane composites itself and must not also be drawn into the frame.
            if !self.hw_cursor_active
                && let Some((px, py)) = cursor
                && let Some(bitmap) = cursor_bitmap.as_deref()
            {
                match (self.format.is_hdr(), self.hdr_encoder.as_deref()) {
                    (true, Some(encoder)) => cursor::composite_xbgr2101010(
                        dst,
                        pitch,
                        dw,
                        dh,
                        (pixel_coord(px), pixel_coord(py)),
                        bitmap,
                        encoder,
                    ),
                    (true, None) => {}
                    (false, _) => {
                        draw_cursor(dst, pitch, dw, dh, pixel_coord(px), pixel_coord(py), bitmap);
                    }
                }
            }
        }

        if self.use_pageflip {
            // Best-effort CPU→host transfer before flipping: on virtual GPUs dirty_framebuffer flushes the
            // dumb buffer into the host resource (without it a bare flip presents unreceived bytes); no-op/unsupported on real GPUs.
            let clips = self.take_dirty_clips();
            let dirty_ok = self
                .fb_at(idx)
                .is_some_and(|fb| self.card.dirty_framebuffer(fb, &clips).is_ok());
            let flip_ok = self.flip_to(idx);
            self.note_publish(dirty_ok, flip_ok);
        } else {
            let _ = self.flush_dirty(idx);
        }
        Ok(true)
    }

    /// The framebuffer bound to buffer slot `idx`, if the index is in range. `idx` is always 0 or
    /// 1 by construction (`front` toggles between the two dumb buffers), but it arrives as a
    /// runtime `usize` rather than a value clippy can prove in bounds, so this stays defensive.
    fn fb_at(&self, idx: usize) -> Option<framebuffer::Handle> {
        self.fbs.get(idx).copied()
    }

    /// Mode dimensions as `u16` for `ClipRect`. Both fields originate from a DRM `Mode`'s `u16`
    /// size in [`Self::from_card`], so this cannot lose precision in practice; `try_from` still
    /// guards it defensively rather than assuming that invariant holds.
    fn clip_dims(&self) -> (u16, u16) {
        (
            u16::try_from(self.width).unwrap_or(u16::MAX),
            u16::try_from(self.height).unwrap_or(u16::MAX),
        )
    }

    /// Queue a page flip to buffer `idx`, making it the new front. A transient `EBUSY`/`EAGAIN` skips the frame; a "not supported" errno latches the page-flip path off, falling back to `dirty_framebuffer`.
    fn flip_to(&mut self, idx: usize) -> bool {
        self.apply_adaptive_sync();
        let Some(fb) = self.fb_at(idx) else {
            tracing::warn!("DRM scanout: no framebuffer at index {idx}; skipping page flip");
            return false;
        };
        match self
            .card
            .page_flip(self.crtc, fb, PageFlipFlags::EVENT, None)
        {
            Ok(()) => {
                self.flip_submission = self.flip_submission.wrapping_add(1);
                self.flip_queued_at = Some(std::time::Instant::now());
                self.flip_queued_mono = Some(monotonic_now());
                self.front = idx;
                self.note_publish(false, true);
                tracing::trace!(
                    crtc = ?self.crtc,
                    buffer = idx,
                    "FRAME-LIFECYCLE event=drm-flip-queued"
                );
                true
            }
            Err(e) => {
                let code = e.raw_os_error().unwrap_or(0);
                if code == libc::EBUSY || code == libc::EAGAIN {
                    tracing::trace!("DRM scanout: page_flip busy; skipping frame");
                    false
                } else {
                    tracing::warn!(
                        "DRM scanout: page_flip unsupported (errno {code}: {e}); \
                         falling back to dirty_framebuffer"
                    );
                    self.use_pageflip = false;
                    let _ = self.card.set_crtc(
                        self.crtc,
                        Some(fb),
                        (0, 0),
                        &[self.connector],
                        Some(self.mode),
                    );
                    self.front = idx;
                    false
                }
            }
        }
    }

    /// Flush the live buffer with `DRM_IOCTL_MODE_DIRTYFB` (how virtio-gpu transfers a CPU buffer to the host), falling back to a forced `set_crtc` where dirtyfb is unsupported.
    /// Restrict the next flush to these output-local rectangles.
    /// An empty list, or never calling this, flushes the whole mode.
    pub fn set_damage_clips(&mut self, clips: Vec<ClipRect>) {
        self.damage_clips = clips;
    }

    /// `None` flushes the whole mode on the next present.
    pub fn set_damage_rect(&mut self, rect: Option<(u16, u16, u16, u16)>) {
        self.damage_clips = rect
            .and_then(|(x, y, width, height)| damage_to_clip(x, y, width, height))
            .map_or(Vec::new(), |clip| vec![clip]);
    }

    fn take_dirty_clips(&mut self) -> Vec<ClipRect> {
        let clips = std::mem::take(&mut self.damage_clips);
        if !clips.is_empty() {
            return clips;
        }
        let (cw, ch) = self.clip_dims();
        damage_to_clip(0, 0, cw, ch).map_or(Vec::new(), |clip| vec![clip])
    }

    fn flush_dirty(&mut self, idx: usize) -> bool {
        self.apply_adaptive_sync();
        if self.dirtyfb_unsupported {
            self.force_flush_via_set_crtc();
            self.note_publish(false, false);
            return false;
        }
        let Some(fb) = self.fb_at(idx) else {
            tracing::warn!("DRM scanout: no framebuffer at index {idx}; falling back to set_crtc");
            self.force_flush_via_set_crtc();
            self.note_publish(false, false);
            return false;
        };
        let clips = self.take_dirty_clips();
        match self.card.dirty_framebuffer(fb, &clips) {
            Ok(()) => {
                // The clip is accepted. A virtual CRTC never delivers the
                // page-flip event, so this return is the retirement.
                self.note_publish(true, false);
                true
            }
            Err(error) => {
                let code = error.raw_os_error().unwrap_or(0);
                if dirty_errno_is_transient(code) {
                    tracing::trace!(
                        "DRM scanout: dirty_framebuffer busy (errno {code}); not modesetting"
                    );
                    // set_crtc would wait for the page-flip event the busy
                    // commit is already holding. Drop the userspace count so
                    // the next frame can start.
                    self.note_publish(false, false);
                    return false;
                }
                tracing::warn!(
                    "DRM scanout: dirty_framebuffer unsupported ({error}); using set_crtc"
                );
                self.dirtyfb_unsupported = true;
                self.force_flush_via_set_crtc();
                self.note_publish(false, false);
                false
            }
        }
    }

    /// Record whether this publish still owns a buffer.
    ///
    /// `dirty_accepted` retires a virtual scanout immediately. A queued page
    /// flip on a real connector stays in flight until its event.
    fn note_publish(&mut self, dirty_accepted: bool, page_flip_queued: bool) {
        let inflight = inflight_after_publish(self.use_pageflip, dirty_accepted, page_flip_queued);
        self.flip_pending = inflight > 0;
        if inflight == 0 {
            self.flip_queued_at = None;
        }
    }

    /// Non-blocking drain of DRM events, clearing `flip_pending` on completion; fd is `O_NONBLOCK` so
    /// `EAGAIN` means no events, not failure. Single-output only — multi-output routes by crtc via [`Self::poll_flip_completions`] + [`Self::mark_flip_complete`].
    fn drain_flip_events(&mut self) {
        if let Ok(events) = self.card.receive_events() {
            for ev in events {
                if let Event::PageFlip(_) = ev {
                    self.log_flip_completion();
                    self.flip_pending =
                        inflight_after_page_flip_event(u8::from(self.flip_pending)) > 0;
                }
            }
        }
    }

    /// Drain pending DRM events, returning the crtc of each flip completion. Every per-connector
    /// scanout dups the same open file description, so completions for all crtcs arrive on any one fd — must drain **once** and route via [`Self::mark_flip_complete`], or the first scanout wedges its siblings' `flip_pending` forever.
    pub fn poll_flip_completions(&mut self) -> Vec<crtc::Handle> {
        self.poll_flip_events()
            .into_iter()
            .map(|event| event.crtc)
            .collect()
    }

    fn poll_flip_events(&self) -> Vec<drm_re::control::PageFlipEvent> {
        self.card.receive_events().map_or_else(
            |_| Vec::new(),
            |events| {
                events
                    .filter_map(|event| match event {
                        Event::PageFlip(flip) => Some(flip),
                        _ => None,
                    })
                    .collect()
            },
        )
    }

    pub fn flip_submission(&self) -> u64 {
        self.flip_submission
    }

    pub fn can_prove_physical_flip(&self) -> bool {
        self.physical_identity.eligible
            && matches!(
                self.card
                    .get_driver_capability(drm_re::DriverCapability::MonotonicTimestamp),
                Ok(1)
            )
    }

    /// The driver stamps page-flip events with `CLOCK_MONOTONIC`, the clock
    /// [`Instant`] reads, so [`instant_from_monotonic`] can place them.
    pub fn monotonic_flip_timestamps(&self) -> bool {
        matches!(
            self.card
                .get_driver_capability(drm_re::DriverCapability::MonotonicTimestamp),
            Ok(1)
        )
    }

    pub fn invalidate_physical_feedback(&mut self) {
        self.physical_identity.uncertain_completion();
    }

    /// Wait for the page-flip event for this CRTC without spinning.
    ///
    /// The card fd is nonblocking for ordinary drains; `poll(2)` supplies the
    /// blocking boundary here. Events for sibling CRTCs are returned as well
    /// because every dup of a card shares one DRM event stream.
    pub fn wait_flip_completions(
        &mut self,
        timeout: Duration,
    ) -> (DirectRetirement, Vec<drm_re::control::PageFlipEvent>) {
        let deadline = Instant::now()
            .checked_add(timeout)
            .unwrap_or_else(Instant::now);
        let mut completed = Vec::new();
        loop {
            completed.extend(self.poll_flip_events());
            let now = Instant::now();
            let crtcs: Vec<_> = completed.iter().map(|event| event.crtc).collect();
            if let Some(retirement) = retirement_after_poll(self.crtc, &crtcs, now >= deadline) {
                return (retirement, completed);
            }
            let remaining = deadline.saturating_duration_since(now);
            let timeout_ms = i32::try_from(remaining.as_millis())
                .unwrap_or(i32::MAX)
                .max(1);
            let mut fd = libc::pollfd {
                fd: self.card.as_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: `fd` points to one valid pollfd for the duration of the
            // call and the borrowed card descriptor outlives it.
            let ready = unsafe { libc::poll(&raw mut fd, 1, timeout_ms) };
            if ready == 0 {
                return (DirectRetirement::TimedOut, completed);
            }
            if ready < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                tracing::warn!(%error, "DRM page-flip retirement poll failed");
                return (DirectRetirement::TimedOut, completed);
            }
        }
    }

    /// Clear `flip_pending` iff `crtc` is this scanout's crtc.
    pub fn mark_flip_complete(&mut self, crtc: crtc::Handle) {
        if crtc == self.crtc {
            self.log_flip_completion();
            self.flip_pending = inflight_after_page_flip_event(u8::from(self.flip_pending)) > 0;
        }
    }

    fn log_flip_completion(&mut self) {
        let Some(queued_at) = self.flip_queued_at.take() else {
            return;
        };
        let elapsed = queued_at.elapsed();
        if elapsed >= std::time::Duration::from_millis(100) {
            eprintln!(
                "FRAME-LIFECYCLE event=drm-flip-completed crtc={:?} elapsed_us={}",
                self.crtc,
                elapsed.as_micros(),
            );
        }
    }

    /// Queue one already-completed imported target. The direct state advances
    /// only after the kernel accepted the flip.
    pub fn submit_direct_target(
        &mut self,
        targets: &mut DirectScanoutTargets,
        index: usize,
    ) -> DirectSubmit {
        if !self.use_pageflip || self.flip_pending || targets.state.render_slot() != Some(index) {
            return DirectSubmit::Busy;
        }
        self.apply_adaptive_sync();
        if targets.format.is_hdr() && !targets.crtc_bound {
            return self.bind_hdr_target(targets, index);
        }
        let Some(framebuffer) = targets.framebuffer(index) else {
            return DirectSubmit::Failed(format!(
                "direct scanout slot {index} has no KMS framebuffer"
            ));
        };
        match self
            .card
            .page_flip(self.crtc, framebuffer, PageFlipFlags::EVENT, None)
        {
            Ok(()) => {
                self.flip_pending = true;
                if targets.state.submit(index) {
                    self.flip_submission = self.flip_submission.wrapping_add(1);
                    targets.presented = true;
                    targets.flip_queued_at = Some(Instant::now());
                    DirectSubmit::Queued
                } else {
                    self.flip_pending = false;
                    DirectSubmit::Failed(format!(
                        "direct scanout slot {index} lost ownership before submit"
                    ))
                }
            }
            Err(error) => {
                let errno = error.raw_os_error().unwrap_or_default();
                if errno == libc::EBUSY || errno == libc::EAGAIN {
                    tracing::trace!("DRM direct scanout page_flip busy; dropping frame");
                    DirectSubmit::Busy
                } else {
                    tracing::warn!(%error, errno, "DRM direct scanout page_flip failed; retaining CPU fallback");
                    DirectSubmit::Failed(format!(
                        "DRM direct scanout page flip failed (errno {errno}): {error}"
                    ))
                }
            }
        }
    }

    /// Show the first frame of HDR direct targets: signal on, then a modeset
    /// onto the 10-bit (or half-float) framebuffer. Undone on any refusal.
    fn bind_hdr_target(
        &mut self,
        targets: &mut DirectScanoutTargets,
        index: usize,
    ) -> DirectSubmit {
        let Some(framebuffer) = targets.framebuffer(index) else {
            return DirectSubmit::Failed(format!(
                "direct scanout slot {index} has no KMS framebuffer"
            ));
        };
        let Some(request) = self.hdr_request.clone() else {
            return DirectSubmit::HdrFailed(
                HdrUnavailable::DriverRejected,
                "HDR targets without an HDR request".to_owned(),
            );
        };
        if self.hdr_signal.needs(true)
            && let Err(error) = self
                .hdr_signal
                .enable(&self.card, self.connector, &request.display)
        {
            return DirectSubmit::HdrFailed(HdrUnavailable::DriverRejected, error);
        }
        if let Err(error) = self.card.set_crtc(
            self.crtc,
            Some(framebuffer),
            (0, 0),
            &[self.connector],
            Some(self.mode),
        ) {
            let _ = self.hdr_signal.disable(&self.card, self.connector);
            self.force_flush_via_set_crtc();
            return DirectSubmit::HdrFailed(
                HdrUnavailable::FormatUnsupported,
                format!("set_crtc ({:?}): {error}", targets.format),
            );
        }
        if !targets.state.present_immediately(index) {
            return DirectSubmit::Failed(format!(
                "direct scanout slot {index} lost ownership before the HDR modeset"
            ));
        }
        targets.crtc_bound = true;
        targets.presented = true;
        eprintln!(
            "tilo compositor: DRM HDR on connector={} path=direct format={:?}",
            u32::from(self.connector),
            targets.format,
        );
        DirectSubmit::Presented
    }

    /// Retire the imported target for this output after its page-flip event.
    pub fn mark_direct_flip_complete(
        &mut self,
        targets: &mut DirectScanoutTargets,
        crtc: crtc::Handle,
    ) {
        if crtc == self.crtc {
            self.log_flip_completion();
            self.flip_pending = false;
            let _ = targets.state.complete();
            targets.flip_queued_at = None;
        }
    }

    fn force_flush_via_set_crtc(&self) {
        let Some(fb) = self.fb_at(self.front) else {
            tracing::warn!("DRM scanout: no front framebuffer; skipping forced flush");
            return;
        };
        let _ = self.card.set_crtc(
            self.crtc,
            Some(fb),
            (0, 0),
            &[self.connector],
            Some(self.mode),
        );
    }

    /// Rebind the established dumb-buffer front before imported GBM framebuffer objects are
    /// released. KMS retains its own framebuffer reference after a successful flip, so dropping
    /// userspace BO/FB owners first can leave the CRTC scanning an object we just destroyed.
    pub fn restore_legacy_front(&mut self) {
        self.physical_identity.cancel_pending(self.flip_pending);
        self.flip_pending = false;
        self.force_flush_via_set_crtc();
        self.match_signal_to_dumb_front();
        self.invalidate_xrgb_history();
    }

    /// Power the display for idle/lock blanking, via the legacy DPMS property (`0 = On`, `3 = Off`
    /// per `drm_mode.h`; virtio-gpu may ignore it) and a CRTC toggle (the reliable cross-driver path). Both best-effort; the backend skips `Self::present` while blanked.
    pub fn set_dpms(&mut self, on: bool) {
        self.physical_identity.cancel_pending(self.flip_pending);
        self.invalidate_xrgb_history();
        self.set_dpms_property(on);
        if on {
            // Re-modeset the saved framebuffer + mode so the last frame is back on screen; the next `present` resumes normal blitting.
            let Some(fb) = self.fb_at(self.front) else {
                tracing::warn!(
                    "DRM scanout: set_dpms(on) has no front framebuffer; skipping restore"
                );
                return;
            };
            if let Err(e) = self.card.set_crtc(
                self.crtc,
                Some(fb),
                (0, 0),
                &[self.connector],
                Some(self.mode),
            ) {
                tracing::warn!("DRM scanout: set_dpms(on) restore set_crtc failed: {e}");
            } else {
                tracing::info!("DRM scanout: display restored (DPMS on)");
            }
            self.match_signal_to_dumb_front();
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

impl Drop for DrmScanout {
    fn drop(&mut self) {
        // Framebuffer IDs are kernel objects independent of the dumb-buffer
        // handles.  Explicit teardown matters for hotplug and rapid developer
        // compositor replacement; otherwise each cycle leaks two IDs until
        // the DRM fd finally closes.
        let standby = self
            .standby
            .as_ref()
            .map_or([None, None], |pair| pair.fbs.map(Some));
        for fb in self.fbs.into_iter().chain(standby.into_iter().flatten()) {
            if let Err(error) = self.card.destroy_framebuffer(fb) {
                tracing::debug!(?fb, %error, "failed to destroy DRM framebuffer during teardown");
            }
        }
    }
}

/// One output's scanout plus the fields the runtime needs to intern a stable [`OutputId`] and lay outputs out side-by-side.
pub struct ScanoutBuild {
    /// `dev_t` of the `cardN` node this output is on.
    pub device_id: u64,
    /// DRM connector handle (raw u32) for this output.
    pub connector_id: u32,
    /// Stable DRM connector name (`eDP-1`, `HDMI-A-1`) used by Settings.
    pub connector_name: String,
    /// Collision-resolved persisted settings key for this physical connector.
    pub config_key: String,
    /// Physical panel size in mm, when reported — the one input to scale derivation that resolution alone cannot supply (see [`super::scale`]).
    pub physical_mm: Option<(u32, u32)>,
    /// EDID identity of the monitor on this connector, when it has one.
    pub monitor: Option<settings_store::hardware::MonitorIdentity>,
    /// The sink's HDR description, when its EDID advertises PQ.
    pub hdr_display: Option<crate::wayland::edid::HdrDisplayInfo>,
    /// The configured mode this output was brought up under, if any.
    pub mode_intent: Option<settings_store::hardware::OutputMode>,
    /// The crtc this output was modeset onto, so a later hotplug can pick one still free.
    pub crtc: crtc::Handle,
    /// Live, modeset scanout bound to this connector's crtc.
    pub scanout: DrmScanout,
}

/// Open `card_path` once and build one [`DrmScanout`] per connected connector with a usable mode,
/// each on a distinct crtc sharing a dup of the single card fd — required since libseat refuses a
/// second `open` of an already-open node, and it means every crtc's flip completions arrive on one
/// event stream, drained once and routed by crtc. A connector with no free crtc/mode is logged and
/// skipped; `Err` only when nothing came up. The runtime normally retains the [`CardHandle`]
/// itself and calls [`CardHandle::bring_up_all`] for every seat GPU; this helper remains a
/// convenience for callers that need a complete one-card transaction.
///
/// # Errors
///
/// Returns `Err` if the card itself can't be opened, or if every connected connector
/// failed to come up (each connector's own modeset failure is logged and skipped rather than
/// propagated — this only errors when *none* came up).
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
                    u32::from(w).saturating_mul(u32::from(h))
                })
        })
        .or_else(|| modes.first())
        .copied()
}

/// Paint the pointer into a 64×64 ARGB8888 cursor buffer (`[B, G, R, A]`, little-endian), transparent where blank (entirely, for a hidden pointer); `pitch` is the row stride in bytes.
fn paint_cursor_argb(dst: &mut [u8], pitch: usize, bitmap: Option<&CursorBitmap>) {
    for b in dst.iter_mut() {
        *b = 0;
    }
    let Some(bitmap) = bitmap else {
        return;
    };
    for y in 0..bitmap.height {
        for x in 0..bitmap.width {
            let source = y
                .saturating_mul(bitmap.width)
                .saturating_add(x)
                .saturating_mul(4);
            let source = usize::try_from(source).unwrap_or(usize::MAX);
            let Some(&[red, green, blue, alpha]) = bitmap
                .rgba()
                .get(source..source.saturating_add(4))
                .and_then(|pixel| <&[u8; 4]>::try_from(pixel).ok())
            else {
                continue;
            };
            let destination = usize::try_from(y)
                .unwrap_or(usize::MAX)
                .saturating_mul(pitch)
                .saturating_add(usize::try_from(x).unwrap_or(usize::MAX).saturating_mul(4));
            if let Some(target) = dst.get_mut(destination..destination.saturating_add(4)) {
                target.copy_from_slice(&[blue, green, red, alpha]);
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
    bitmap: &CursorBitmap,
) {
    cursor::composite_xrgb(dst, pitch, dw, dh, px, py, bitmap);
}

/// Conservative clipped byte footprint of [`draw_cursor`]. The square bound intentionally covers
/// every scaled arrow byte without relying on the ASCII-art shape, so an old software cursor is
/// always restored from retained staging before a target is reused.
fn software_cursor_damage_rect(
    cursor: Option<(f64, f64)>,
    width: usize,
    height: usize,
    bitmap: Option<&CursorBitmap>,
) -> Option<FineDamageRect> {
    let (px, py) = cursor?;
    let bitmap = bitmap?;
    let span_x = i32::try_from(bitmap.width).ok()?;
    let span_y = i32::try_from(bitmap.height).ok()?;
    let x0 = pixel_coord(px).saturating_sub(i32::try_from(bitmap.hotspot.0).ok()?);
    let y0 = pixel_coord(py).saturating_sub(i32::try_from(bitmap.hotspot.1).ok()?);
    let right = x0.saturating_add(span_x);
    let bottom = y0.saturating_add(span_y);
    let width_i32 = i32::try_from(width).unwrap_or(i32::MAX);
    let height_i32 = i32::try_from(height).unwrap_or(i32::MAX);
    let left = x0.clamp(0, width_i32);
    let top = y0.clamp(0, height_i32);
    let right = right.clamp(0, width_i32);
    let bottom = bottom.clamp(0, height_i32);
    let rect_width = right.saturating_sub(left);
    let rect_height = bottom.saturating_sub(top);
    if rect_width == 0 || rect_height == 0 {
        return None;
    }
    Some(FineDamageRect {
        x: u32::try_from(left).ok()?,
        y: u32::try_from(top).ok()?,
        width: u32::try_from(rect_width).ok()?,
        height: u32::try_from(rect_height).ok()?,
    })
}

/// A dumb buffer mapped for as long as the CPU renderer writes it. Mapping
/// it for every frame costs milliseconds: the mmap, the page faults and the
/// TLB shootdown of the munmap, for a copy of a few hundred pixels.
struct CpuMapping {
    handle: u32,
    ptr: std::ptr::NonNull<u8>,
    len: usize,
}

// SAFETY: the mapping is shared memory of a buffer this scanout owns; the
// pointer is only dereferenced through `&mut self`.
unsafe impl Send for CpuMapping {}

impl CpuMapping {
    fn new(card: BorrowedFd<'_>, handle: u32, len: usize) -> std::io::Result<Self> {
        /// `DRM_IOCTL_MODE_MAP_DUMB`: `_IOWR('d', 0xB3, struct drm_mode_map_dumb)`.
        const MAP_DUMB: u32 = 0xC010_64B3;
        #[repr(C)]
        struct MapDumb {
            handle: u32,
            pad: u32,
            offset: u64,
        }
        let mut request = MapDumb {
            handle,
            pad: 0,
            offset: 0,
        };
        // SAFETY: the request is the kernel's `drm_mode_map_dumb` layout and
        // lives for the call.
        let result = unsafe { libc::ioctl(card.as_raw_fd(), MAP_DUMB.into(), &raw mut request) };
        if result != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let offset = libc::off_t::try_from(request.offset)
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidData))?;
        // SAFETY: a fresh shared mapping of the buffer's fake offset on the
        // card; the kernel checks `len` against the buffer size.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                card.as_raw_fd(),
                offset,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(std::io::Error::last_os_error());
        }
        let ptr = std::ptr::NonNull::new(ptr.cast::<u8>())
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidData))?;
        Ok(Self { handle, ptr, len })
    }

    fn bytes(&mut self) -> &mut [u8] {
        // SAFETY: `ptr` maps `len` bytes until `drop`, and `&mut self` is
        // the only access.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

impl Drop for CpuMapping {
    fn drop(&mut self) {
        // SAFETY: unmaps exactly the mapping made in `new`.
        unsafe { libc::munmap(self.ptr.as_ptr().cast(), self.len) };
    }
}

/// The [`Instant`] of a `CLOCK_MONOTONIC` page-flip timestamp.
pub fn instant_from_monotonic(timestamp: Duration) -> Instant {
    instant_at(timestamp, Instant::now(), monotonic_now())
}

/// `timestamp` placed on the `Instant` timeline through one paired reading
/// (`now`, `now_monotonic`) of both clocks. A timestamp from the future is
/// clamped to `now`.
fn instant_at(timestamp: Duration, now: Instant, now_monotonic: Duration) -> Instant {
    now.checked_sub(now_monotonic.saturating_sub(timestamp))
        .unwrap_or(now)
}

/// `CLOCK_MONOTONIC` now, comparable with page-flip event timestamps.
fn monotonic_now() -> Duration {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `now` is a valid timespec for the duration of the call.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut now) };
    Duration::new(
        u64::try_from(now.tv_sec).unwrap_or(0),
        u32::try_from(now.tv_nsec).unwrap_or(0),
    )
}

/// A tracked page-flip write remains reusable only if both the established transfer hint and the
/// flip request succeeded. The caller still submits the flip after a dirtyfb failure; this helper
/// describes only the destination-history commit decision.
fn pageflip_history_commit_is_safe(dirty_ok: bool, flip_ok: bool) -> bool {
    dirty_ok && flip_ok
}

/// Virtual CRTCs do not signal vblank. Page-flipping one waits out the kernel
/// helper (about 50 ms) inside the ioctl and holds the shared DRM file
/// description, so the logic thread cannot start the next frame until that
/// acceptance returns. Publish those scanouts with `dirty_framebuffer` on the
/// buffer already being scanned out.
fn pageflip_for_connector(interface: connector::Interface) -> bool {
    !matches!(interface, connector::Interface::Virtual)
}

/// Whether this connector should queue page flips. A connector that cannot be
/// reread keeps the physical default: dropping page flips on a real panel
/// would tear, and a virtual connector is still reported when the card answers.
fn connector_uses_pageflip(card: &Card, connector: connector::Handle) -> bool {
    match card.get_connector(connector, false) {
        Ok(info) => pageflip_for_connector(info.interface()),
        Err(_) => true,
    }
}

/// In-flight presents after one publish attempt.
///
/// A real connector's queued page flip stays in flight until
/// [`inflight_after_page_flip_event`]. A virtual scanout has no such event:
/// `dirty_framebuffer` retiring the clip (`dirty_accepted`) drops the count
/// even when a flip was queued, so the next frame can start.
fn inflight_after_publish(use_pageflip: bool, dirty_accepted: bool, page_flip_queued: bool) -> u8 {
    let virtual_clip_retired = !use_pageflip && dirty_accepted;
    u8::from(page_flip_queued && !virtual_clip_retired)
}

/// The page-flip event is the retirement for a connector that queued one.
fn inflight_after_page_flip_event(inflight: u8) -> u8 {
    inflight.saturating_sub(1)
}

/// `EBUSY` / `EAGAIN` mean a previous clip is still inside the kernel.
/// `set_crtc` waits on that same page-flip event; a virtual CRTC never sends
/// it, and the presenter would stay full.
fn dirty_errno_is_transient(errno: i32) -> bool {
    errno == libc::EBUSY || errno == libc::EAGAIN
}

/// Damage `(x, y, width, height)` as the clip `dirty_framebuffer` accepts.
///
/// `ClipRect` is the inclusive-origin exclusive-end rectangle `(x1, y1, x2, y2)`.
/// Passing the width and height as the end corner publishes the wrong region
/// whenever the transition clip does not start at the origin.
fn damage_to_clip(x: u16, y: u16, width: u16, height: u16) -> Option<ClipRect> {
    if width == 0 || height == 0 {
        return None;
    }
    let x2 = x.checked_add(width)?;
    let y2 = y.checked_add(height)?;
    Some(ClipRect::new(x, y, x2, y2))
}

/// A page flip that the kernel has not accepted yet owns both dumb buffers.
/// The next frame has to wait, or it would write a buffer the CRTC still
/// scans. A virtual scanout never takes that path: `dirty_framebuffer`
/// returns when the clip is accepted, and the following frame may start.
fn pending_pageflip_refuses_frame(use_pageflip: bool, flip_pending: bool) -> bool {
    inflight_after_publish(use_pageflip, !use_pageflip, flip_pending) > 0
}

/// A mapped scanout buffer becomes presentable only after the producer says
/// every pixel write completed. Keep this decision explicit and testable:
/// render/readback errors are frame drops, never permission to expose the
/// untouched back buffer.
fn paint_completed<E>(result: Result<(), E>) -> Result<bool, E> {
    result.map(|()| true)
}

#[cfg(test)]
mod physical_identity_tests {
    use super::PhysicalFlipIdentity;

    #[test]
    fn physical_completion_identity_stays_unavailable_after_timeout_or_pending_reset() {
        let mut normal = PhysicalFlipIdentity::default();
        assert!(normal.eligible);
        normal.cancel_pending(false);
        assert!(normal.eligible, "an idle restore has no outstanding event");
        normal.cancel_pending(true);
        assert!(
            !normal.eligible,
            "a late cancelled event cannot certify a newer frame"
        );
        let crtc = std::num::NonZeroU32::new(11).unwrap().into();
        let late = [smithay::reexports::drm::control::PageFlipEvent {
            crtc,
            frame: 42,
            duration: std::time::Duration::from_secs(123),
        }];
        assert!(
            super::super::physical_flip_proof(
                super::DirectRetirement::Complete,
                &late,
                crtc,
                normal.eligible
            )
            .is_none(),
            "even a real matching CRTC completion after cancellation cannot credit new pixels"
        );
        normal.cancel_pending(false);
        assert!(
            !normal.eligible,
            "later idle restoration cannot erase ambiguity"
        );
        let mut timed_out = PhysicalFlipIdentity::default();
        timed_out.uncertain_completion();
        assert!(!timed_out.eligible);
        timed_out.cancel_pending(false);
        assert!(!timed_out.eligible);
    }
}

#[cfg(test)]
mod tests {

    fn arrow(size: u32) -> std::sync::Arc<CursorBitmap> {
        cursor::bitmap(CursorShape::Default, size)
    }
    use super::*;
    use ui::ColorCompat as _;

    #[test]
    fn a_flip_timestamp_lands_on_the_instant_timeline_at_its_age() {
        let now = Instant::now();
        let clock = Duration::from_secs(5_000);
        let vblank = clock.checked_sub(Duration::from_micros(2_500)).unwrap();
        assert_eq!(
            instant_at(vblank, now, clock),
            now.checked_sub(Duration::from_micros(2_500)).unwrap(),
            "a vblank 2.5 ms before the paired reading is 2.5 ms before `now`"
        );
        assert_eq!(
            instant_at(clock + Duration::from_millis(1), now, clock),
            now,
            "a stamp from the future is clamped rather than scheduling ahead of it"
        );
    }

    #[test]
    fn only_connected_real_display_connectors_with_modes_are_scanout_eligible() {
        // Phase `connected_virtual_connector_with_usable_mode_is_scanout_eligible`.
        assert!(connector_is_scanout_eligible(
            connector::State::Connected,
            connector::Interface::Virtual,
            true
        ));
        // Phase `disconnected_connector_is_not_scanout_eligible`.
        assert!(!connector_is_scanout_eligible(
            connector::State::Disconnected,
            connector::Interface::HDMIA,
            true
        ));
        // Phase `connector_without_usable_mode_is_not_scanout_eligible`.
        assert!(!connector_is_scanout_eligible(
            connector::State::Connected,
            connector::Interface::HDMIA,
            false
        ));
        // Phase `writeback_connector_is_not_scanout_eligible`.
        assert!(!connector_is_scanout_eligible(
            connector::State::Connected,
            connector::Interface::Writeback,
            true
        ));
        // Phase `unknown_connector_is_not_scanout_eligible`.
        assert!(!connector_is_scanout_eligible(
            connector::State::Connected,
            connector::Interface::Unknown,
            true
        ));
        // Phase `usb_c_display_interfaces_are_scanout_targets`.
        assert!(is_display_connector(connector::Interface::DisplayPort));
        assert!(is_display_connector(connector::Interface::USB));
        assert!(is_display_connector(connector::Interface::HDMIA));
        assert!(!is_display_connector(connector::Interface::Writeback));
        assert!(!is_display_connector(connector::Interface::Unknown));
    }

    #[test]
    fn virtual_scanout_presents_the_next_frame_without_waiting_for_vblank() {
        // Phase `a_virtual_connector_publishes_without_waiting_for_vblank`.
        assert!(
            !pageflip_for_connector(connector::Interface::Virtual),
            "a virtual CRTC has no vblank; page-flipping it stalls the next frame"
        );
        // Phase `a_panel_connector_still_waits_until_its_page_flip_is_accepted`.
        assert!(pageflip_for_connector(connector::Interface::HDMIA));
        assert!(pageflip_for_connector(connector::Interface::DisplayPort));
        assert!(pageflip_for_connector(connector::Interface::USB));
        assert!(pending_pageflip_refuses_frame(true, true));
        assert!(!pending_pageflip_refuses_frame(true, false));
        // The clip upload (`present_no_drain_result`) and the single-output
        // interaction path (`present_scene_damage`) both ask this gate.
        // Phase `a_transition_clip_starts_while_a_virtual_flip_would_still_be_pending`.
        let virtual_pageflip = pageflip_for_connector(connector::Interface::Virtual);
        assert!(!pending_pageflip_refuses_frame(virtual_pageflip, true));
        // Phase `an_interaction_frame_starts_before_virtual_flip_acceptance`.
        assert!(!pending_pageflip_refuses_frame(virtual_pageflip, true));
    }

    #[test]
    fn virtual_dirty_accept_retires_without_a_page_flip_event() {
        // Phase `an_accepted_virtual_clip_drops_the_in_flight_count`.
        assert_eq!(
            inflight_after_publish(false, true, true),
            0,
            "dirty_framebuffer acceptance retires a virtual present; the page-flip event never arrives"
        );
        assert_eq!(inflight_after_publish(false, true, false), 0);
        assert!(
            !pending_pageflip_refuses_frame(false, true),
            "the next transition frame starts while a virtual flip would still be pending"
        );
        // Phase `a_real_connector_stays_in_flight_until_the_page_flip_event`.
        assert_eq!(inflight_after_publish(true, true, true), 1);
        assert_eq!(inflight_after_publish(true, false, true), 1);
        assert_eq!(inflight_after_page_flip_event(1), 0);
        assert!(pending_pageflip_refuses_frame(true, true));
        assert!(!pending_pageflip_refuses_frame(true, false));
        // Phase `a_busy_dirty_does_not_fall_through_to_a_blocking_modeset`.
        assert!(dirty_errno_is_transient(libc::EBUSY));
        assert!(dirty_errno_is_transient(libc::EAGAIN));
        assert!(!dirty_errno_is_transient(libc::ENOSYS));
        assert!(!dirty_errno_is_transient(0));
        // Phase `a_transition_clip_is_the_rectangle_dirty_framebuffer_accepts`.
        let clip = damage_to_clip(80, 40, 300, 200).expect("non-empty damage");
        assert_eq!(
            (clip.x1(), clip.y1(), clip.x2(), clip.y2()),
            (80, 40, 380, 240)
        );
    }

    #[test]
    fn direct_scanout_device_identity_must_match_exactly() {
        // Phase `card_and_udev_share_the_same_dev_t_identity`.
        {
            // UdevEvent carries libc::dev_t and Card::dev_id reads
            // fstat(card_fd).st_rdev, also libc::dev_t.  This conversion is the
            // one used by both paths before looking in `DrmDispatchState::cards`.
            let dev_t: libc::dev_t = 0x10203;
            assert_eq!(device_id_from_dev_t(dev_t), 0x10203);
        }
        // Phase `pci_bdf_parser_accepts_vulkan_and_drm_forms`.
        {
            let expected = PciBdf {
                domain: 0,
                bus: 3,
                device: 0,
                function: 1,
            };
            assert_eq!(parse_pci_bdf("0000:03:00.1"), Some(expected));
            assert_eq!(parse_pci_bdf("pci:0000:03:00.1"), Some(expected));
            assert_eq!(parse_pci_bdf("PCI:0000:03:00.1"), None);
        }
        // Phase `direct_scanout_requires_the_same_pci_function`.
        assert!(renderer_matches_drm_card(
            "0000:03:00.0",
            "pci:0000:03:00.0"
        ));
        assert!(!renderer_matches_drm_card(
            "0000:03:00.0",
            "pci:0000:04:00.0"
        ));
        assert!(!renderer_matches_drm_card(
            "0000:03:00.0",
            "pci:0000:03:00.1"
        ));
        // Phase `direct_scanout_fails_closed_for_missing_or_malformed_identity`.
        for identity in [
            "",
            "03:00.0",
            "0000:03:00",
            "0000:03:00.8",
            "sysfs-platform-gpu",
        ] {
            assert!(!renderer_matches_drm_card(identity, "pci:0000:03:00.0"));
            assert!(!renderer_matches_drm_card("0000:03:00.0", identity));
        }
    }

    #[cfg(unix)]
    #[test]
    fn sysfs_pci_identity_is_bound_to_the_exact_pci_device() {
        // Phase `sysfs_pci_identity_is_bound_to_the_exact_device_number`.
        {
            use std::os::unix::fs::symlink;

            let root = tempfile::tempdir().expect("temp sysfs");
            let sys_dev_char = root.path().join("dev/char");
            let pci_device = root.path().join("devices/0000:66:00.0");
            std::fs::create_dir_all(&sys_dev_char).expect("dev char root");
            std::fs::create_dir_all(&pci_device).expect("pci device");

            let dev = smithay::reexports::rustix::fs::makedev(226, 1);
            let node = sys_dev_char.join("226:1");
            std::fs::create_dir(&node).expect("char node");
            symlink(&pci_device, node.join("device")).expect("device link");

            assert_eq!(
                sysfs_pci_bus_id_at(&sys_dev_char, dev).as_deref(),
                Some("pci:0000:66:00.0")
            );
            assert_eq!(
                sysfs_pci_bus_id_at(
                    &sys_dev_char,
                    smithay::reexports::rustix::fs::makedev(226, 0)
                ),
                None,
                "another card's dev_t must not inherit this identity"
            );
        }
        // Phase `sysfs_pci_identity_rejects_non_pci_and_malformed_devices`.
        {
            use std::os::unix::fs::symlink;

            let root = tempfile::tempdir().expect("temp sysfs");
            let sys_dev_char = root.path().join("dev/char");
            std::fs::create_dir_all(&sys_dev_char).expect("dev char root");

            for (minor, component) in [(1, "platform-gpu"), (2, "0000:66:00.8")] {
                let target = root.path().join("devices").join(component);
                let node = sys_dev_char.join(format!("226:{minor}"));
                std::fs::create_dir_all(&target).expect("device target");
                std::fs::create_dir(&node).expect("char node");
                symlink(&target, node.join("device")).expect("device link");
                assert_eq!(
                    sysfs_pci_bus_id_at(
                        &sys_dev_char,
                        smithay::reexports::rustix::fs::makedev(226, minor)
                    ),
                    None
                );
            }
        }
    }

    #[test]
    fn direct_scanout_slots_never_reuse_busy_buffers_and_fail_safe() {
        // Phase `direct_scanout_never_renders_front_or_flip_pending_slots`.
        {
            let mut state = DirectScanoutState::new(0);
            assert_eq!(state.render_slot(), Some(1));
            assert!(state.submit(1));
            assert_eq!(state.render_slot(), None, "both slots are owned by KMS");
            assert!(
                !state.submit(0),
                "front cannot be submitted as fresh output"
            );
            assert!(!state.submit(1), "pending slot cannot be submitted twice");

            assert!(state.complete());
            assert_eq!(state.render_slot(), Some(0));
            assert_eq!(state.front, 1);
        }
        // Phase `direct_scanout_renderer_or_pageflip_failure_keeps_back_slot_available`.
        {
            let state = DirectScanoutState::new(0);
            let slot = state.render_slot().expect("initial back slot");

            // Renderer failure performs no transition and therefore cannot expose
            // the unfinished target. A failed page_flip likewise never calls
            // `submit`, so the same slot remains available for a later frame.
            assert_eq!(state.render_slot(), Some(slot));
            assert_eq!(state.pending, None);
            assert_eq!(state.front, 0);

            // A page-flip failure does not call `submit`, so it is exactly the
            // same no-transition state as a renderer failure.
            assert_eq!(state.render_slot(), Some(slot));
            assert_eq!(state.front, 0);
        }
        // Phase `direct_scanout_completion_alternates_two_slots`.
        {
            let mut state = DirectScanoutState::new(0);
            for expected in [1, 0, 1, 0] {
                assert_eq!(state.render_slot(), Some(expected));
                assert!(state.submit(expected));
                assert!(state.complete());
                assert_eq!(state.front, expected);
            }
            assert!(!state.complete(), "duplicate completion is ignored");
        }
        // Phase `direct_retirement_waits_for_the_exact_crtc`.
        {
            let awaited = crtc::Handle::from(std::num::NonZeroU32::new(7).unwrap());
            let sibling = crtc::Handle::from(std::num::NonZeroU32::new(9).unwrap());

            assert_eq!(retirement_after_poll(awaited, &[], false), None);
            assert_eq!(retirement_after_poll(awaited, &[sibling], false), None);
            assert_eq!(
                retirement_after_poll(awaited, &[sibling, awaited], false),
                Some(DirectRetirement::Complete)
            );
            assert_eq!(
                retirement_after_poll(awaited, &[sibling], true),
                Some(DirectRetirement::TimedOut)
            );
        }
        // Phase `pageflip_history_failure_does_not_skip_the_flip_decision`.
        assert!(pageflip_history_commit_is_safe(true, true));
        assert!(!pageflip_history_commit_is_safe(false, true));
        assert!(!pageflip_history_commit_is_safe(true, false));
        assert!(!pageflip_history_commit_is_safe(false, false));
        // Phase `failed_paint_never_authorizes_presenting_the_back_buffer`.
        assert_eq!(paint_completed::<&str>(Ok(())), Ok(true));
        assert_eq!(
            paint_completed(Err("readback timed out")),
            Err("readback timed out")
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn card_and_connector_config_identity_survives_reenumeration() {
        // Phase `card_config_identity_prefers_bus_then_stable_sysfs_before_dev_t`.
        assert_eq!(
            config_identity(
                Some("pci:0000:03:00.0".into()),
                Some("sysfs-0000:01:00.0".into()),
                77,
            ),
            "pci:0000:03:00.0"
        );
        assert_eq!(
            config_identity(None, Some("sysfs-0000:01:00.0".into()), 77),
            "sysfs-0000:01:00.0"
        );
        assert_eq!(
            config_identity(None, None, 77),
            "drm-dev-77",
            "the final dev_t fallback is intentionally session-local"
        );
        // Phase `card0_to_card1_reenumeration_matches_the_same_physical_gpu_before_open`.
        {
            // `dev_t` and card-node names are attachment identifiers; an eGPU
            // may get both a new one during removal. The canonical sysfs identity
            // remains the pre-open guard that prevents a second libseat lease.
            let old_node = "/dev/dri/card0";
            let new_node = "/dev/dri/card1";
            assert_ne!(old_node, new_node);
            let old_dev_t = 226_u64;
            let new_dev_t = 227_u64;
            assert_ne!(old_dev_t, new_dev_t);
            assert!(same_physical_card_identity(
                Some("sysfs-0000:03:00.0"),
                Some("sysfs-0000:03:00.0")
            ));
            assert!(!same_physical_card_identity(
                Some("sysfs-0000:03:00.0"),
                Some("sysfs-0000:04:00.0")
            ));
        }
        // Phase `duplicate_connector_labels_become_card_qualified_but_unique_stay_legacy`.
        {
            let first = DrmOutputKey {
                device: 10,
                connector: 7,
            };
            let second = DrmOutputKey {
                device: 11,
                connector: 7,
            };
            let panel = DrmOutputKey {
                device: 10,
                connector: 8,
            };
            let keys = resolve_connector_config_keys(
                [
                    ConnectorKeyCandidate {
                        key: first,
                        connector_name: "HDMI-A-1",
                        card_identity: "pci:0000:01:00.0",
                    },
                    ConnectorKeyCandidate {
                        key: second,
                        connector_name: "HDMI-A-1",
                        card_identity: "pci:0000:03:00.0",
                    },
                    ConnectorKeyCandidate {
                        key: panel,
                        connector_name: "eDP-1",
                        card_identity: "pci:0000:01:00.0",
                    },
                ],
                &settings_store::hardware::DisplayConfig::default(),
            );
            assert_eq!(
                keys[&first], "pci:0000:01:00.0/HDMI-A-1",
                "first duplicate must not retain an ambiguous legacy key"
            );
            assert_eq!(keys[&second], "pci:0000:03:00.0/HDMI-A-1");
            assert_eq!(
                keys[&panel], "eDP-1",
                "single-GPU and unique-connector configs stay unchanged"
            );
        }
        // Phase `qualified_connector_config_survives_duplicate_peer_unplug_and_replug`.
        {
            use settings_store::hardware::{DisplayConfig, OutputConfig, OutputMode};

            let first = DrmOutputKey {
                device: 10,
                connector: 7,
            };
            let second = DrmOutputKey {
                device: 11,
                connector: 7,
            };
            let first_key = "pci:0000:01:00.0/HDMI-A-1";
            let second_key = "pci:0000:03:00.0/HDMI-A-1";
            let first_intent = OutputConfig {
                enabled: false,
                primary: true,
                x: 320,
                y: 180,
                scale: 1.5,
                mode: Some(OutputMode {
                    width: 2560,
                    height: 1440,
                    refresh_mhz: 120_000,
                }),
                mirror_of: Some(second_key.into()),
                ..Default::default()
            };
            let mut config = DisplayConfig::default();
            config
                .outputs
                .insert(first_key.into(), first_intent.clone());
            config
                .outputs
                .insert(second_key.into(), OutputConfig::default());
            let first_candidate = ConnectorKeyCandidate {
                key: first,
                connector_name: "HDMI-A-1",
                card_identity: "pci:0000:01:00.0",
            };
            let second_candidate = ConnectorKeyCandidate {
                key: second,
                connector_name: "HDMI-A-1",
                card_identity: "pci:0000:03:00.0",
            };

            let collided =
                resolve_connector_config_keys([first_candidate, second_candidate], &config);
            assert_eq!(collided[&first], first_key);
            assert_eq!(collided[&second], second_key);

            // With only the first connector still attached, retaining its saved
            // qualified key is what keeps all of its confirmed intent intact.
            let unplugged = resolve_connector_config_keys([first_candidate], &config);
            assert_eq!(unplugged[&first], first_key);
            assert_eq!(
                config.output_for(&unplugged[&first]),
                Some(&first_intent),
                "enabled/mode/scale/position/mirror intent must remain addressable"
            );

            let replugged =
                resolve_connector_config_keys([first_candidate, second_candidate], &config);
            assert_eq!(replugged[&first], first_key);
            assert_eq!(replugged[&second], second_key);
        }
    }

    #[test]
    fn cursor_plane_hotspot_follows_the_output_scale() {
        assert_eq!(scaled_hotspot((3, 5), 1.0), (3, 5));
        assert_eq!(scaled_hotspot((3, 5), 2.0), (6, 10));
        assert_eq!(scaled_hotspot((3, 5), 1.5), (5, 8));
        assert_eq!(scaled_hotspot((3, 5), f64::NAN), (3, 5));
    }

    #[test]
    fn software_cursor_writes_clips_and_reports_damage() {
        // Phase `cursor_bitmap_writes_opaque_and_transparent_pixels`.
        {
            // 64×64 ARGB, tight pitch.
            let pitch = (CURSOR_DIM * 4) as usize;
            let mut buf = vec![0xCDu8; pitch * CURSOR_DIM as usize];
            paint_cursor_argb(&mut buf, pitch, Some(&arrow(24)));

            // The explicit (0,0) hotspot sits immediately beside the first
            // antialiased dark outline; it need not force an opaque corner.
            let (fill, outline) = cursor::palette();
            let outline_max = outline.r().max(outline.g()).max(outline.b());
            assert_eq!(cursor::bitmap(CursorShape::Default, 24).hotspot, (0, 0));
            assert!(
                buf[..8 * pitch].chunks_exact(4).any(|pixel| pixel[3] > 0
                    && pixel[0] <= outline_max
                    && pixel[1] <= outline_max
                    && pixel[2] <= outline_max),
                "tip neighbourhood should contain the dark outline"
            );

            // A blank area (far bottom-right) stays fully transparent.
            let o = 60 * pitch + 60 * 4;
            assert_eq!(
                &buf[o..o + 4],
                &[0, 0, 0, 0],
                "blank cursor area is transparent"
            );

            // There is at least one fully covered fill pixel somewhere (BGRA).
            let any_fill = buf
                .chunks_exact(4)
                .any(|p| p == [fill.b(), fill.g(), fill.r(), 0xFF]);
            assert!(any_fill, "cursor must contain opaque fill pixels");
        }
        // Phase `software_cursor_draws_into_xrgb_buffer`.
        {
            // 32×32 XRGB destination, clamp-safe.
            let (w, h) = (32usize, 32usize);
            let pitch = w * 4;
            let mut dst = vec![0u8; pitch * h];
            draw_cursor(&mut dst, pitch, w, h, 4, 4, &arrow(24));
            assert!(
                dst.chunks_exact(4).any(|pixel| pixel[3] == 0xFF),
                "cursor should composite visible pixels"
            );
            // A pixel far from the cursor is untouched (still zero).
            assert_eq!(&dst[0..4], &[0, 0, 0, 0]);
        }
        // Phase `software_cursor_clamps_at_edges_without_panic`.
        {
            // Drawing partially off the right/bottom edge must not panic.
            let (w, h) = (20usize, 20usize);
            let pitch = w * 4;
            let mut dst = vec![0u8; pitch * h];
            draw_cursor(&mut dst, pitch, w, h, 18, 18, &arrow(24));
            draw_cursor(&mut dst, pitch, w, h, -3, -3, &arrow(24));
        }
        // Phase `software_cursor_damage_footprint_covers_every_clipped_write`.
        {
            let (width, height) = (80_usize, 80_usize);
            let pitch = width * 4;
            for cursor in [Some((4.0, 5.0)), Some((73.0, 75.0)), Some((-8.0, -6.0))] {
                let mut bytes = vec![0xA7; pitch * height];
                let before = bytes.clone();
                if let Some((x, y)) = cursor {
                    draw_cursor(
                        &mut bytes,
                        pitch,
                        width,
                        height,
                        pixel_coord(x),
                        pixel_coord(y),
                        &arrow(48),
                    );
                }
                let footprint =
                    software_cursor_damage_rect(cursor, width, height, Some(&arrow(48)))
                        .expect("partially visible cursor has a restoration footprint");
                for (offset, (actual, original)) in bytes.iter().zip(&before).enumerate() {
                    if actual == original {
                        continue;
                    }
                    let pixel = offset / 4;
                    let x = pixel % width;
                    let y = pixel / width;
                    assert!(
                        x >= usize::try_from(footprint.x).expect("test x")
                            && x < usize::try_from(footprint.x + footprint.width)
                                .expect("test right")
                            && y >= usize::try_from(footprint.y).expect("test y")
                            && y < usize::try_from(footprint.y + footprint.height)
                                .expect("test bottom"),
                        "cursor write ({x},{y}) must be restored by its footprint {footprint:?}"
                    );
                }
            }
            assert_eq!(
                software_cursor_damage_rect(None, width, height, Some(&arrow(24))),
                None
            );
        }
    }

    #[test]
    #[allow(
        clippy::arithmetic_side_effects,
        clippy::too_many_lines,
        reason = "the cursor fixture keeps both destination-age transactions next to their exact same-staging restoration checks"
    )]
    fn two_target_history_restores_moved_and_hidden_software_cursor_from_staging() {
        use crate::compositor::backend::drm::paint::XrgbCpuWriteMode;
        use vello::kurbo::{Affine, Rect};
        use vello::peniko::{Color, Fill};

        const WIDTH: u32 = 128;
        const HEIGHT: u32 = 96;
        let mut bridge = match VelloDrmBridge::new(WIDTH, HEIGHT) {
            Ok(bridge) => bridge,
            Err(BridgeError::NoAdapter(message)) => {
                eprintln!("skipping cursor history test: {message}");
                return;
            }
            Err(error) => panic!("unexpected bridge error: {error:?}"),
        };
        bridge.warm_renderer_for_test();
        // The fixture checks restored pixels, not frame latency.
        bridge.wait_for_gpu_completion_for_test();
        let width = usize::try_from(WIDTH).expect("test width");
        let height = usize::try_from(HEIGHT).expect("test height");
        let pitch = width.checked_mul(4).expect("test pitch") + 16;
        let len = pitch.checked_mul(height).expect("test length");
        let identity = bridge
            .xrgb_history_identity(pitch, 2)
            .expect("test identity")
            .with_scanout_epoch(3);
        let mut history = XrgbTargetHistory::new(2);
        let mut targets = [vec![0xA7; len], vec![0xB8; len]];
        let mut cursor_slots = [None, None];
        let first_cursor = Some((4.0, 5.0));
        let moved_cursor = Some((58.0, 12.0));
        let a = FineDamageRect {
            x: 8,
            y: 10,
            width: 12,
            height: 9,
        };
        let b = FineDamageRect {
            x: 88,
            y: 35,
            width: 11,
            height: 8,
        };
        let c = FineDamageRect {
            x: 42,
            y: 68,
            width: 9,
            height: 7,
        };
        let scene_for = |rects: &[(FineDamageRect, Color)]| {
            let mut scene = Scene::new();
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                Color::from_rgb8(0x11, 0x22, 0x33),
                None,
                &Rect::new(0.0, 0.0, f64::from(WIDTH), f64::from(HEIGHT)),
            );
            for (damage, color) in rects {
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    *color,
                    None,
                    &Rect::new(
                        f64::from(damage.x),
                        f64::from(damage.y),
                        f64::from(damage.x.saturating_add(damage.width)),
                        f64::from(damage.y.saturating_add(damage.height)),
                    ),
                );
            }
            scene
        };
        let initial_scene = scene_for(&[]);
        let first_scene = scene_for(&[(a, Color::from_rgb8(0xC0, 0x50, 0x20))]);
        let second_scene = scene_for(&[
            (a, Color::from_rgb8(0xC0, 0x50, 0x20)),
            (b, Color::from_rgb8(0x20, 0x80, 0xD0)),
        ]);
        let hidden_scene = scene_for(&[
            (a, Color::from_rgb8(0xC0, 0x50, 0x20)),
            (b, Color::from_rgb8(0x20, 0x80, 0xD0)),
            (c, Color::from_rgb8(0x60, 0xD0, 0x70)),
        ]);
        let regions_a = [a];
        let regions_b = [b];
        let regions_c = [c];

        let initial = SceneDamage {
            revision: 1,
            previous_revision: None,
            regions: Some(&regions_a),
        };
        let transaction = history.prepare(
            0,
            Some(identity),
            initial.revision,
            initial.previous_revision,
            initial.regions,
            &[],
        );
        let write = bridge
            .paint_damage_into_xrgb8888_with_history(
                &initial_scene,
                initial,
                &transaction,
                &mut targets[0],
                pitch,
            )
            .expect("seed target zero");
        draw_cursor(&mut targets[0], pitch, width, height, 4, 5, &arrow(24));
        cursor_slots[0] =
            software_cursor_damage_rect(first_cursor, width, height, Some(&arrow(24)));
        transaction.commit(if write.damage_admitted {
            XrgbHistoryCommit::DamageAccepted
        } else {
            XrgbHistoryCommit::FullBoundary
        });

        let first = SceneDamage {
            revision: 2,
            previous_revision: Some(1),
            regions: Some(&regions_a),
        };
        let transaction = history.prepare(
            1,
            Some(identity),
            first.revision,
            first.previous_revision,
            first.regions,
            cursor_slots[1].as_slice(),
        );
        let write = bridge
            .paint_damage_into_xrgb8888_with_history(
                &first_scene,
                first,
                &transaction,
                &mut targets[1],
                pitch,
            )
            .expect("seed target one");
        draw_cursor(&mut targets[1], pitch, width, height, 4, 5, &arrow(24));
        cursor_slots[1] =
            software_cursor_damage_rect(first_cursor, width, height, Some(&arrow(24)));
        transaction.commit(if write.damage_admitted {
            XrgbHistoryCommit::DamageAccepted
        } else {
            XrgbHistoryCommit::FullBoundary
        });

        let moved = SceneDamage {
            revision: 3,
            previous_revision: Some(2),
            regions: Some(&regions_b),
        };
        let transaction = history.prepare(
            0,
            Some(identity),
            moved.revision,
            moved.previous_revision,
            moved.regions,
            cursor_slots[0].as_slice(),
        );
        let write = bridge
            .paint_damage_into_xrgb8888_with_history(
                &second_scene,
                moved,
                &transaction,
                &mut targets[0],
                pitch,
            )
            .expect("restore old cursor and draw moved cursor");
        assert_eq!(write.mode, XrgbCpuWriteMode::Partial);
        draw_cursor(&mut targets[0], pitch, width, height, 58, 12, &arrow(24));
        cursor_slots[0] =
            software_cursor_damage_rect(moved_cursor, width, height, Some(&arrow(24)));
        transaction.commit(if write.damage_admitted {
            XrgbHistoryCommit::DamageAccepted
        } else {
            XrgbHistoryCommit::FullBoundary
        });
        let mut moved_reference = targets[0].clone();
        bridge
            .swizzle_current_staging_for_test(&mut moved_reference, pitch)
            .expect("moved cursor staging reference");
        draw_cursor(
            &mut moved_reference,
            pitch,
            width,
            height,
            58,
            12,
            &arrow(24),
        );
        assert_eq!(
            targets[0]
                .iter()
                .zip(&moved_reference)
                .position(|(a, b)| a != b),
            None,
            "moved cursor target differs from current staging reference"
        );

        let hidden = SceneDamage {
            revision: 4,
            previous_revision: Some(3),
            regions: Some(&regions_c),
        };
        let transaction = history.prepare(
            1,
            Some(identity),
            hidden.revision,
            hidden.previous_revision,
            hidden.regions,
            cursor_slots[1].as_slice(),
        );
        let write = bridge
            .paint_damage_into_xrgb8888_with_history(
                &hidden_scene,
                hidden,
                &transaction,
                &mut targets[1],
                pitch,
            )
            .expect("restore old cursor when hidden");
        assert_eq!(write.mode, XrgbCpuWriteMode::Partial);
        cursor_slots[1] = None;
        transaction.commit(if write.damage_admitted {
            XrgbHistoryCommit::DamageAccepted
        } else {
            XrgbHistoryCommit::FullBoundary
        });
        let mut hidden_reference = targets[1].clone();
        bridge
            .swizzle_current_staging_for_test(&mut hidden_reference, pitch)
            .expect("hidden cursor staging reference");
        assert_eq!(
            targets[1]
                .iter()
                .zip(&hidden_reference)
                .position(|(a, b)| a != b),
            None,
            "hidden cursor target differs from current staging reference"
        );
        assert_eq!(
            cursor_slots[1], None,
            "hidden cursor retains no stale footprint"
        );
    }

    #[test]
    fn cursor_routing_picks_the_output_under_the_pointer() {
        // Phase `cursor_routing_single_output_is_global_coords`.
        {
            // One output at the origin contains every in-bounds point, so routing always selects index 0.
            let rects = [(0, 0, 1920u32, 1080u32)];
            assert_eq!(output_index_at(&rects, 0, 0), Some(0));
            assert_eq!(output_index_at(&rects, 1919, 1079), Some(0));
            assert_eq!(output_index_at(&rects, 1920, 0), None);
        }
        // Phase `cursor_routing_picks_output_under_pointer`.
        {
            // Two 1920x1080 outputs side by side: [0,1920) -> output 0, [1920,3840) -> output 1 — decides which crtc's cursor plane a pointer move drives.
            let rects = [(0, 0, 1920u32, 1080u32), (1920, 0, 1920u32, 1080u32)];
            assert_eq!(output_index_at(&rects, 10, 10), Some(0));
            assert_eq!(output_index_at(&rects, 1919, 500), Some(0));
            assert_eq!(output_index_at(&rects, 1920, 500), Some(1));
            assert_eq!(output_index_at(&rects, 3000, 500), Some(1));
            assert_eq!(output_index_at(&rects, 3840, 500), None);
        }
        // Phase `cursor_routing_gap_between_mismatched_heights_is_none`.
        {
            // A shorter second output leaves a gap below it; a point in that gap belongs to no output and must not panic / mis-route.
            let rects = [(0, 0, 1920u32, 1080u32), (1920, 0, 1280u32, 720u32)];
            assert_eq!(output_index_at(&rects, 2000, 700), Some(1));
            assert_eq!(output_index_at(&rects, 2000, 800), None);
        }
        // Phase `cursor_coordinates_follow_output_rotation`.
        {
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

    #[test]
    fn resume_keeps_logind_granted_master_without_set_master() {
        use super::super::session::{SessionAction, SessionState};
        // Phase `resume_from_sleep_on_a_master_fd_issues_no_set_master`.
        {
            // logind opened the node and keeps master across suspend. The
            // kernel refuses SET_MASTER from this unprivileged process with
            // EACCES even on that master fd, so the resume sequence must
            // only probe it.
            let session = SessionState::new_active();
            let mut set_master_calls = 0;
            for action in session.resume_actions() {
                if action == SessionAction::AcquireMaster {
                    let outcome = reclaim_master(Ok(true), || {
                        set_master_calls += 1;
                        Err(std::io::Error::from_raw_os_error(libc::EACCES))
                    });
                    assert!(matches!(outcome, MasterReclaim::AlreadyHeld));
                }
            }
            assert_eq!(set_master_calls, 0);
        }
        // Phase `fd_without_master_still_asks_the_kernel`.
        {
            let mut asked = false;
            let outcome = reclaim_master(Ok(false), || {
                asked = true;
                Ok(())
            });
            assert!(asked);
            assert!(matches!(outcome, MasterReclaim::Acquired));
        }
        // Phase `refused_set_master_is_reported_not_granted`.
        {
            let outcome = reclaim_master(Ok(false), || {
                Err(std::io::Error::from_raw_os_error(libc::EACCES))
            });
            assert!(matches!(
                outcome,
                MasterReclaim::NotGranted(ref e) if e.raw_os_error() == Some(libc::EACCES)
            ));
        }
        // Phase `unprobeable_fd_falls_back_to_set_master`.
        {
            // A non-DRM fd cannot answer the probe; that is "unknown", not "master".
            let null = std::fs::File::open("/dev/null").expect("open /dev/null");
            let probe = fd_is_drm_master(null.as_fd());
            assert!(
                probe.is_err(),
                "a non-DRM fd must not read as master: {probe:?}"
            );
            let mut asked = false;
            let _ = reclaim_master(probe, || {
                asked = true;
                Ok(())
            });
            assert!(asked);
        }
    }
}
