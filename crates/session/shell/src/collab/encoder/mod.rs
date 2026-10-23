//! Video encoder for shared surfaces, over the shared media stack.
//!
//! One backend: [`media::RealtimeEncoder`], which chooses the `GStreamer`
//! element from `media`'s real-time tables — VA-API / NVCODEC / V4L2
//! hardware first, then royalty-free software (libvpx VP9, libaom / rav1e
//! AV1), then Cisco's `openh264` binary loaded by the system for H.264 — and
//! proves the choice with a probe frame. [`EncoderKind`] reports which kind
//! of element won; [`Codec`] is the bitstream on the wire.
//!
//! The caller derives the codec from negotiation with its viewers
//! ([`media::realtime::negotiate`]) and the HDR config; a codec no local
//! element can encode reports [`EncoderError::Unavailable`].

pub mod nv12;

use crate::collab::colorimetry::Colorimetry;
use crate::collab::hdr_metadata::{Hdr10Metadata, HdrConfig};
use crate::config::EncoderChoice;
use media::realtime::{
    CodecElement, CodecFamily, CodecTier, EncoderConfig, HdrStaticMetadata, InputFormat,
    InputFrame, RealtimeEncoder, RealtimePolicy, StreamColor,
};
use std::time::{Duration, Instant};

/// The bitstream on the wire. Its [`Codec::wire_label`] is what
/// `SurfaceStreamStart.codec` carries.
pub use media::StreamCodec as Codec;

/// Which kind of element is doing the encode.
///
/// Distinct from [`EncoderChoice`] — that's the config-time *request*,
/// this is the runtime *outcome* of the media stack's selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderKind {
    /// VA-API hardware (Intel, AMD, some ARM `SoCs`).
    VaApi,
    /// NVIDIA NVENC hardware.
    NvEnc,
    /// Linux V4L2 hardware encoder (embedded `SoCs`).
    V4l2,
    /// Royalty-free software (libvpx, libaom, rav1e).
    RoyaltyFreeSoftware,
    /// Cisco's `openh264` binary, loaded by the system's `GStreamer` plugin.
    CiscoOpenH264,
}

impl EncoderKind {
    /// Human-readable label; embedded in logs and OSD overlays.
    pub fn as_str(self) -> &'static str {
        match self {
            EncoderKind::VaApi => "vaapi",
            EncoderKind::NvEnc => "nvenc",
            EncoderKind::V4l2 => "v4l2",
            EncoderKind::RoyaltyFreeSoftware => "software",
            EncoderKind::CiscoOpenH264 => "openh264",
        }
    }

    /// Whether the encode runs on fixed-function video hardware.
    pub fn is_hardware(self) -> bool {
        matches!(
            self,
            EncoderKind::VaApi | EncoderKind::NvEnc | EncoderKind::V4l2
        )
    }

    /// The kind an element of the media tables belongs to.
    pub fn of(element: &CodecElement) -> Self {
        match (element.tier, element.family) {
            (CodecTier::Hardware, CodecFamily::NvCodec) => EncoderKind::NvEnc,
            (CodecTier::Hardware, CodecFamily::V4l2) => EncoderKind::V4l2,
            (CodecTier::Hardware, _) => EncoderKind::VaApi,
            (CodecTier::CiscoOpenH264, _) => EncoderKind::CiscoOpenH264,
            (CodecTier::RoyaltyFree | CodecTier::SystemProvided, _) => {
                EncoderKind::RoyaltyFreeSoftware
            }
        }
    }
}

/// One frame's encoder stats.
#[derive(Debug, Clone, Copy, Default)]
pub struct FrameStats {
    pub encode_wallclock: Duration,
    pub keyframe: bool,
    /// Bytes emitted for this frame.
    pub bytes: usize,
}

/// Errors returned by construction / encode.
#[derive(Debug)]
pub enum EncoderError {
    /// No element can encode the requested codec here.
    Unavailable { kind: EncoderKind, reason: String },
    /// The encoder rejected the configuration (bad dimensions).
    ConfigFailed { kind: EncoderKind, detail: String },
    /// An encode step failed at runtime, or the input was refused (a dmabuf
    /// layout the element cannot import: the caller reads the frame back).
    EncodeFailed { kind: EncoderKind, detail: String },
}

