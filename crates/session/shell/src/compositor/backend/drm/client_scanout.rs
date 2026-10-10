//! Scanning a fullscreen client's own buffer out.
//!
//! When the frame builder proves a client image is on top of everything on
//! an output, pixel for pixel ([`crate::shell::scanout_candidate`]), the
//! direct path can page-flip the client's dma-buf instead of rendering the
//! frame: no raster, no copy into a scanout slot, one flip. This module holds
//! the parts of that route that touch KMS — the primary plane's formats, the
//! framebuffer made from a client buffer, the `TEST_ONLY` commit that proves
//! the plane takes it — and the pure rules that decide when the route is
//! taken ([`client_scanout_rejection`]) and how long a buffer is held
//! ([`ClientHold`]).
//!
//! Any refusal composites the same frame instead, so the output never shows
//! anything but the frame or the frame before it.

use std::sync::{Arc, OnceLock};

use smithay::backend::allocator::Buffer as _;
use smithay::backend::allocator::dmabuf::{Dmabuf, WeakDmabuf};
use smithay::reexports::drm as drm_re;

use drm_re::ClientCapability;
use drm_re::Device as BasicDevice;
use drm_re::buffer::{self as drm_buffer, DrmFourcc, DrmModifier, PlanarBuffer};
use drm_re::control::atomic::AtomicModeReq;
use drm_re::control::{
    AtomicCommitFlags, Device as ControlDevice, FbCmd2Flags, crtc, framebuffer, plane, property,
};

use super::output_signal::find_property;
use super::scanout::Card;
use crate::compositor::backend::OutputId;
use crate::shell::gpu_frames::GpuFrame;

/// Set to `0` to keep every frame composited: a hardware A/B switch and an
/// escape hatch for a driver that misbehaves with client framebuffers.
pub const CLIENT_SCANOUT_ENV: &str = "TILO_CLIENT_SCANOUT";

/// Client buffers whose framebuffer (or refusal) is remembered per output: a
/// swapchain of up to four, and the buffers of the size it is leaving.
const REMEMBERED_BUFFERS: usize = 8;

/// `DRM_PLANE_TYPE_PRIMARY`.
const PLANE_TYPE_PRIMARY: u64 = 1;

/// `DRM_FORMAT_MOD_LINEAR`.
const MODIFIER_LINEAR: u64 = 0;

/// Whether client buffers may be scanned out at all ([`CLIENT_SCANOUT_ENV`]).
pub fn client_scanout_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os(CLIENT_SCANOUT_ENV).is_none_or(|value| value != "0"))
}

/// Everything known about one frame's candidate on one output, before KMS is
/// asked anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent facts about one candidate, reported together when its scanout is refused"
)]
pub(super) struct ClientEligibility {
    /// Client scanout is not switched off ([`CLIENT_SCANOUT_ENV`]).
    pub enabled: bool,
    /// The output's scanout is upright: a client buffer flipped onto a
    /// turned output would show unturned.
    pub upright: bool,
    /// The candidate covers exactly this output, pixel for pixel.
    pub covers_output: bool,
    /// The candidate draws a client's dma-buf.
    pub client_buffer: bool,
    /// Its alpha is padding, so it covers what is below as the frame does.
    pub opaque: bool,
    /// The output is driven in SDR through 8-bit targets: the client's
    /// buffer is an SDR signal and needs no conversion.
    pub sdr: bool,
    /// The frame composites no HDR layer.
    pub no_hdr_layers: bool,
    /// The buffer has the format of the framebuffer KMS shows now. A legacy
    /// page flip cannot change the format.
    pub same_format: bool,
}

/// Why a frame's candidate cannot be scanned out on this output, or `None`
/// when KMS may be asked.
pub(super) fn client_scanout_rejection(input: ClientEligibility) -> Option<&'static str> {
    if !input.enabled {
        return Some("client scanout is switched off");
    }
    if !input.upright {
        return Some("the output is rotated");
    }
    if !input.covers_output {
        return Some("the fullscreen client does not cover this output pixel for pixel");
    }
    if !input.client_buffer {
        return Some("the fullscreen client's frame is not a dma-buf");
    }
    if !input.opaque {
        return Some("the fullscreen client's buffer has an alpha channel");
    }
    if !input.sdr || !input.no_hdr_layers {
        return Some("the output composites HDR");
    }
    if !input.same_format {
        return Some("the client's buffer format differs from the output's");
    }
    None
}

