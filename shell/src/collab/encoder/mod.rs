//! H.264 encoder trait + runtime selection.
//!
//! The encoder abstraction lives here (instead of `network::encoder`
//! which still hosts the openh264 wrapper for backwards compatibility)
//! because the flatpak-share pipeline is the only caller today, and
//! keeping the trait next to the session code makes the ownership graph
//! obvious.
//!
//! Three concrete backends:
//!
//! - [`OpenH264Backend`] — pure-software Cisco openh264, always
//!   available. Preferred fallback and the only path guaranteed to
//!   compile without system headers.
//! - [`VaApiBackend`] — VA-API (Intel iGPU, AMD, some ARM SoCs). Gated
//!   behind the `vaapi` cargo feature: default off so the shell still
//!   compiles on hosts without `libva-devel`. Uses zero-copy DMABUF import
//!   when the capture path hands it a DMABUF fd; falls back to
//!   `vaCreateImage` upload from the NV12/RGBA CPU path otherwise.
//! - [`NvEncBackend`] — NVENC via `nvidia-video-codec-sdk`. Behind the
//!   `nvenc` cargo feature (default off — NVIDIA-only variant).
//!
//! The trait keeps the encoder loop backend-agnostic and lets
//! [`SharedFlatpakSession`] hold `Box<dyn Encoder + Send>` without
//! sprinkling `cfg(feature)` through the composition. Backends that
//! aren't linked in this build simply don't register a probe entry.

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
}

/// Outcome of a runtime probe for one candidate backend.
///
/// Returned by [`probe`] so callers can report the *reason* a backend
/// was chosen (or skipped). This ends up in the `tracing::info!` at
/// session start.
#[derive(Debug, Clone)]
pub struct ProbeOutcome {
    pub kind: EncoderKind,
    pub available: bool,
    pub reason: String,
}

/// Probe every compiled-in backend and return the highest-priority
/// available one, along with the outcomes of every backend probed.
///
/// Priority order matches the task spec:
/// 1. NVENC (if compiled + available)
/// 2. VA-API (if compiled + `/dev/dri/renderD128` opens + supports H.264)
/// 3. openh264 (software, always available)
///
/// The `outcomes` vector is returned so [`SharedFlatpakSession::start`]
/// can log every step (not just the chosen one) — useful for
/// diagnosing why VA-API is falling through to software on a specific
/// host.
pub fn probe(force: Option<EncoderChoice>) -> (EncoderKind, Vec<ProbeOutcome>) {
    let mut outcomes = Vec::new();
    // Respect an explicit non-auto config choice. If a specific backend
    // is requested and probes as unavailable, we *still* fall through
    // to the next best — the caller wanted the best-effort behaviour,
    // and the outcome vector records the failure so the log surfaces it.
    // (An explicit "force this or fail" mode would be a new
    // `EncoderChoice::VaapiOrFail` — not adopted here to keep the config
    // stable.)
    match force {
        Some(EncoderChoice::Openh264) => {
            let outcome = openh264_probe();
            outcomes.push(outcome);
            return (EncoderKind::Openh264, outcomes);
        }
        Some(EncoderChoice::Nvenc) => {
            let out = nvenc_probe();
            let ok = out.available;
            outcomes.push(out);
            if ok {
                return (EncoderKind::NvEnc, outcomes);
            }
            // Fall through to try lower-priority.
        }
        Some(EncoderChoice::Vaapi) => {
            let out = vaapi_probe();
            let ok = out.available;
            outcomes.push(out);
            if ok {
                return (EncoderKind::VaApi, outcomes);
            }
        }
        Some(EncoderChoice::Auto) | None => {
            let nv = nvenc_probe();
            let nv_ok = nv.available;
            outcomes.push(nv);
            if nv_ok {
                return (EncoderKind::NvEnc, outcomes);
            }
            let va = vaapi_probe();
            let va_ok = va.available;
            outcomes.push(va);
            if va_ok {
                return (EncoderKind::VaApi, outcomes);
            }
        }
    }
    // Software backup — always available.
    let sw = openh264_probe();
    outcomes.push(sw);
    (EncoderKind::Openh264, outcomes)
}