impl std::fmt::Display for EncoderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EncoderError::Unavailable { kind, reason } => {
                write!(f, "{} encoder unavailable: {reason}", kind.as_str())
            }
            EncoderError::ConfigFailed { kind, detail } => {
                write!(f, "{} encoder config failed: {detail}", kind.as_str())
            }
            EncoderError::EncodeFailed { kind, detail } => {
                write!(f, "{} encoder runtime failure: {detail}", kind.as_str())
            }
        }
    }
}

impl std::error::Error for EncoderError {}

/// Which input the caller hands over on this call.
pub enum FrameInput<'a> {
    /// Row-major RGBA, `width * height * 4` bytes, no padding.
    Rgba(&'a [u8]),
    /// As [`Self::Rgba`], differing from the frame encoded before it only
    /// within `changed`.
    RgbaChanged {
        rgba: &'a [u8],
        changed: &'a [nv12::Region],
    },
    /// DMABUF fds + per-plane offsets/strides. Imported without a copy when
    /// the element can read the layout (VA-API through `vapostproc`);
    /// otherwise refused with [`EncoderError::EncodeFailed`] so the caller
    /// falls back to a readback.
    ///
    /// `plane_fds[i]` are borrowed for the encode call; the encoder dups what
    /// it keeps. `num_planes` is the count of valid entries.
    DmabufFd {
        plane_fds: [std::os::fd::RawFd; 4],
        plane_offsets: [u64; 4],
        plane_strides: [u32; 4],
        num_planes: u8,
        drm_fourcc: u32,
        drm_modifier: u64,
        width: u32,
        height: u32,
    },
}

/// Encoder handle for one shared surface. The caller must not queue more
/// than one frame ahead: encode synchronously and ship the bytes.
pub trait Encoder: Send {
    /// Which kind of element is running.
    fn kind(&self) -> EncoderKind;
    /// Bitstream codec being emitted.
    fn codec(&self) -> Codec;
    /// Configured width, in pixels.
    fn width(&self) -> u32;
    /// Configured height, in pixels.
    fn height(&self) -> u32;
    /// Whether [`FrameInput::DmabufFd`] of this layout can be imported.
    fn accepts_dmabuf(&self, fourcc: u32, modifier: u64) -> bool;
    /// Make the next frame a keyframe (a viewer joined).
    fn force_keyframe(&mut self);
    /// Encode one frame, appending its access unit to `out`.
    ///
    /// # Errors
    ///
    /// Returns [`EncoderError::EncodeFailed`] if the input is refused or the
    /// element fails.
    fn encode(
        &mut self,
        input: FrameInput<'_>,
        out: &mut Vec<u8>,
    ) -> Result<FrameStats, EncoderError>;
}

/// Static config passed to [`build`].
#[derive(Debug, Clone, Copy)]
pub struct EncoderInit {
    pub width: u32,
    pub height: u32,
    /// Target frame rate for rate control.
    pub fps: u32,
    /// Target bitrate in kilobits per second. `0` = element default.
    pub bitrate_kbps: u32,
    /// Keyframe interval in seconds. `0` = first frame and requested ones.
    pub keyframe_interval_secs: u32,
    /// Bitstream codec to emit.
    pub codec: Codec,
    /// Colorimetry tagged into the bitstream.
    pub colorimetry: Colorimetry,
    /// HDR10 static metadata written with keyframes. `None` = SDR.
    pub hdr: Option<Hdr10Metadata>,
    /// Offer the zero-copy dmabuf path (compositor buffers are `BGRx`).
    pub dmabuf_input: bool,
    /// Encode at this size rather than `width`×`height`: frames are scaled
    /// down first, for receivers whose hardware decoders stop short of the
    /// window's size.
    pub output: Option<(u32, u32)>,
}

impl EncoderInit {
    pub const fn sdr(
        codec: Codec,
        width: u32,
        height: u32,
        fps: u32,
        bitrate_kbps: u32,
        keyframe_interval_secs: u32,
    ) -> Self {
        Self {
            width,
            height,
            fps,
            bitrate_kbps,
            keyframe_interval_secs,
            codec,
            colorimetry: Colorimetry::sdr_bt709(),
            hdr: None,
            dmabuf_input: false,
            output: None,
        }
    }