/// What the primary plane scans out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum PlaneFormats {
    /// From `IN_FORMATS`: every format with each modifier it takes.
    Modifiers(Vec<(u32, u64)>),
    /// A driver without modifiers: formats, in their implicit layout.
    Implicit(Vec<u32>),
}

impl PlaneFormats {
    /// Whether the plane takes `fourcc` laid out as `modifier`. An implicit
    /// modifier (`DRM_FORMAT_MOD_INVALID`) is the driver's own layout, so it
    /// is offered wherever the format is; the `TEST_ONLY` commit decides.
    pub fn supports(&self, fourcc: u32, modifier: u64) -> bool {
        let implicit = modifier == u64::from(DrmModifier::Invalid);
        match self {
            Self::Modifiers(pairs) => pairs
                .iter()
                .any(|&(format, with)| format == fourcc && (implicit || with == modifier)),
            Self::Implicit(formats) => {
                formats.contains(&fourcc) && (implicit || modifier == MODIFIER_LINEAR)
            }
        }
    }
}

/// Parse an `IN_FORMATS` blob (`struct drm_format_modifier_blob`): each
/// format with every modifier the plane takes it in. `None` when the blob is
/// malformed.
pub(super) fn parse_in_formats(blob: &[u8]) -> Option<Vec<(u32, u64)>> {
    let word = |at: usize| -> Option<u32> {
        let bytes = blob.get(at..at.checked_add(4)?)?;
        Some(u32::from_ne_bytes(bytes.try_into().ok()?))
    };
    let quad = |at: usize| -> Option<u64> {
        let bytes = blob.get(at..at.checked_add(8)?)?;
        Some(u64::from_ne_bytes(bytes.try_into().ok()?))
    };
    let count_formats = usize::try_from(word(8)?).ok()?;
    let formats_offset = usize::try_from(word(12)?).ok()?;
    let count_modifiers = usize::try_from(word(16)?).ok()?;
    let modifiers_offset = usize::try_from(word(20)?).ok()?;
    let formats = (0..count_formats)
        .map(|index| word(formats_offset.checked_add(index.checked_mul(4)?)?))
        .collect::<Option<Vec<u32>>>()?;
    let mut pairs = Vec::new();
    for index in 0..count_modifiers {
        // `struct drm_format_modifier`: a 64-bit format mask, the offset of
        // its first format, padding, the modifier.
        let base = modifiers_offset.checked_add(index.checked_mul(24)?)?;
        let mask = quad(base)?;
        let first = usize::try_from(word(base.checked_add(8)?)?).ok()?;
        let modifier = quad(base.checked_add(16)?)?;
        for bit in 0..u64::BITS {
            if mask.checked_shr(bit).is_some_and(|rest| rest & 1 == 1) {
                let at = first.checked_add(usize::try_from(bit).ok()?)?;
                pairs.push((*formats.get(at)?, modifier));
            }
        }
    }
    Some(pairs)
}

/// The plane KMS shows this CRTC's framebuffers on.
#[derive(Clone, Debug)]
pub(super) struct PrimaryPlane {
    handle: plane::Handle,
    fb_id: property::Handle,
    formats: PlaneFormats,
}

/// Find the primary plane on `crtc`, enabling atomic modesetting on `card`
/// so that buffers can be tried on it in a `TEST_ONLY` commit.
///
/// The scanout keeps presenting through legacy page flips; atomic is enabled
/// for the test commit only. It makes the kernel report aspect-ratio flags
/// in modes, which the rest of the backend does not expect, so that is
/// switched back off.
fn probe_primary_plane(card: &Card, crtc: crtc::Handle) -> Result<PrimaryPlane, String> {
    card.set_client_capability(ClientCapability::Atomic, true)
        .map_err(|error| format!("atomic modesetting is unavailable: {error}"))?;
    if let Err(error) = card.set_client_capability(ClientCapability::AspectRatio, false) {
        tracing::debug!(%error, "could not switch mode aspect-ratio flags back off");
    }
    let planes = card
        .plane_handles()
        .map_err(|error| format!("list planes: {error}"))?;
    for handle in planes {
        let Ok(info) = card.get_plane(handle) else {
            continue;
        };
        if info.crtc() != Some(crtc) {
            continue;
        }
        let Some((_, kind, _)) = find_property(card, handle, "type") else {
            continue;
        };
        if kind != PLANE_TYPE_PRIMARY {
            continue;
        }
        let (fb_id, _, _) = find_property(card, handle, "FB_ID")
            .ok_or_else(|| "the primary plane has no FB_ID property".to_owned())?;
        let formats = find_property(card, handle, "IN_FORMATS")
            .and_then(|(_, blob, _)| card.get_property_blob(blob).ok())
            .and_then(|blob| parse_in_formats(&blob))
            .map_or_else(
                || PlaneFormats::Implicit(info.formats().to_vec()),
                PlaneFormats::Modifiers,
            );
        return Ok(PrimaryPlane {
            handle,
            fb_id,
            formats,
        });
    }
    Err("no primary plane shows this CRTC".to_owned())
}

