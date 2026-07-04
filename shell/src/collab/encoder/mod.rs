//! Video encoder trait + runtime selection.
//!
//! The encoder abstraction lives here (instead of `network::encoder`
//! which still hosts the openh264 wrapper for backwards compatibility)
//! because the flatpak-share pipeline is the only caller today, and
//! keeping the trait next to the session code makes the ownership graph
//! obvious.
//!
//! Three concrete backends × three codecs:
//!
//! - [`OpenH264Backend`] — pure-software Cisco openh264, always
//!   available. H.264 8-bit only. Preferred fallback.
//! - [`VaApiBackend`] — VA-API (Intel iGPU, AMD, some ARM SoCs). Gated
//!   behind the `vaapi` cargo feature. Supports H.264 8-bit + HEVC
//!   Main10 (10-bit) depending on the render node's capabilities.
//! - [`NvEncBackend`] — NVENC via `ffmpeg-next`. Behind the `nvenc`
//!   cargo feature. Supports H.264 8-bit + HEVC Main10.
//!
//! The trait keeps the encoder loop backend + codec agnostic; the
//! caller derives a [`Codec`] from the session config (SDR → H.264,
//! HDR → HEVC Main10) and every backend either supports it or returns
//! [`EncoderError::Unavailable`].
//!
//! Backends that aren't linked in this build simply don't register a
//! probe entry.

use crate::collab::colorimetry::Colorimetry;
use crate::collab::hdr_metadata::{Hdr10Metadata, HdrConfig};
use crate::config::EncoderChoice;
use std::time::Duration;

pub mod openh264;

#[cfg(feature = "vaapi")]
pub mod vaapi;

#[cfg(feature = "nvenc")]
pub mod nvenc;

/// Which concrete encoder implementation is actually running.
///
/// Distinct from [`EncoderChoice`] — that's the config-time *request*,
/// this is the runtime *outcome*. The two diverge whenever
/// `EncoderChoice::Auto` (default) resolves to a specific backend, and
/// whenever a requested hardware backend is unavailable and the caller
/// falls back to software.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncoderKind {
    /// Cisco openh264 software backend.
    Openh264,
    /// VA-API hardware backend on the local render node.
    VaApi,
    /// NVIDIA NVENC hardware backend.
    NvEnc,
}

impl EncoderKind {
    /// Human-readable label; embedded in `tracing::info!` logs and OSD
    /// overlays.
    pub fn as_str(self) -> &'static str {
        match self {
            EncoderKind::Openh264 => "openh264",
            EncoderKind::VaApi => "vaapi",
            EncoderKind::NvEnc => "nvenc",
        }
    }
}

/// Which bitstream codec is riding on the encoder backend.
///
/// Distinct from [`EncoderKind`] because the same backend can drive
/// multiple codecs — a modern NVIDIA GPU handles both `h264_nvenc` and
/// `hevc_nvenc`; a modern Intel iGPU does `h264_vaapi` + `hevc_vaapi`.
///
/// SDR pipelines use [`Self::H264`] because that's still the best-
/// supported wire format for 8-bit content — HEVC is more efficient
/// but adds decode-side compatibility overhead. HDR pipelines use
/// [`Self::HevcMain10`] because H.264 doesn't do >8-bit and AV1
/// hardware is Ada+/Arc+/Battlemage-only.
///
/// Wire label — matches the string that goes on the wire in
/// [`crate::network::NeighbourMsg::SurfaceStreamStart::codec`] so the
/// receiver can construct the right decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    /// H.264 8-bit — `codec: "h264"` on the wire.
    H264,
    /// HEVC Main10 10-bit — `codec: "hevc-main10"` on the wire.
    HevcMain10,
}

impl Codec {
    /// Wire label matching [`crate::network::protocol::SurfaceStreamStart::codec`].
    pub fn wire_label(self) -> &'static str {
        match self {
            Codec::H264 => "h264",
            Codec::HevcMain10 => "hevc-main10",
        }
    }

    /// libavcodec encoder-name suffix for this codec + backend.
    /// e.g. `Codec::HevcMain10` on `EncoderKind::VaApi` picks
    /// `hevc_vaapi`. Openh264 only supports H.264.
    pub fn libav_encoder_name(self, backend: EncoderKind) -> Option<&'static str> {
        match (self, backend) {
            (Codec::H264, EncoderKind::VaApi) => Some("h264_vaapi"),
            (Codec::HevcMain10, EncoderKind::VaApi) => Some("hevc_vaapi"),
            (Codec::H264, EncoderKind::NvEnc) => Some("h264_nvenc"),
            (Codec::HevcMain10, EncoderKind::NvEnc) => Some("hevc_nvenc"),
            (Codec::H264, EncoderKind::Openh264) => None, // uses in-tree openh264, not libav
            (Codec::HevcMain10, EncoderKind::Openh264) => None,
        }
    }

    /// Bit depth of the codec's target pixel format.
    pub fn bit_depth(self) -> u8 {
        match self {
            Codec::H264 => 8,
            Codec::HevcMain10 => 10,
        }
    }
}