    /// The SDR H.264 configuration.
    pub const fn sdr_h264(
        width: u32,
        height: u32,
        fps: u32,
        bitrate_kbps: u32,
        keyframe_interval_secs: u32,
    ) -> Self {
        Self::sdr(
            Codec::H264,
            width,
            height,
            fps,
            bitrate_kbps,
            keyframe_interval_secs,
        )
    }

    /// An HDR10 configuration: 10-bit `codec` (HEVC Main10 or AV1) with the
    /// config's transfer and metadata.
    pub fn hdr10(
        codec: Codec,
        width: u32,
        height: u32,
        fps: u32,
        bitrate_kbps: u32,
        keyframe_interval_secs: u32,
        hdr: HdrConfig,
    ) -> Self {
        Self {
            width,
            height,
            fps,
            bitrate_kbps,
            keyframe_interval_secs,
            codec,
            colorimetry: hdr.colorimetry(),
            hdr: Some(hdr.metadata),
            dmabuf_input: false,
            output: None,
        }
    }

    /// Bits per sample of the encoded stream.
    pub fn bit_depth(&self) -> u8 {
        if self.hdr.is_some() || self.colorimetry.is_hdr() {
            10
        } else {
            self.codec.bit_depth()
        }
    }
}

impl EncoderChoice {
    /// The media stack's policy for this configured preference.
    pub fn policy(self) -> RealtimePolicy {
        match self {
            EncoderChoice::Auto => RealtimePolicy::encoder_from_env(),
            EncoderChoice::Software => RealtimePolicy::SoftwareOnly,
            EncoderChoice::Vaapi => RealtimePolicy::Prefer(CodecFamily::Va),
            EncoderChoice::Nvenc => RealtimePolicy::Prefer(CodecFamily::NvCodec),
            EncoderChoice::V4l2 => RealtimePolicy::Prefer(CodecFamily::V4l2),
        }
    }
}

/// Outcome of looking for an encoder for one codec, for logs.
#[derive(Debug, Clone)]
pub struct ProbeOutcome {
    pub kind: EncoderKind,
    pub codec: Codec,
    pub available: bool,
    pub reason: String,
}

fn colour(c: Colorimetry) -> StreamColor {
    StreamColor {
        primaries: c.primaries.cicp(),
        transfer: c.transfer.cicp(),
        matrix: c.matrix.cicp(),
        full_range: c.range == crate::collab::colorimetry::ColorRange::Full,
    }
}

fn hdr_metadata(m: Hdr10Metadata) -> HdrStaticMetadata {
    // The wire orders primaries R, G, B; the media stack takes G, B, R.
    let [r, g, b] = m.mastering.display_primaries;
    HdrStaticMetadata {
        display_primaries: [g, b, r],
        white_point: m.mastering.white_point,
        max_luminance: m.mastering.max_display_mastering_luminance,
        min_luminance: m.mastering.min_display_mastering_luminance,
        max_cll: m.light.max_content_light_level,
        max_fall: m.light.max_frame_average_light_level,
    }
}

/// Whether frames for `init` are converted to NV12 here: an SDR stream in
/// limited-range BT.709, the colour [`nv12::Nv12Frame`] converts into.
fn converts_here(init: &EncoderInit) -> bool {
    let colour = colour(init.colorimetry);
    !init.dmabuf_input && init.hdr.is_none() && colour.matrix == 1 && !colour.full_range
}

/// The media stack's configuration for `init` under `choice`.
fn media_config(init: &EncoderInit, choice: EncoderChoice) -> EncoderConfig {
    // 4:2:0 encoders need even dimensions; an odd surface is encoded without
    // its last row/column.
    let mut config = EncoderConfig::new(
        init.codec,
        init.width & !1,
        init.height & !1,
        init.fps.max(1),
    );
    config.bitrate_bps = u64::from(init.bitrate_kbps).saturating_mul(1000);
    config.keyframe_interval = init.keyframe_interval_secs.saturating_mul(init.fps.max(1));
    // Captures are 8-bit RGB. An SDR BT.709 stream is handed NV12 converted
    // here, where only what changed is converted; any other stream colour
    // is converted by the pipeline.
    config.input = if init.dmabuf_input {
        InputFormat::Bgrx
    } else if converts_here(init) {
        InputFormat::Nv12
    } else {
        InputFormat::Rgba
    };
    config.dmabuf_input = init.dmabuf_input;
    config.output_size = init.output.map(|(width, height)| (width & !1, height & !1));
    config.color = colour(init.colorimetry);
    config.hdr = init.hdr.map(hdr_metadata);
    config.policy = choice.policy();
    config
}

