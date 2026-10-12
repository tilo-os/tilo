//! Hardware decoder discovery, ranking, and reporting.
//!
//! Autoplugging picks a decoder by *rank*, and distributions do not agree on
//! what rank a hardware decoder should receive. This module raises hardware
//! decoders above software ones according to policy and reports the element
//! selected by the pipeline.
//!
//! [`HwReport`] is filled from the element instantiated by `GStreamer`, rather
//! than from configuration.
//!
//! `TILO_MEDIA_HWDEC` overrides the policy:
//!
//! | value | effect |
//! |---|---|
//! | `auto` (default) | prefer hardware, fall back to software |
//! | `off` | demote every hardware decoder; software only |
//! | `va` / `nvcodec` / `v4l2` / `d3d11` / `vulkan` | force that family to the top |
//!
//! The real-time path (screen sharing, calls, recording) names its encoders
//! and decoders outright instead of autoplugging; its element tables and the
//! order they are tried in live in [`realtime`], under the same variables
//! (`TILO_MEDIA_HWENC` for encoders, `TILO_MEDIA_HWDEC` for decoders).

/// Which decoder family a chosen element belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecoderKind {
    /// VA-API — Intel, AMD, and anything else with a libva driver.
    Va,
    /// NVIDIA NVDEC via the `nvcodec` plugin.
    NvCodec,
    /// Linux `V4L2` stateless / stateful codecs — embedded system-on-chip
    /// parts, Raspberry Pi.
    V4l2,
    /// Vulkan Video.
    Vulkan,
    /// Direct3D 11/12 video (present for completeness; not reachable here).
    D3d,
    /// Apple `VideoToolbox`.
    VideoToolbox,
    /// A CPU decoder — `avdec_*`, `openh264dec`, `dav1d`, `vp8dec`, …
    Software,
}

impl DecoderKind {
    /// Whether a decoder of this kind runs on fixed-function video hardware.
    pub fn is_hardware(self) -> bool {
        !matches!(self, DecoderKind::Software)
    }

    /// The family name used by `TILO_MEDIA_HWDEC` and reported in logs.
    pub fn as_str(self) -> &'static str {
        match self {
            DecoderKind::Va => "va",
            DecoderKind::NvCodec => "nvcodec",
            DecoderKind::V4l2 => "v4l2",
            DecoderKind::Vulkan => "vulkan",
            DecoderKind::D3d => "d3d11",
            DecoderKind::VideoToolbox => "videotoolbox",
            DecoderKind::Software => "software",
        }
    }

    /// Classify a decoder by its `GStreamer` element name.
    ///
    /// Names are matched against the prefix each plugin uses. `vaapi*` is the
    /// old gstreamer-vaapi plugin and `va*` the new one in -bad; both are
    /// VA-API. Ordering matters: `vah264dec` must not be read as `v4l2`, and
    /// the software `vp8dec` / `vp9dec` must not be read as VA-API, so the
    /// software names are tested first.
    pub fn classify(element_name: &str) -> Self {
        let n = element_name.to_ascii_lowercase();
        // Software decoders whose names would otherwise collide with a
        // hardware prefix (`vp8dec` starts with "vp", `dav1ddec` with "d").
        if n.starts_with("avdec_")
            || n.starts_with("openh264")
            || n.starts_with("dav1d")
            || n.starts_with("vp8dec")
            || n.starts_with("vp9dec")
            || n.starts_with("theoradec")
            || n.starts_with("libde265")
            || n.starts_with("avviddec")
        {
            return DecoderKind::Software;
        }
        if n.starts_with("vaapi") || n.starts_with("va") {
            return DecoderKind::Va;
        }
        if n.starts_with("nv") || n.starts_with("cuda") {
            return DecoderKind::NvCodec;
        }
        if n.starts_with("v4l2") {
            return DecoderKind::V4l2;
        }
        if n.starts_with("vulkan") {
            return DecoderKind::Vulkan;
        }
        if n.starts_with("d3d11") || n.starts_with("d3d12") || n.starts_with("mfdec") {
            return DecoderKind::D3d;
        }
        if n.starts_with("vtdec") || n.starts_with("videotoolbox") {
            return DecoderKind::VideoToolbox;
        }
        DecoderKind::Software
    }
}