/// One frame's encoder stats — populated by every backend after `encode`.
#[derive(Debug, Clone, Copy, Default)]
pub struct FrameStats {
    /// Wall-clock time spent inside `encode`.
    pub encode_wallclock: Duration,
    /// Was this frame an IDR / keyframe?
    pub keyframe: bool,
    /// Bytes emitted for this frame (Annex-B NALU size).
    pub bytes: usize,
}

/// Errors returned by a concrete backend's construction / encode.
#[derive(Debug)]
pub enum EncoderError {
    /// The backend isn't linked (feature disabled) or the runtime probe
    /// determined the host can't drive it (no device, no supported
    /// codec, etc.).
    Unavailable {
        kind: EncoderKind,
        reason: String,
    },
    /// The backend failed to configure — usually bad dimensions or an
    /// out-of-range bitrate.
    ConfigFailed {
        kind: EncoderKind,
        detail: String,
    },
    /// An encode step failed at runtime (device disappeared, buffer
    /// mismatch, driver error).
    EncodeFailed {
        kind: EncoderKind,
        detail: String,
    },
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

/// Which input the encoder is willing to accept on this call.
///
/// `Rgba` is the general fallback (SHM readback path); `NV12` matches
/// hardware encoder's native input; `DmabufFd` is the zero-copy path
/// (VA-API only for now).
pub enum FrameInput<'a> {
    /// Row-major RGBA, `width * height * 4` bytes, no padding.
    Rgba(&'a [u8]),
    /// Planar NV12 (Y plane then interleaved UV plane).
    #[allow(dead_code)]
    Nv12 { y: &'a [u8], uv: &'a [u8] },
    /// DMABUF fds + per-plane offsets/strides. VA-API can import via
    /// `av_hwframe_map` on a `DRM_PRIME` source frame; other backends
    /// must reject with `EncoderError::EncodeFailed` and let the
    /// caller upload via a GPU→CPU readback.
    ///
    /// The plane arrays are fixed-size (matches libavutil's
    /// `AV_DRM_MAX_PLANES = 4`) so the callee doesn't have to
    /// heap-allocate on the hot path. `num_planes` is the count of
    /// valid entries in `plane_fds` / `plane_offsets` / `plane_strides`.
    /// Callers must zero the unused tail. A single-plane BGRA buffer
    /// has `num_planes = 1`; NV12 has 2 (Y then interleaved UV); P010
    /// has 2 (Y then interleaved UV, 10-bit).
    ///
    /// `plane_fds[i]` are borrowed for the encode call — the encoder
    /// dupes them itself via `av_hwframe_map` if it needs to hold on
    /// past the call. The caller keeps ownership.
    #[allow(dead_code)]
    DmabufFd {
        /// Per-plane file descriptors. Same fd may appear in every
        /// entry for a single-object multi-plane buffer.
        plane_fds: [std::os::fd::RawFd; 4],
        /// Per-plane byte offsets into the DMABUF.
        plane_offsets: [u64; 4],
        /// Per-plane strides (a.k.a. pitches / linesizes) in bytes.
        plane_strides: [u32; 4],
        /// Count of valid plane entries (1..=4).
        num_planes: u8,
        /// DRM_FORMAT_* fourcc code identifying the pixel layout.
        drm_fourcc: u32,
        /// DRM format modifier (vendor tiling / compression); use
        /// `DRM_FORMAT_MOD_LINEAR = 0` for uncompressed linear.
        drm_modifier: u64,
        /// Buffer width in pixels.
        width: u32,
        /// Buffer height in pixels.
        height: u32,
    },
}

/// Backend-agnostic H.264 encoder handle.
///
/// Every backend owns its device / codec context and pushes encoded
/// NALUs into the caller's buffer. The caller must not queue more than
/// one frame ahead of the encoder — this contract matches the flatpak-
/// share tick which encodes synchronously and ships the bytes.
pub trait Encoder: Send {
    /// Which concrete backend is running.
    fn kind(&self) -> EncoderKind;
    /// Bitstream codec being emitted. Distinct from `kind` because the
    /// same backend can drive multiple codecs (`h264_vaapi` vs
    /// `hevc_vaapi` on the same VA-API device). Wire and diagnostic
    /// paths read this to report the actual runtime codec + bit depth.
    fn codec(&self) -> Codec;
    /// Configured width, in pixels.
    fn width(&self) -> u32;
    /// Configured height, in pixels.
    fn height(&self) -> u32;
    /// Encode one frame. On success, appends the Annex-B NALU bytes to
    /// `out` and returns the frame's stats. `out` is a caller-owned
    /// buffer so backends that pre-know an upper bound (VA-API knows
    /// from the codec context, NVENC exposes it via
    /// `nvEncGetEncodeStats`) can `reserve` without allocating a new
    /// vec per frame.
    fn encode(
        &mut self,
        input: FrameInput<'_>,
        out: &mut Vec<u8>,
    ) -> Result<FrameStats, EncoderError>;
}

/// Static config passed to the backend constructor.
///
/// Kept as a plain struct so the backends can pull the fields they need
/// without every constructor growing more positional arguments as we
/// add knobs.
#[derive(Debug, Clone, Copy)]
pub struct EncoderInit {
    pub width: u32,
    pub height: u32,
    /// Target frame rate; used by the encoder's rate-control bookkeeping
    /// only. The wall-clock cadence is enforced by
    /// [`crate::network::streaming::StreamingThrottle`].
    pub fps: u32,
    /// Target bitrate in kilobits per second. `0` = backend default.
    pub bitrate_kbps: u32,
    /// Keyframe interval in seconds. `0` = backend default (openh264
    /// picks it internally; VA-API uses `fps` seconds).
    pub keyframe_interval_secs: u32,
    /// Bitstream codec to emit. Default [`Codec::H264`] preserves the
    /// prior SDR path.
    pub codec: Codec,
    /// Colorimetry the encoder tags the bitstream with. VUI on the SPS
    /// side, and side-data on every frame. Default is SDR BT.709.
    pub colorimetry: Colorimetry,
    /// HDR10 static metadata to embed as SEI. `None` = SDR, no SEI
    /// insertion; `Some(_)` = HDR pipeline, emit mastering-display + CLL
    /// SEIs per keyframe.
    pub hdr: Option<Hdr10Metadata>,
}

impl EncoderInit {
    /// Convenience — the SDR default matches every call before HDR
    /// support existed. Used by tests + code paths that haven't been
    /// migrated yet.
    pub const fn sdr_h264(
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
            codec: Codec::H264,
            colorimetry: Colorimetry::sdr_bt709(),
            hdr: None,
        }
    }