/// A KMS framebuffer made from a client's dma-buf. Removed when the last
/// holder lets go — never while KMS shows it or has it queued, since the
/// output's [`ClientHold`] is one of those holders.
pub struct ClientFramebuffer {
    card: Card,
    handle: framebuffer::Handle,
}

impl std::fmt::Debug for ClientFramebuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("ClientFramebuffer")
            .field(&self.handle)
            .finish()
    }
}

impl ClientFramebuffer {
    /// Make `dmabuf` a framebuffer on `card`. The framebuffer keeps its own
    /// references to the buffer's memory, so the GEM handles go at once.
    fn import(card: &Card, dmabuf: &Dmabuf) -> Result<Self, String> {
        let mut planes = DmabufPlanes {
            dmabuf,
            handles: [None; 4],
        };
        for (slot, fd) in planes.handles.iter_mut().zip(dmabuf.handles()) {
            *slot = Some(
                card.prime_fd_to_buffer(fd)
                    .map_err(|error| format!("import the client buffer: {error}"))?,
            );
        }
        let flags = if planes.modifier().is_some() {
            FbCmd2Flags::MODIFIERS
        } else {
            FbCmd2Flags::empty()
        };
        let added = card.add_planar_framebuffer(&planes, flags);
        let mut closed = Vec::new();
        for handle in planes.handles.into_iter().flatten() {
            if !closed.contains(&handle) {
                let _ = card.close_buffer(handle);
                closed.push(handle);
            }
        }
        let handle = added.map_err(|error| format!("make a framebuffer of it: {error}"))?;
        let card = match card.try_clone() {
            Ok(card) => card,
            Err(error) => {
                let _ = card.destroy_framebuffer(handle);
                return Err(error);
            }
        };
        Ok(Self { card, handle })
    }

    pub fn handle(&self) -> framebuffer::Handle {
        self.handle
    }
}

impl Drop for ClientFramebuffer {
    fn drop(&mut self) {
        if let Err(error) = self.card.destroy_framebuffer(self.handle) {
            tracing::debug!(fb = ?self.handle, %error, "failed to remove a client framebuffer");
        }
    }
}

/// A client dma-buf described for `ADDFB2`.
struct DmabufPlanes<'a> {
    dmabuf: &'a Dmabuf,
    handles: [Option<drm_buffer::Handle>; 4],
}

impl PlanarBuffer for DmabufPlanes<'_> {
    fn size(&self) -> (u32, u32) {
        (self.dmabuf.width(), self.dmabuf.height())
    }
    fn format(&self) -> DrmFourcc {
        self.dmabuf.format().code
    }
    fn modifier(&self) -> Option<DrmModifier> {
        let modifier = self.dmabuf.format().modifier;
        (modifier != DrmModifier::Invalid).then_some(modifier)
    }
    fn pitches(&self) -> [u32; 4] {
        let mut pitches = [0; 4];
        for (pitch, stride) in pitches.iter_mut().zip(self.dmabuf.strides()) {
            *pitch = stride;
        }
        pitches
    }
    fn handles(&self) -> [Option<drm_buffer::Handle>; 4] {
        self.handles
    }
    fn offsets(&self) -> [u32; 4] {
        let mut offsets = [0; 4];
        for (offset, at) in offsets.iter_mut().zip(self.dmabuf.offsets()) {
            *offset = at;
        }
        offsets
    }
}