/// The hardware-decode policy for this process, from `TILO_MEDIA_HWDEC`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HwPolicy {
    /// Prefer hardware; fall back to software when none can decode the stream.
    Auto,
    /// Software only.
    Off,
    /// Force one family to outrank everything, including other hardware.
    Force(DecoderKind),
}

impl HwPolicy {
    /// Read the policy from the environment. An unrecognised value is
    /// [`HwPolicy::Auto`] with a warning — a typo must not silently disable
    /// hardware decode.
    pub fn from_env() -> Self {
        match std::env::var("TILO_MEDIA_HWDEC") {
            Err(_) => HwPolicy::Auto,
            Ok(v) => Self::parse(&v),
        }
    }

    /// Parse one `TILO_MEDIA_HWDEC` value. Separated from the environment read
    /// so the mapping is exercised directly, without mutating process state.
    pub fn parse(v: &str) -> Self {
        match v.trim().to_ascii_lowercase().as_str() {
            "" | "auto" | "1" | "on" => HwPolicy::Auto,
            "off" | "0" | "none" | "sw" | "software" => HwPolicy::Off,
            "va" | "vaapi" => HwPolicy::Force(DecoderKind::Va),
            "nvcodec" | "nvdec" | "nvidia" | "cuda" => HwPolicy::Force(DecoderKind::NvCodec),
            "v4l2" | "v4l2sl" => HwPolicy::Force(DecoderKind::V4l2),
            "vulkan" => HwPolicy::Force(DecoderKind::Vulkan),
            "d3d11" | "d3d" => HwPolicy::Force(DecoderKind::D3d),
            other => {
                tracing::warn!(
                    "TILO_MEDIA_HWDEC={other:?} is not a known value \
                     (auto|off|va|nvcodec|v4l2|vulkan|d3d11); using auto"
                );
                HwPolicy::Auto
            }
        }
    }
}

/// What the pipeline did, filled from the elements `GStreamer` instantiated.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HwReport {
    /// The video decoder element's name, e.g. `nvh264dec0`, `vah264dec0`,
    /// `avdec_h264-0`. `None` until a decoder has been plugged.
    pub decoder_element: Option<String>,
    /// The decoder's family, classified from its name.
    pub decoder_kind: Option<DecoderKind>,
    /// The caps the video sink negotiated, verbatim, describing which
    /// memory the frames arrive in (`memory:DMABuf`, `memory:CUDAMemory`, or
    /// plain system memory).
    pub negotiated_caps: Option<String>,
    /// Every element the pipeline instantiated whose name looks like a
    /// decoder, in the order they appeared. Kept for diagnostics when the
    /// chosen decoder is not the expected one.
    pub decoder_candidates: Vec<String>,
}

impl HwReport {
    /// Whether a hardware decoder is doing the work. `false` both when a
    /// software decoder was plugged and when nothing has been plugged yet —
    /// so a caller can never read "not yet known" as "yes".
    pub fn is_hardware(&self) -> bool {
        self.decoder_kind.is_some_and(DecoderKind::is_hardware)
    }

    /// Whether frames arrive as dmabufs (the zero-copy path).
    pub fn is_dmabuf(&self) -> bool {
        self.negotiated_caps
            .as_deref()
            .is_some_and(|c| c.contains("memory:DMABuf"))
    }

    /// Record a newly instantiated element, keeping it only if it looks like a
    /// video decoder. Returns `true` when this became the chosen decoder.
    pub fn note_element(&mut self, name: &str) -> bool {
        if !looks_like_video_decoder(name) {
            return false;
        }
        self.decoder_candidates.push(name.to_string());
        // The first video decoder plugged is the one decoding the primary
        // video stream; a later one belongs to another stream (a second video
        // track, an embedded thumbnail) and must not overwrite the report.
        if self.decoder_element.is_none() {
            self.decoder_element = Some(name.to_string());
            self.decoder_kind = Some(DecoderKind::classify(name));
            return true;
        }
        false
    }