    /// HDR10 constructor. Wires the config's transfer + metadata
    /// through, forces HEVC Main10.
    pub fn hdr10_hevc(
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
            codec: Codec::HevcMain10,
            colorimetry: hdr.colorimetry(),
            hdr: Some(hdr.metadata),
        }
    }
}

/// Outcome of a runtime probe for one candidate backend.
///
/// Returned by [`probe`] so callers can report the *reason* a backend
/// was chosen (or skipped). This ends up in the `tracing::info!` at
/// session start.
#[derive(Debug, Clone)]
pub struct ProbeOutcome {
    pub kind: EncoderKind,
    pub codec: Codec,
    pub available: bool,
    pub reason: String,
}

/// Probe every compiled-in backend for the requested codec and return
/// the highest-priority available one, along with the outcomes of
/// every backend probed.
///
/// Priority order:
/// 1. NVENC (if compiled + supports codec)
/// 2. VA-API (if compiled + render node supports codec)
/// 3. openh264 (software) — only for [`Codec::H264`]
///
/// For [`Codec::HevcMain10`] the openh264 backend reports unavailable
/// (it only handles H.264 8-bit); a software HEVC fallback via libx265
/// is a follow-up. On a host with no hardware HEVC encoder,
/// `Codec::HevcMain10` probe returns [`EncoderKind::Openh264`] as the
/// picked kind with `available = false` — the caller must fall back to
/// SDR + [`Codec::H264`] or fail the share.
pub fn probe(
    codec: Codec,
    force: Option<EncoderChoice>,
) -> (EncoderKind, Vec<ProbeOutcome>) {
    let mut outcomes = Vec::new();
    match force {
        Some(EncoderChoice::Openh264) => {
            let outcome = openh264_probe(codec);
            outcomes.push(outcome);
            return (EncoderKind::Openh264, outcomes);
        }
        Some(EncoderChoice::Nvenc) => {
            let out = nvenc_probe(codec);
            let ok = out.available;
            outcomes.push(out);
            if ok {
                return (EncoderKind::NvEnc, outcomes);
            }
        }
        Some(EncoderChoice::Vaapi) => {
            let out = vaapi_probe(codec);
            let ok = out.available;
            outcomes.push(out);
            if ok {
                return (EncoderKind::VaApi, outcomes);
            }
        }
        Some(EncoderChoice::Auto) | None => {
            let nv = nvenc_probe(codec);
            let nv_ok = nv.available;
            outcomes.push(nv);
            if nv_ok {
                return (EncoderKind::NvEnc, outcomes);
            }
            let va = vaapi_probe(codec);
            let va_ok = va.available;
            outcomes.push(va);
            if va_ok {
                return (EncoderKind::VaApi, outcomes);
            }
        }
    }
    // Software backup — always available for H.264 only.
    let sw = openh264_probe(codec);
    outcomes.push(sw);
    (EncoderKind::Openh264, outcomes)
}