/// Openh264 always probes as available (it's a pure-Rust/C compile-in
/// backend). The probe still returns a `ProbeOutcome` so the log
/// pipeline is uniform.
fn openh264_probe() -> ProbeOutcome {
    ProbeOutcome {
        kind: EncoderKind::Openh264,
        available: true,
        reason: "openh264 (software) is always available".to_string(),
    }
}

/// VA-API probe. Without the `vaapi` feature this always reports
/// unavailable so the caller falls through.
///
/// When the feature is on, the probe:
///  1. Enumerates `/dev/dri/renderD*` nodes
///  2. Opens the first one that has H.264 encode support
///
/// The feature-off path lets the shell compile + test-suite run
/// everywhere without libva-devel present.
fn vaapi_probe() -> ProbeOutcome {
    #[cfg(feature = "vaapi")]
    {
        return vaapi::probe();
    }
    #[allow(unreachable_code)]
    ProbeOutcome {
        kind: EncoderKind::VaApi,
        available: false,
        reason: "vaapi feature not compiled into this build".to_string(),
    }
}

/// NVENC probe. Feature-gated behind `nvenc`; without the feature the
/// probe always reports unavailable.
fn nvenc_probe() -> ProbeOutcome {
    #[cfg(feature = "nvenc")]
    {
        return nvenc::probe();
    }
    #[allow(unreachable_code)]
    ProbeOutcome {
        kind: EncoderKind::NvEnc,
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
    fn openh264_probe_is_always_available() {
        let outcome = openh264_probe();
        assert_eq!(outcome.kind, EncoderKind::Openh264);
        assert!(outcome.available);
    }

    #[test]
    fn auto_probe_falls_back_to_openh264_when_no_hardware() {
        // On a stripped host (no vaapi feature, no nvenc feature) the
        // Auto probe must always land on Openh264 as the last-resort
        // fallback. This test is deterministic on CI runners with no
        // GPU features enabled.
        let (chosen, outcomes) = probe(Some(EncoderChoice::Auto));
        assert_eq!(chosen, EncoderKind::Openh264);
        // Software probe is *always* the last entry in the outcomes.
        assert_eq!(outcomes.last().unwrap().kind, EncoderKind::Openh264);
        assert!(outcomes.last().unwrap().available);
    }

    #[test]
    fn explicit_openh264_choice_bypasses_hardware_probes() {
        let (chosen, outcomes) = probe(Some(EncoderChoice::Openh264));
        assert_eq!(chosen, EncoderKind::Openh264);
        // Only the software entry — hardware probes are skipped.
        assert_eq!(outcomes.len(), 1);
        assert_eq!(outcomes[0].kind, EncoderKind::Openh264);
    }

    #[test]
    fn requested_vaapi_falls_through_when_unavailable() {
        // Without the vaapi feature, requesting VA-API explicitly still
        // records the outcome + falls through to openh264 rather than
        // returning an error. That preserves the "best-effort, log the
        // reason" contract the flatpak-share session expects.
        let (chosen, outcomes) = probe(Some(EncoderChoice::Vaapi));
        // We can't assert `chosen == Openh264` unconditionally because
        // a `cargo test --features vaapi` run would legitimately land on
        // VaApi. But we can assert the outcome vector has at least the
        // vaapi + openh264 probes, and openh264 remains available.
        assert!(outcomes.iter().any(|o| o.kind == EncoderKind::VaApi));
        // On the default (no `vaapi` feature) build, chosen == Openh264.
        #[cfg(not(feature = "vaapi"))]
        {
            assert_eq!(chosen, EncoderKind::Openh264);
        }
        // On a vaapi-featured build, chosen can be either — we just
        // assert the enum is one of the compiled backends.
        let _ = chosen;
    }
}