/// Ask KMS, without changing anything, whether `plane` takes `framebuffer`
/// in place of the one it shows.
fn test_on_plane(
    card: &Card,
    plane: &PrimaryPlane,
    framebuffer: framebuffer::Handle,
) -> Result<(), String> {
    let mut request = AtomicModeReq::new();
    request.add_property(
        plane.handle,
        plane.fb_id,
        property::Value::Framebuffer(Some(framebuffer)),
    );
    card.atomic_commit(AtomicCommitFlags::TEST_ONLY, request)
        .map_err(|error| format!("the primary plane refused the buffer in a test commit: {error}"))
}

/// A client buffer KMS shows, or has queued: the frame it was imported for,
/// which holds the buffer back from its client, and its framebuffer.
#[derive(Clone, Debug)]
pub struct ClientScanout {
    frame: Arc<GpuFrame>,
    framebuffer: Arc<ClientFramebuffer>,
}

impl ClientScanout {
    pub fn new(frame: Arc<GpuFrame>, framebuffer: Arc<ClientFramebuffer>) -> Self {
        Self { frame, framebuffer }
    }

    pub fn framebuffer(&self) -> framebuffer::Handle {
        self.framebuffer.handle()
    }

    pub fn is(&self, frame: &Arc<GpuFrame>) -> bool {
        Arc::ptr_eq(&self.frame, frame)
    }
}

/// Which client buffer one output's KMS state holds. A buffer is held from
/// the flip that queues it until the flip that replaces it retires — the
/// client's or the compositor's — so its client never renders into a buffer
/// KMS may still read.
#[derive(Debug)]
pub struct ClientHold<T> {
    /// Shown now.
    front: Option<T>,
    /// Queued by a flip that has not retired.
    pending: Option<T>,
}

impl<T> Default for ClientHold<T> {
    fn default() -> Self {
        Self {
            front: None,
            pending: None,
        }
    }
}

impl<T> ClientHold<T> {
    /// A flip onto `buffer` was queued.
    pub fn queued(&mut self, buffer: T) {
        self.pending = Some(buffer);
    }

    /// The queued flip onto a client buffer retired: it is shown, and the
    /// buffer it replaced goes.
    pub fn client_flip_retired(&mut self) {
        if let Some(shown) = self.pending.take() {
            self.front = Some(shown);
        }
    }

    /// A flip onto one of the compositor's own targets retired: no client
    /// buffer is shown any more.
    pub fn target_flip_retired(&mut self) {
        self.front = None;
    }

    /// The buffer most recently handed to KMS: the queued one, else the
    /// shown one.
    pub fn latest(&self) -> Option<&T> {
        self.pending.as_ref().or(self.front.as_ref())
    }

    /// KMS shows a client buffer, or will once its flip retires.
    pub fn any(&self) -> bool {
        self.front.is_some() || self.pending.is_some()
    }
}

/// Client scanout as one output last did it, for the session log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ClientStatus {
    /// No candidate for this output.
    Idle,
    Active,
    Refused(String),
}

/// One remembered client buffer: its framebuffer, or why it cannot be one.
struct Remembered {
    buffer: WeakDmabuf,
    verdict: Result<Arc<ClientFramebuffer>, String>,
}

/// One output's client scanout state that outlives its targets: the primary
/// plane, the buffers already tried on it, and what the log last said.
#[derive(Default)]
pub(super) struct ClientPlane {
    plane: Option<Result<PrimaryPlane, String>>,
    buffers: Vec<Remembered>,
    status: Option<ClientStatus>,
}

impl ClientPlane {
    /// The framebuffer `dmabuf` scans out through, made and tried in a
    /// `TEST_ONLY` commit the first time the buffer comes by; a refusal is
    /// remembered as well, so a buffer the plane refused is not asked again.
    pub fn framebuffer(
        &mut self,
        card: &Card,
        crtc: crtc::Handle,
        dmabuf: &Dmabuf,
    ) -> Result<Arc<ClientFramebuffer>, String> {
        self.buffers.retain(|entry| !entry.buffer.is_gone());
        let weak = dmabuf.weak();
        if let Some(entry) = self.buffers.iter().find(|entry| entry.buffer == weak) {
            return entry.verdict.clone();
        }
        let verdict = self.import(card, crtc, dmabuf);
        if self.buffers.len() >= REMEMBERED_BUFFERS {
            self.buffers.remove(0);
        }
        self.buffers.push(Remembered {
            buffer: weak,
            verdict: verdict.clone(),
        });
        verdict
    }