/// Openh264 always probes as available for H.264. Reports unavailable
/// for any 10-bit codec — the SVC encoder wraps Cisco's 8-bit-only
/// implementation.
fn openh264_probe(codec: Codec) -> ProbeOutcome {
    let (available, reason) = match codec {
        Codec::H264 => (true, "openh264 (software) is always available".to_string()),
        Codec::HevcMain10 => (
            false,
            "openh264 backend only supports H.264 8-bit; HEVC Main10 needs a hardware encoder"
                .to_string(),
        ),
    };
    ProbeOutcome {
        kind: EncoderKind::Openh264,
        codec,
        available,
        reason,
    }
}

/// VA-API probe. Without the `vaapi` feature this always reports
/// unavailable so the caller falls through. With the feature: opens
/// `/dev/dri/renderD*` and asks libavcodec whether the render node
/// exposes an encode profile for `codec`.
fn vaapi_probe(codec: Codec) -> ProbeOutcome {
    #[cfg(feature = "vaapi")]
    {
        return vaapi::probe(codec);
    }
    #[allow(unreachable_code)]
    ProbeOutcome {
        kind: EncoderKind::VaApi,
        codec,
        available: false,
        reason: "vaapi feature not compiled into this build".to_string(),
    }
}

/// NVENC probe. Feature-gated behind `nvenc`; queries libavcodec for
/// the encoder name matching `codec` on `EncoderKind::NvEnc`.
fn nvenc_probe(codec: Codec) -> ProbeOutcome {
    #[cfg(feature = "nvenc")]
    {
        return nvenc::probe(codec);
    }
    #[allow(unreachable_code)]
    ProbeOutcome {
        kind: EncoderKind::NvEnc,
        codec,
        available: false,
        reason: "nvenc feature not compiled into this build".to_string(),
    }
}