/// Build an encoder for `init`, letting the media stack pick the element
/// under `choice`.
///
/// # Errors
///
/// Returns [`EncoderError::Unavailable`] if no element can encode
/// `init.codec` here, or [`EncoderError::ConfigFailed`] for dimensions no
/// element takes.
pub fn build(init: EncoderInit, choice: EncoderChoice) -> Result<Box<dyn Encoder>, EncoderError> {
    GstEncoder::new(init, choice).map(|e| Box::new(e) as Box<dyn Encoder>)
}

/// Whether some element can encode `codec` under `choice`, and which kind,
/// by building one at a small size. Used for logs and tests; real sessions
/// just call [`build`].
pub fn probe(codec: Codec, choice: EncoderChoice) -> ProbeOutcome {
    match GstEncoder::new(EncoderInit::sdr(codec, 64, 64, 30, 1000, 0), choice) {
        Ok(e) => ProbeOutcome {
            kind: e.kind(),
            codec,
            available: true,
            reason: e.inner.element().describe(),
        },
        Err(e) => ProbeOutcome {
            kind: EncoderKind::RoyaltyFreeSoftware,
            codec,
            available: false,
            reason: e.to_string(),
        },
    }
}

/// Copy the top-left `size` of a `src_width`-wide RGBA frame into `out`,
/// swapping red and blue when `bgrx`.
fn repack(rgba: &[u8], src_width: u32, size: (u32, u32), bgrx: bool, out: &mut Vec<u8>) {
    let src_row = (src_width as usize).saturating_mul(4);
    let dst_row = (size.0 as usize).saturating_mul(4);
    out.clear();
    for row in rgba.chunks_exact(src_row.max(1)).take(size.1 as usize) {
        let visible = row.get(..dst_row).unwrap_or(row);
        if bgrx {
            for px in visible.chunks_exact(4) {
                if let &[r, g, b, a] = px {
                    out.extend_from_slice(&[b, g, r, a]);
                }
            }
        } else {
            out.extend_from_slice(visible);
        }
    }
}

/// The one encoder backend.
pub struct GstEncoder {
    inner: RealtimeEncoder,
    kind: EncoderKind,
    init: EncoderInit,
    /// RGBA frames swizzled for a `BGRx`-configured (dmabuf-capable) encoder.
    swizzle: Vec<u8>,
    /// The window as the NV12 frame the encoder takes, where it takes one.
    nv12: Option<nv12::Nv12Frame>,
    origin: Instant,
}

impl GstEncoder {
    /// # Errors
    ///
    /// As [`build`].
    pub fn new(init: EncoderInit, choice: EncoderChoice) -> Result<Self, EncoderError> {
        let config = media_config(&init, choice);
        let inner = RealtimeEncoder::new(&config).map_err(|e| match e {
            media::RealtimeError::InvalidConfig(detail) => EncoderError::ConfigFailed {
                kind: EncoderKind::RoyaltyFreeSoftware,
                detail,
            },
            other => EncoderError::Unavailable {
                kind: EncoderKind::RoyaltyFreeSoftware,
                reason: other.to_string(),
            },
        })?;
        let kind = EncoderKind::of(inner.element());
        tracing::info!(
            codec = init.codec.wire_label(),
            element = %inner.element().describe(),
            dmabuf = inner.accepts_dmabuf(),
            "shared-surface encoder selected"
        );
        let config = inner.config();
        let nv12 = (config.input == InputFormat::Nv12)
            .then(|| nv12::Nv12Frame::new(config.width, config.height, init.width));
        Ok(Self {
            inner,
            kind,
            init,
            swizzle: Vec::new(),
            nv12,
            origin: Instant::now(),
        })
    }

    /// The element's description, for logs.
    pub fn element(&self) -> &'static CodecElement {
        self.inner.element()
    }
}