    fn import(
        &mut self,
        card: &Card,
        crtc: crtc::Handle,
        dmabuf: &Dmabuf,
    ) -> Result<Arc<ClientFramebuffer>, String> {
        let plane = self
            .plane
            .get_or_insert_with(|| probe_primary_plane(card, crtc))
            .clone()?;
        let format = dmabuf.format();
        if !plane
            .formats
            .supports(format.code as u32, u64::from(format.modifier))
        {
            return Err(format!(
                "the primary plane does not scan out {:?} with modifier {:#x}",
                format.code,
                u64::from(format.modifier)
            ));
        }
        let framebuffer = ClientFramebuffer::import(card, dmabuf)?;
        test_on_plane(card, &plane, framebuffer.handle())?;
        Ok(Arc::new(framebuffer))
    }

    /// KMS refused `dmabuf` after all: composite it from now on.
    pub fn refuse(&mut self, dmabuf: &Dmabuf, reason: &str) {
        let weak = dmabuf.weak();
        if let Some(entry) = self.buffers.iter_mut().find(|entry| entry.buffer == weak) {
            entry.verdict = Err(reason.to_owned());
        }
    }

    /// Forget every verdict: the CRTC was set up again, and what its plane
    /// takes may have changed with it.
    pub fn forget_buffers(&mut self) {
        self.buffers.clear();
    }