/// Construct the chosen encoder. Returns `EncoderError::Unavailable`
/// only if the chosen backend fails to instantiate after a successful
/// probe — a rare race (device disappeared between probe + open).
pub fn build(kind: EncoderKind, init: EncoderInit) -> Result<Box<dyn Encoder>, EncoderError> {
    match kind {
        EncoderKind::Openh264 => {
            openh264::OpenH264Backend::new(init).map(|e| Box::new(e) as Box<dyn Encoder>)
        }
        EncoderKind::VaApi => {
            #[cfg(feature = "vaapi")]
            {
                vaapi::VaApiBackend::new(init).map(|e| Box::new(e) as Box<dyn Encoder>)
            }
            #[cfg(not(feature = "vaapi"))]
            {
                Err(EncoderError::Unavailable {
                    kind: EncoderKind::VaApi,
                    reason: "vaapi feature not compiled".to_string(),
                })
            }
        }
        EncoderKind::NvEnc => {
            #[cfg(feature = "nvenc")]
            {
                nvenc::NvEncBackend::new(init).map(|e| Box::new(e) as Box<dyn Encoder>)
            }
            #[cfg(not(feature = "nvenc"))]
            {
                Err(EncoderError::Unavailable {
                    kind: EncoderKind::NvEnc,
                    reason: "nvenc feature not compiled".to_string(),
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openh264_probe_is_always_available_for_h264() {
        let outcome = openh264_probe(Codec::H264);
        assert_eq!(outcome.kind, EncoderKind::Openh264);
        assert_eq!(outcome.codec, Codec::H264);
        assert!(outcome.available);
    }

    #[test]
    fn openh264_probe_unavailable_for_hevc() {
        let outcome = openh264_probe(Codec::HevcMain10);
        assert_eq!(outcome.kind, EncoderKind::Openh264);
        assert!(!outcome.available, "openh264 must not claim HEVC support");
    }

    #[test]
    fn auto_probe_falls_back_to_openh264_when_no_hardware() {
        let (chosen, outcomes) = probe(Codec::H264, Some(EncoderChoice::Auto));
        assert_eq!(chosen, EncoderKind::Openh264);
        assert_eq!(outcomes.last().unwrap().kind, EncoderKind::Openh264);
        assert!(outcomes.last().unwrap().available);
    }

    #[test]
    fn explicit_openh264_choice_bypasses_hardware_probes() {
        let (chosen, outcomes) = probe(Codec::H264, Some(EncoderChoice::Openh264));
        assert_eq!(chosen, EncoderKind::Openh264);
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].kind, EncoderKind::Openh264);
    }

    #[test]
    fn requested_vaapi_falls_through_when_unavailable() {
        let (chosen, outcomes) = probe(Codec::H264, Some(EncoderChoice::Vaapi));
        assert!(outcomes.iter().any(|o| o.kind == EncoderKind::VaApi));
        #[cfg(not(feature = "vaapi"))]
        {
            assert_eq!(chosen, EncoderKind::Openh264);
        }
        let _ = chosen;
    }

    #[test]
    fn hevc_probe_on_stripped_host_still_returns_openh264_but_unavailable() {
        // On a host with no hardware HEVC support, the caller sees the
        // openh264 fallback picked but flagged unavailable — the session
        // uses that to fail HDR-share setup with a clear error rather
        // than silently downgrading to SDR.
        let (_chosen, outcomes) = probe(Codec::HevcMain10, Some(EncoderChoice::Auto));
        let last = outcomes.last().expect("has an outcome");
        assert_eq!(last.kind, EncoderKind::Openh264);
        assert_eq!(last.codec, Codec::HevcMain10);
        #[cfg(not(any(feature = "vaapi", feature = "nvenc")))]
        assert!(!last.available);
    }

    #[test]
    fn init_convenience_constructors_pair_codec_with_colorimetry() {
        let sdr = EncoderInit::sdr_h264(1280, 720, 30, 6000, 2);
        assert_eq!(sdr.codec, Codec::H264);
        assert_eq!(sdr.colorimetry, Colorimetry::sdr_bt709());
        assert!(sdr.hdr.is_none());

        let hdr = EncoderInit::hdr10_hevc(
            1920,
            1080,
            30,
            15_000,
            2,
            HdrConfig::pq_reference(),
        );
        assert_eq!(hdr.codec, Codec::HevcMain10);
        assert!(hdr.colorimetry.is_hdr());
        assert!(hdr.hdr.is_some());
    }

    #[test]
    fn codec_wire_label_stability() {
        // Wire labels are wire ABI — any change breaks receivers.
        assert_eq!(Codec::H264.wire_label(), "h264");
        assert_eq!(Codec::HevcMain10.wire_label(), "hevc-main10");
    }

    #[test]
    fn codec_libav_encoder_name_matches_ffmpeg() {
        assert_eq!(Codec::H264.libav_encoder_name(EncoderKind::VaApi), Some("h264_vaapi"));
        assert_eq!(Codec::HevcMain10.libav_encoder_name(EncoderKind::VaApi), Some("hevc_vaapi"));
        assert_eq!(Codec::H264.libav_encoder_name(EncoderKind::NvEnc), Some("h264_nvenc"));
        assert_eq!(Codec::HevcMain10.libav_encoder_name(EncoderKind::NvEnc), Some("hevc_nvenc"));
        // Openh264 is not a libavcodec encoder.
        assert_eq!(Codec::H264.libav_encoder_name(EncoderKind::Openh264), None);
    }
}