impl Encoder for GstEncoder {
    fn kind(&self) -> EncoderKind {
        self.kind
    }
    fn codec(&self) -> Codec {
        self.init.codec
    }
    fn width(&self) -> u32 {
        self.init.width
    }
    fn height(&self) -> u32 {
        self.init.height
    }
    fn accepts_dmabuf(&self, fourcc: u32, modifier: u64) -> bool {
        self.inner.accepts_dmabuf_format(fourcc, modifier)
    }
    fn force_keyframe(&mut self) {
        self.inner.force_keyframe();
    }
    fn encode(
        &mut self,
        input: FrameInput<'_>,
        out: &mut Vec<u8>,
    ) -> Result<FrameStats, EncoderError> {
        let kind = self.kind;
        let failed = |detail: String| EncoderError::EncodeFailed { kind, detail };
        let start = Instant::now();
        // Timestamps advance with the wall clock since construction, which
        // is what rate control needs; frames can arrive at any cadence.
        let pts = self.origin.elapsed();
        let units = match input {
            FrameInput::Rgba(rgba) | FrameInput::RgbaChanged { rgba, .. } => {
                let changed = match input {
                    FrameInput::RgbaChanged { changed, .. } => Some(changed),
                    _ => None,
                };
                let expected = (self.init.width as usize)
                    .saturating_mul(self.init.height as usize)
                    .saturating_mul(4);
                if rgba.len() != expected {
                    return Err(failed(format!(
                        "{} bytes for a {}x{} RGBA frame; expected {expected}",
                        rgba.len(),
                        self.init.width,
                        self.init.height
                    )));
                }
                let config = self.inner.config();
                let bgrx = config.input == InputFormat::Bgrx;
                let cropped = (config.width, config.height) != (self.init.width, self.init.height);
                let frame = if let Some(nv12) = self.nv12.as_mut() {
                    nv12.update(rgba, changed);
                    nv12.bytes()
                } else if bgrx || cropped {
                    // 4:2:0 encoders take even dimensions, so an odd-sized
                    // surface loses its last row/column; the dmabuf-capable
                    // pipeline takes BGRx, so a readback frame is swizzled.
                    repack(
                        rgba,
                        self.init.width,
                        (config.width, config.height),
                        bgrx,
                        &mut self.swizzle,
                    );
                    &self.swizzle
                } else {
                    rgba
                };
                self.inner
                    .encode(InputFrame::Packed(frame), pts)
                    .map_err(|e| failed(e.to_string()))?
            }
            FrameInput::DmabufFd {
                plane_fds,
                plane_offsets,
                plane_strides,
                num_planes,
                drm_fourcc,
                drm_modifier,
                width,
                height,
            } => {
                if !self.inner.accepts_dmabuf_format(drm_fourcc, drm_modifier) {
                    return Err(failed(format!(
                        "dmabuf layout {}:{drm_modifier:#x} is not importable",
                        media::dmabuf::fourcc_str(drm_fourcc)
                    )));
                }
                let mut planes = Vec::with_capacity(usize::from(num_planes));
                for i in 0..usize::from(num_planes.min(4)) {
                    let (Some(&fd), Some(&offset), Some(&stride)) =
                        (plane_fds.get(i), plane_offsets.get(i), plane_strides.get(i))
                    else {
                        break;
                    };
                    if fd < 0 {
                        return Err(failed(format!("plane {i} has no fd")));
                    }
                    // SAFETY: the caller keeps every plane fd open for the
                    // duration of this call (see `FrameInput::DmabufFd`).
                    let fd = unsafe { std::os::fd::BorrowedFd::borrow_raw(fd) };
                    planes.push(media::realtime::DmabufInputPlane {
                        fd,
                        offset: u32::try_from(offset).map_err(|e| failed(e.to_string()))?,
                        stride,
                    });
                }
                self.inner
                    .encode(
                        InputFrame::Dmabuf(media::realtime::DmabufInput {
                            planes: &planes,
                            fourcc: drm_fourcc,
                            modifier: drm_modifier,
                            width,
                            height,
                        }),
                        pts,
                    )
                    .map_err(|e| failed(e.to_string()))?
            }
        };
        let mut stats = FrameStats {
            encode_wallclock: start.elapsed(),
            ..FrameStats::default()
        };
        for unit in units {
            stats.keyframe |= unit.keyframe;
            stats.bytes = stats.bytes.saturating_add(unit.data.len());
            out.extend_from_slice(&unit.data);
        }
        Ok(stats)
    }
}