    /// A one-line summary for a log or a test failure message.
    pub fn describe(&self) -> String {
        match (&self.decoder_element, self.decoder_kind) {
            (Some(el), Some(kind)) => format!(
                "{el} ({}, {}) → {}",
                kind.as_str(),
                if kind.is_hardware() {
                    "hardware"
                } else {
                    "software"
                },
                self.negotiated_caps
                    .as_deref()
                    .unwrap_or("caps not yet negotiated"),
            ),
            _ => "no decoder plugged yet".to_string(),
        }
    }
}

/// Whether an element name is a video decoder rather than a parser, converter,
/// demuxer, or audio decoder.
///
/// Element instance names carry a trailing index (`nvh264dec0`), so this
/// matches on a `dec` segment rather than a suffix.
pub fn looks_like_video_decoder(name: &str) -> bool {
    // Audio decoders and generic converters would otherwise match on "dec".
    const AUDIO: &[&str] = &[
        "aac",
        "mp3",
        "mpegaudio",
        "vorbis",
        "opus",
        "flac",
        "alaw",
        "mulaw",
        "ac3",
        "dts",
        "wma",
        "sbc",
        "audio",
    ];
    // A known video codec token, or an explicit video-decoder plugin prefix.
    const VIDEO: &[&str] = &[
        "h264",
        "h265",
        "hevc",
        "av1",
        "vp8",
        "vp9",
        "mpeg2",
        "mpeg4",
        "mpegvideo",
        "jpeg",
        "theora",
        "av1dec",
        "prores",
        "dnxhd",
        "vc1",
        "wmv",
    ];

    let n = name.to_ascii_lowercase();
    if !n.contains("dec") {
        return false;
    }
    // `decodebin`/`uridecodebin`/`parsebin` are containers, not decoders.
    if n.starts_with("decodebin") || n.starts_with("uridecodebin") || n.starts_with("parsebin") {
        return false;
    }
    if AUDIO.iter().any(|a| n.contains(a)) {
        return false;
    }
    VIDEO.iter().any(|c| n.contains(c))
}

/// A decoder element discovered in the registry, with the rank it carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecoderEntry {
    pub name: String,
    pub kind: DecoderKind,
    pub rank: u32,
}

/// `GStreamer`'s `GST_RANK_PRIMARY`. Software decoders sit here; hardware ones
/// are lifted above it so autoplugging reaches them first.
pub const RANK_PRIMARY: u32 = 256;
/// The rank a preferred hardware decoder is raised to.
pub const RANK_HW_PREFERRED: u32 = RANK_PRIMARY + 128;
/// The rank a forced family is raised to — above every other hardware family.
pub const RANK_HW_FORCED: u32 = RANK_PRIMARY + 256;
/// Where a demoted decoder lands: below software, but still reachable if it is
/// the only element that can handle the stream.
pub const RANK_DEMOTED: u32 = 1;

/// The rank `policy` assigns to a decoder of `kind`.
///
/// Pure, so the policy is testable without a `GStreamer` registry.
pub fn rank_for(policy: HwPolicy, kind: DecoderKind) -> u32 {
    match policy {
        HwPolicy::Off => {
            if kind.is_hardware() {
                RANK_DEMOTED
            } else {
                RANK_PRIMARY
            }
        }
        HwPolicy::Auto => {
            if kind.is_hardware() {
                RANK_HW_PREFERRED
            } else {
                RANK_PRIMARY
            }
        }
        HwPolicy::Force(want) => {
            if kind == want {
                RANK_HW_FORCED
            } else if kind.is_hardware() {
                // Other hardware families stay available but below software,
                // so a forced family that cannot handle the stream falls back
                // to a CPU decoder rather than to a different GPU one.
                RANK_DEMOTED
            } else {
                RANK_PRIMARY
            }
        }
    }
}

/// The rank a decoder is given: a hardware family gets the rank `policy`
/// assigns, while a software decoder keeps the rank its plugin `shipped`
/// with. Flattening every CPU decoder to one rank would throw away the
/// ordering the plugins encode between themselves — dav1d above libaom for
/// AV1, libvpx above libavcodec for VP9 — and let the alphabet pick
/// instead.
pub fn effective_rank(policy: HwPolicy, kind: DecoderKind, shipped: u32) -> u32 {
    if kind.is_hardware() {
        rank_for(policy, kind)
    } else {
        shipped
    }
}

pub mod fallback;
pub mod realtime;
pub mod v4l2;