    /// Log `status` for `output` when it differs from the last one logged.
    /// Silence while an output never had a candidate.
    pub fn note(&mut self, output: OutputId, status: ClientStatus) {
        let previous = self.status.replace(status.clone());
        if previous.as_ref() == Some(&status)
            || (previous.is_none() && status == ClientStatus::Idle)
        {
            return;
        }
        match status {
            ClientStatus::Active => eprintln!(
                "client_scanout_status=active output={} DRM fullscreen client scanned out",
                output.0
            ),
            ClientStatus::Idle => eprintln!(
                "client_scanout_status=inactive reason=\"no fullscreen client on top\" output={} DRM fullscreen client composited",
                output.0
            ),
            ClientStatus::Refused(reason) => eprintln!(
                "client_scanout_status=inactive reason={reason:?} output={} DRM fullscreen client composited",
                output.0
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eligible() -> ClientEligibility {
        ClientEligibility {
            enabled: true,
            upright: true,
            covers_output: true,
            client_buffer: true,
            opaque: true,
            sdr: true,
            no_hdr_layers: true,
            same_format: true,
        }
    }

    #[test]
    fn a_candidate_is_scanned_out_only_when_nothing_would_differ() {
        assert_eq!(client_scanout_rejection(eligible()), None);
        let refusals = [
            ClientEligibility {
                enabled: false,
                ..eligible()
            },
            ClientEligibility {
                upright: false,
                ..eligible()
            },
            ClientEligibility {
                covers_output: false,
                ..eligible()
            },
            ClientEligibility {
                client_buffer: false,
                ..eligible()
            },
            ClientEligibility {
                opaque: false,
                ..eligible()
            },
            ClientEligibility {
                sdr: false,
                ..eligible()
            },
            ClientEligibility {
                no_hdr_layers: false,
                ..eligible()
            },
            ClientEligibility {
                same_format: false,
                ..eligible()
            },
        ];
        for refusal in refusals {
            assert!(
                client_scanout_rejection(refusal).is_some(),
                "{refusal:?} must composite"
            );
        }
    }

    /// An `IN_FORMATS` blob: `formats`, then one `drm_format_modifier` per
    /// `(mask, offset, modifier)`.
    fn blob(formats: &[u32], modifiers: &[(u64, u32, u64)]) -> Vec<u8> {
        let count = |len: usize| u32::try_from(len).expect("a small fixture");
        let header = 24_u32;
        let formats_offset = header;
        let modifiers_offset = header.saturating_add(count(formats.len()).saturating_mul(4));
        let mut out = Vec::new();
        for word in [
            1,
            0,
            count(formats.len()),
            formats_offset,
            count(modifiers.len()),
            modifiers_offset,
        ] {
            out.extend_from_slice(&word.to_ne_bytes());
        }
        for format in formats {
            out.extend_from_slice(&format.to_ne_bytes());
        }
        for &(mask, offset, modifier) in modifiers {
            out.extend_from_slice(&mask.to_ne_bytes());
            out.extend_from_slice(&offset.to_ne_bytes());
            out.extend_from_slice(&0_u32.to_ne_bytes());
            out.extend_from_slice(&modifier.to_ne_bytes());
        }
        out
    }

    const XRGB: u32 = DrmFourcc::Xrgb8888 as u32;
    const ARGB: u32 = DrmFourcc::Argb8888 as u32;
    const NV12: u32 = DrmFourcc::Nv12 as u32;
    const TILED: u64 = 0x0100_0000_0000_0002;

    #[test]
    fn in_formats_lists_each_format_with_its_modifiers() {
        let parsed = parse_in_formats(&blob(
            &[XRGB, ARGB, NV12],
            &[
                (0b011, 0, MODIFIER_LINEAR),
                (0b001, 0, TILED),
                (0b1, 2, TILED),
            ],
        ))
        .expect("well formed");
        assert_eq!(
            parsed,
            vec![
                (XRGB, MODIFIER_LINEAR),
                (ARGB, MODIFIER_LINEAR),
                (XRGB, TILED),
                (NV12, TILED),
            ]
        );
        let formats = PlaneFormats::Modifiers(parsed);
        assert!(formats.supports(XRGB, TILED));
        assert!(formats.supports(XRGB, MODIFIER_LINEAR));
        assert!(!formats.supports(ARGB, TILED), "ARGB is linear only");
        assert!(
            formats.supports(XRGB, u64::from(DrmModifier::Invalid)),
            "an implicit layout is left to the test commit"
        );
        assert!(!formats.supports(DrmFourcc::Xbgr8888 as u32, MODIFIER_LINEAR));
    }

    #[test]
    fn a_malformed_in_formats_blob_is_refused() {
        let good = blob(&[XRGB], &[(0b1, 0, MODIFIER_LINEAR)]);
        assert!(parse_in_formats(&good).is_some());
        assert_eq!(
            parse_in_formats(&good[..good.len().saturating_sub(1)]),
            None,
            "truncated"
        );
        assert_eq!(parse_in_formats(&[]), None, "empty");
        assert_eq!(
            parse_in_formats(&blob(&[XRGB], &[(0b10, 0, MODIFIER_LINEAR)])),
            None,
            "a mask bit past the format list"
        );
    }

    #[test]
    fn a_driver_without_modifiers_takes_its_formats_linear_or_implicit() {
        let formats = PlaneFormats::Implicit(vec![XRGB]);
        assert!(formats.supports(XRGB, MODIFIER_LINEAR));
        assert!(formats.supports(XRGB, u64::from(DrmModifier::Invalid)));
        assert!(!formats.supports(XRGB, TILED));
        assert!(!formats.supports(ARGB, MODIFIER_LINEAR));
    }

    /// A buffer that records when its holder let go.
    struct Buffer(&'static str, Arc<std::sync::Mutex<Vec<&'static str>>>);

    impl Drop for Buffer {
        fn drop(&mut self) {
            self.1.lock().expect("released").push(self.0);
        }
    }

    #[test]
    fn a_client_buffer_is_held_until_the_flip_that_replaces_it_retires() {
        let released = Arc::new(std::sync::Mutex::new(Vec::new()));
        let buffer = |name| Buffer(name, released.clone());
        let released = || released.lock().expect("released").clone();
        let mut hold = ClientHold::default();
        assert!(!hold.any());

        hold.queued(buffer("a"));
        assert_eq!(hold.latest().map(|b| b.0), Some("a"));
        hold.client_flip_retired();
        assert!(hold.any(), "a is shown");

        // The client's next buffer is queued: a is still shown.
        hold.queued(buffer("b"));
        assert_eq!(hold.latest().map(|b| b.0), Some("b"));
        assert!(
            released().is_empty(),
            "a is on screen until b's flip retires"
        );
        hold.client_flip_retired();
        assert_eq!(released(), ["a"]);

        // The compositor renders again (an overlay appeared): b is held until
        // the flip onto the compositor's own target retires.
        assert_eq!(released(), ["a"], "queuing the target's flip keeps b");
        hold.target_flip_retired();
        assert_eq!(released(), ["a", "b"]);
        assert!(!hold.any());
        assert!(hold.latest().is_none());
    }
}