#[cfg(test)]
mod cost_tests;

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    reason = "test fixtures are tiny, bounded gradients"
)]
mod tests {
    use super::*;

    fn gradient(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::new();
        for y in 0..h {
            for x in 0..w {
                out.extend_from_slice(&[(x * 200 / w) as u8, (y * 200 / h) as u8, 128, 255]);
            }
        }
        out
    }

    #[test]
    fn software_encoder_encodes_royalty_free_codecs_and_refuses_bad_input() {
        // Phase `software_choice_encodes_royalty_free_codecs_without_a_gpu`.
        {
            for codec in [Codec::Vp9, Codec::Av1] {
                let init = EncoderInit::sdr(codec, 64, 64, 30, 1000, 0);
                let mut enc = build(init, EncoderChoice::Software).expect("software encoder");
                assert_eq!(enc.kind(), EncoderKind::RoyaltyFreeSoftware);
                assert_eq!(enc.codec(), codec);
                let mut out = Vec::new();
                let stats = enc
                    .encode(FrameInput::Rgba(&gradient(64, 64)), &mut out)
                    .expect("encode");
                assert!(stats.keyframe, "first frame is a keyframe");
                assert_eq!(stats.bytes, out.len());
                assert!(!out.is_empty());
            }
        }
        // Phase `hevc_has_no_software_encoder`.
        {
            let outcome = probe(Codec::HevcMain10, EncoderChoice::Software);
            assert!(
                !outcome.available,
                "HEVC must never be encoded in software: {}",
                outcome.reason
            );
        }
        // Phase `wrong_sized_input_is_refused`.
        {
            let init = EncoderInit::sdr(Codec::Vp9, 32, 32, 30, 1000, 0);
            let mut enc = build(init, EncoderChoice::Software).expect("encoder");
            let mut out = Vec::new();
            assert!(matches!(
                enc.encode(FrameInput::Rgba(&[0; 12]), &mut out),
                Err(EncoderError::EncodeFailed { .. })
            ));
        }
    }

    #[test]
    fn encoder_kinds_colorimetry_and_legacy_config_keep_their_meaning() {
        // Phase `element_families_map_to_kinds`.
        {
            for e in media::hw::realtime_encoders(Codec::H264) {
                let kind = EncoderKind::of(e);
                assert_eq!(kind.is_hardware(), e.is_hardware(), "{}", e.element);
            }
            let cisco = media::hw::realtime_encoders(Codec::H264)
                .iter()
                .find(|e| e.element == "openh264enc")
                .expect("listed");
            assert_eq!(EncoderKind::of(cisco), EncoderKind::CiscoOpenH264);
        }
        // Phase `init_constructors_pair_codec_with_colorimetry`.
        {
            let sdr = EncoderInit::sdr_h264(1280, 720, 30, 6000, 2);
            assert_eq!(sdr.codec, Codec::H264);
            assert_eq!(sdr.bit_depth(), 8);
            let hdr = EncoderInit::hdr10(
                Codec::Av1,
                1920,
                1080,
                30,
                15_000,
                2,
                HdrConfig::pq_reference(),
            );
            assert!(hdr.colorimetry.is_hdr());
            assert_eq!(hdr.bit_depth(), 10);
            let config = media_config(&hdr, EncoderChoice::Auto);
            assert!(config.color.is_hdr());
            assert!(config.hdr.is_some());
        }
        // Phase `legacy_config_values_keep_their_meaning`.
        {
            assert_eq!(
                EncoderChoice::Software.policy(),
                RealtimePolicy::SoftwareOnly
            );
            assert_eq!(
                EncoderChoice::Vaapi.policy(),
                RealtimePolicy::Prefer(CodecFamily::Va)
            );
            assert_eq!(
                EncoderChoice::Nvenc.policy(),
                RealtimePolicy::Prefer(CodecFamily::NvCodec)
            );
            assert_eq!(
                EncoderChoice::V4l2.policy(),
                RealtimePolicy::Prefer(CodecFamily::V4l2)
            );
            let parsed: EncoderChoice = serde_json::from_str("\"v4l2\"").unwrap();
            assert_eq!(parsed, EncoderChoice::V4l2);
        }
    }
}