pub use realtime::{
    CodecElement, CodecFamily, CodecTier, RealtimePolicy, StreamCodec, realtime_candidates,
    realtime_decoders, realtime_encoders,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_classification_reads_what_was_actually_plugged() {
        // Phase `real_element_names_classify_to_the_right_family`.
        {
            // The names this machine actually produces.
            assert_eq!(DecoderKind::classify("nvh264dec0"), DecoderKind::NvCodec);
            assert_eq!(DecoderKind::classify("vah264dec0"), DecoderKind::Va);
            assert_eq!(DecoderKind::classify("avdec_h264-0"), DecoderKind::Software);
            // And the ones other hardware produces.
            assert_eq!(DecoderKind::classify("vaapih265dec"), DecoderKind::Va);
            assert_eq!(DecoderKind::classify("v4l2h264dec"), DecoderKind::V4l2);
            // The stateless family the Hantro / rkvdec / Cedrus blocks drive.
            assert_eq!(DecoderKind::classify("v4l2slh264dec0"), DecoderKind::V4l2);
            assert_eq!(DecoderKind::classify("v4l2slvp8dec"), DecoderKind::V4l2);
            assert_eq!(DecoderKind::classify("v4l2slmpeg2dec"), DecoderKind::V4l2);
            assert!(looks_like_video_decoder("v4l2slh264dec0"));
            assert!(looks_like_video_decoder("v4l2slmpeg2dec0"));
            assert_eq!(DecoderKind::classify("vulkanh264dec"), DecoderKind::Vulkan);
            assert_eq!(DecoderKind::classify("d3d11h264dec"), DecoderKind::D3d);
        }
        // Phase `software_decoders_with_hardware_looking_names_are_not_mistaken_for_gpus`.
        {
            // `vp8dec`/`vp9dec` are libvpx on the CPU but start with "v"; calling
            // them VA-API would report hardware decode that never happened.
            assert_eq!(DecoderKind::classify("vp9dec0"), DecoderKind::Software);
            assert_eq!(DecoderKind::classify("vp8dec0"), DecoderKind::Software);
            assert_eq!(DecoderKind::classify("dav1ddec0"), DecoderKind::Software);
            assert_eq!(DecoderKind::classify("openh264dec0"), DecoderKind::Software);
            assert!(!DecoderKind::classify("vp9dec0").is_hardware());
        }
        // Phase `decoder_detection_ignores_parsers_containers_and_audio`.
        {
            assert!(looks_like_video_decoder("nvh264dec0"));
            assert!(looks_like_video_decoder("vah264dec0"));
            assert!(looks_like_video_decoder("avdec_h264-0"));
            assert!(
                !looks_like_video_decoder("h264parse0"),
                "a parser is not a decoder"
            );
            assert!(
                !looks_like_video_decoder("decodebin3-0"),
                "a bin is not a decoder"
            );
            assert!(!looks_like_video_decoder("uridecodebin3-0"));
            assert!(!looks_like_video_decoder("parsebin0"));
            assert!(
                !looks_like_video_decoder("avdec_aac0"),
                "audio decoder excluded"
            );
            assert!(!looks_like_video_decoder("opusdec0"));
            assert!(
                !looks_like_video_decoder("videoconvert0"),
                "a converter is not a decoder"
            );
            assert!(!looks_like_video_decoder("qtdemux0"));
        }
        // Phase `report_never_claims_hardware_before_a_decoder_is_plugged`.
        {
            let mut r = HwReport::default();
            assert!(!r.is_hardware(), "unknown is not hardware");
            assert!(!r.is_dmabuf());
            assert_eq!(r.describe(), "no decoder plugged yet");

            assert!(
                r.note_element("nvh264dec0"),
                "first video decoder is the chosen one"
            );
            assert!(r.is_hardware());
            assert_eq!(r.decoder_kind, Some(DecoderKind::NvCodec));
        }
        // Phase `a_second_decoder_does_not_overwrite_the_chosen_one`.
        {
            let mut r = HwReport::default();
            r.note_element("nvh264dec0");
            // A second video track's decoder must not rewrite the report — the
            // preview is showing the first stream.
            assert!(!r.note_element("avdec_h264-1"));
            assert_eq!(r.decoder_element.as_deref(), Some("nvh264dec0"));
            assert!(r.is_hardware(), "still reporting the real primary decoder");
            assert_eq!(r.decoder_candidates.len(), 2, "both kept for diagnostics");
        }
        // Phase `dmabuf_is_read_from_the_negotiated_caps_not_assumed`.
        {
            let mut r = HwReport::default();
            r.note_element("vah264dec0");
            assert!(!r.is_dmabuf(), "hardware alone does not imply dmabuf");
            r.negotiated_caps = Some("video/x-raw(memory:CUDAMemory), format=(string)NV12".into());
            assert!(!r.is_dmabuf(), "CUDA memory is hardware but not dmabuf");
            r.negotiated_caps =
                Some("video/x-raw(memory:DMABuf), drm-format=(string)NV12:0x0".into());
            assert!(r.is_dmabuf());
        }
    }

    #[test]
    fn decode_policy_ranks_hardware_families_against_software() {
        // Phase `policy_off_demotes_hardware_below_software`.
        {
            assert!(
                rank_for(HwPolicy::Off, DecoderKind::NvCodec)
                    < rank_for(HwPolicy::Off, DecoderKind::Software),
                "with hardware off, software must win autoplug"
            );
        }
        // Phase `policy_auto_lifts_every_hardware_family_above_software`.
        {
            for k in [
                DecoderKind::Va,
                DecoderKind::NvCodec,
                DecoderKind::V4l2,
                DecoderKind::Vulkan,
            ] {
                assert!(
                    rank_for(HwPolicy::Auto, k) > rank_for(HwPolicy::Auto, DecoderKind::Software),
                    "{} must outrank software under auto",
                    k.as_str()
                );
            }
        }
        // Phase `forcing_a_family_puts_it_above_the_others_and_leaves_software_as_the_fallback`.
        {
            let p = HwPolicy::Force(DecoderKind::Va);
            assert!(rank_for(p, DecoderKind::Va) > rank_for(p, DecoderKind::NvCodec));
            assert!(rank_for(p, DecoderKind::Va) > rank_for(p, DecoderKind::Software));
            // A forced family that cannot handle the stream must fall through to
            // software, not to a different GPU family.
            assert!(rank_for(p, DecoderKind::Software) > rank_for(p, DecoderKind::NvCodec));
        }
        // Phase `software_decoders_keep_their_shipped_order_under_every_policy`.
        {
            // dav1ddec ships one above libaom's av1dec; vp9dec above avdec_vp9.
            let dav1d = 257;
            let libaom = 256;
            for p in [
                HwPolicy::Auto,
                HwPolicy::Off,
                HwPolicy::Force(DecoderKind::V4l2),
            ] {
                assert!(
                    effective_rank(p, DecoderKind::Software, dav1d)
                        > effective_rank(p, DecoderKind::Software, libaom),
                    "{p:?} must not flatten the software order"
                );
            }
            assert!(
                effective_rank(HwPolicy::Auto, DecoderKind::V4l2, 0)
                    > effective_rank(HwPolicy::Auto, DecoderKind::Software, dav1d),
                "hardware still outranks the best software decoder"
            );
        }
        // Phase `env_policy_parses_the_documented_values_and_survives_a_typo`.
        {
            for (v, want) in [
                ("auto", HwPolicy::Auto),
                ("", HwPolicy::Auto),
                ("ON", HwPolicy::Auto),
                ("off", HwPolicy::Off),
                ("software", HwPolicy::Off),
                (" va ", HwPolicy::Force(DecoderKind::Va)),
                ("vaapi", HwPolicy::Force(DecoderKind::Va)),
                ("nvcodec", HwPolicy::Force(DecoderKind::NvCodec)),
                ("nvdec", HwPolicy::Force(DecoderKind::NvCodec)),
                ("v4l2", HwPolicy::Force(DecoderKind::V4l2)),
                ("vulkan", HwPolicy::Force(DecoderKind::Vulkan)),
            ] {
                assert_eq!(HwPolicy::parse(v), want, "TILO_MEDIA_HWDEC={v:?}");
            }
            assert_eq!(
                HwPolicy::parse("wat"),
                HwPolicy::Auto,
                "a typo must never silently disable hardware decode"
            );
        }
    }
}
