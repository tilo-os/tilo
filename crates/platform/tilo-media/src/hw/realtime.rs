//! Real-time codec element tables and the order they are tried in.
//!
//! Playback lets `decodebin3` autoplug under the ranks in [`crate::hw`]. The
//! real-time path (screen sharing, calls, recording) names its elements
//! outright, so the order it tries them in lives here, next to the decode
//! ranking, rather than in each consumer:
//!
//! 1. fixed-function hardware: VA-API, NVCODEC, V4L2 (stateless decode
//!    first), then Vulkan Video decode;
//! 2. royalty-free software: dav1d / libaom for AV1 decode, SVT-AV1 / rav1e /
//!    libaom for AV1 encode, libvpx for VP9;
//! 3. H.264 in software only through Cisco's `openh264` binary, which the
//!    `openh264` `GStreamer` plugin loads from the system. Nothing here
//!    compiles an H.264 or HEVC implementation into Tilo.
//!
//! HEVC has no software encoder entry at all: the OS does not ship a patented
//! software encoder. Software decoders for patented codecs are listed last as
//! [`CodecTier::SystemProvided`] and are only used when the system itself
//! provides one (for example after the user installed the additional-codecs
//! pack).

use crate::encode::{EncoderKind, HwEncPolicy};
use crate::hw::{DecoderKind, HwPolicy};

/// A bitstream the real-time path can carry.
///
/// The wire labels are protocol: [`Self::wire_label`] is what peers put in a
/// stream announcement, and [`Self::from_wire_label`] is its inverse.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StreamCodec {
    /// H.264, 8-bit 4:2:0. The baseline every peer can decode.
    H264,
    /// HEVC Main10, the HDR path when a hardware encoder exists.
    HevcMain10,
    /// VP9 profile 0, royalty-free.
    Vp9,
    /// AV1 main profile (8- or 10-bit), royalty-free.
    Av1,
}

impl StreamCodec {
    /// Every codec this build knows.
    pub const ALL: [StreamCodec; 4] = [
        StreamCodec::H264,
        StreamCodec::HevcMain10,
        StreamCodec::Vp9,
        StreamCodec::Av1,
    ];

    /// The label carried on the wire.
    pub fn wire_label(self) -> &'static str {
        match self {
            StreamCodec::H264 => "h264",
            StreamCodec::HevcMain10 => "hevc-main10",
            StreamCodec::Vp9 => "vp9",
            StreamCodec::Av1 => "av1",
        }
    }

    /// Parse a wire label. `None` for a codec this build does not know.
    pub fn from_wire_label(label: &str) -> Option<Self> {
        match label {
            "h264" => Some(StreamCodec::H264),
            "hevc-main10" => Some(StreamCodec::HevcMain10),
            "vp9" => Some(StreamCodec::Vp9),
            "av1" => Some(StreamCodec::Av1),
            _ => None,
        }
    }

    /// The compressed caps both sides of the real-time path agree on: Annex-B
    /// access units for H.264/HEVC, one frame per buffer for VP9, one temporal
    /// unit of an OBU stream for AV1.
    pub fn stream_caps(self) -> &'static str {
        match self {
            StreamCodec::H264 => {
                "video/x-h264, stream-format=(string)byte-stream, alignment=(string)au"
            }
            StreamCodec::HevcMain10 => {
                "video/x-h265, stream-format=(string)byte-stream, alignment=(string)au"
            }
            StreamCodec::Vp9 => "video/x-vp9",
            StreamCodec::Av1 => {
                "video/x-av1, stream-format=(string)obu-stream, alignment=(string)tu"
            }
        }
    }

    /// The parser that normalises an encoder's output (and a peer's input)
    /// into [`Self::stream_caps`].
    pub fn parser(self) -> &'static str {
        match self {
            StreamCodec::H264 => "h264parse",
            StreamCodec::HevcMain10 => "h265parse",
            StreamCodec::Vp9 => "vp9parse",
            StreamCodec::Av1 => "av1parse",
        }
    }

    /// Bits per sample the codec carries on this path.
    pub fn bit_depth(self) -> u8 {
        match self {
            StreamCodec::HevcMain10 => 10,
            StreamCodec::H264 | StreamCodec::Vp9 | StreamCodec::Av1 => 8,
        }
    }

    /// Whether encoding this codec in software needs no patent licence.
    pub fn is_royalty_free(self) -> bool {
        matches!(self, StreamCodec::Vp9 | StreamCodec::Av1)
    }
}

/// Which tier of the real-time preference order an element belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CodecTier {
    /// Fixed-function video hardware.
    Hardware,
    /// A royalty-free software implementation (dav1d, libaom, SVT-AV1,
    /// rav1e, libvpx).
    RoyaltyFree,
    /// Cisco's `openh264` binary, loaded from the system at runtime.
    CiscoOpenH264,
    /// A software implementation of a patented codec that the system
    /// provides. Never shipped by Tilo; decode only, and last.
    SystemProvided,
}

impl CodecTier {
    pub fn as_str(self) -> &'static str {
        match self {
            CodecTier::Hardware => "hardware",
            CodecTier::RoyaltyFree => "royalty-free software",
            CodecTier::CiscoOpenH264 => "Cisco openh264",
            CodecTier::SystemProvided => "system-provided software",
        }
    }
}

/// Which hardware family (or none) an element drives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodecFamily {
    Va,
    NvCodec,
    V4l2,
    Vulkan,
    Software,
}

impl CodecFamily {
    /// The family name used in logs and by the override variables.
    pub fn as_str(self) -> &'static str {
        match self {
            CodecFamily::Va => "va",
            CodecFamily::NvCodec => "nvcodec",
            CodecFamily::V4l2 => "v4l2",
            CodecFamily::Vulkan => "vulkan",
            CodecFamily::Software => "software",
        }
    }
}

/// One real-time encoder or decoder element.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CodecElement {
    /// The `GStreamer` element factory name.
    pub element: &'static str,
    pub family: CodecFamily,
    pub tier: CodecTier,
}

impl CodecElement {
    /// Whether this element runs on video hardware.
    pub fn is_hardware(&self) -> bool {
        self.tier == CodecTier::Hardware
    }

    /// A one-line description for logs.
    pub fn describe(&self) -> String {
        format!(
            "{} ({}, {})",
            self.element,
            self.family.as_str(),
            self.tier.as_str()
        )
    }
}

const fn hw(element: &'static str, family: CodecFamily) -> CodecElement {
    CodecElement {
        element,
        family,
        tier: CodecTier::Hardware,
    }
}

const fn royalty_free(element: &'static str) -> CodecElement {
    CodecElement {
        element,
        family: CodecFamily::Software,
        tier: CodecTier::RoyaltyFree,
    }
}

const fn cisco(element: &'static str) -> CodecElement {
    CodecElement {
        element,
        family: CodecFamily::Software,
        tier: CodecTier::CiscoOpenH264,
    }
}

const fn system(element: &'static str) -> CodecElement {
    CodecElement {
        element,
        family: CodecFamily::Software,
        tier: CodecTier::SystemProvided,
    }
}

const ENCODE_H264: &[CodecElement] = &[
    hw("vah264enc", CodecFamily::Va),
    // Some Intel parts expose only the low-power entry point.
    hw("vah264lpenc", CodecFamily::Va),
    hw("nvh264enc", CodecFamily::NvCodec),
    hw("v4l2h264enc", CodecFamily::V4l2),
    cisco("openh264enc"),
];

const ENCODE_HEVC: &[CodecElement] = &[
    hw("vah265enc", CodecFamily::Va),
    hw("vah265lpenc", CodecFamily::Va),
    hw("nvh265enc", CodecFamily::NvCodec),
    hw("v4l2h265enc", CodecFamily::V4l2),
];

const ENCODE_VP9: &[CodecElement] = &[
    hw("vavp9enc", CodecFamily::Va),
    hw("vavp9lpenc", CodecFamily::Va),
    royalty_free("vp9enc"),
];

/// AV1 in software: libaom in its real-time usage profile, then rav1e in
/// low-latency mode.
///
/// `svtav1enc` is deliberately absent. Measured with SVT-AV1 3.1.2 under
/// `GStreamer` 1.28.6: in its low-delay prediction structure (the only one
/// without a lookahead) the element emits one frame and then blocks inside
/// `svt_av1_enc_get_packet` forever, and in its random-access structure every
/// frame arrives about 700 ms late. Neither is usable for a live stream; the
/// export path, which has no latency budget, still uses it.
const ENCODE_AV1: &[CodecElement] = &[
    hw("vaav1enc", CodecFamily::Va),
    hw("vaav1lpenc", CodecFamily::Va),
    hw("nvav1enc", CodecFamily::NvCodec),
    royalty_free("av1enc"),
    royalty_free("rav1enc"),
];

const DECODE_H264: &[CodecElement] = &[
    hw("vah264dec", CodecFamily::Va),
    hw("nvh264dec", CodecFamily::NvCodec),
    hw("v4l2slh264dec", CodecFamily::V4l2),
    hw("v4l2h264dec", CodecFamily::V4l2),
    hw("vulkanh264dec", CodecFamily::Vulkan),
    cisco("openh264dec"),
    system("avdec_h264"),
];

const DECODE_HEVC: &[CodecElement] = &[
    hw("vah265dec", CodecFamily::Va),
    hw("nvh265dec", CodecFamily::NvCodec),
    hw("v4l2slh265dec", CodecFamily::V4l2),
    hw("v4l2h265dec", CodecFamily::V4l2),
    hw("vulkanh265dec", CodecFamily::Vulkan),
    system("avdec_h265"),
    system("libde265dec"),
];

const DECODE_VP9: &[CodecElement] = &[
    hw("vavp9dec", CodecFamily::Va),
    hw("nvvp9dec", CodecFamily::NvCodec),
    hw("v4l2slvp9dec", CodecFamily::V4l2),
    hw("v4l2vp9dec", CodecFamily::V4l2),
    hw("vulkanvp9dec", CodecFamily::Vulkan),
    royalty_free("vp9dec"),
];

const DECODE_AV1: &[CodecElement] = &[
    hw("vaav1dec", CodecFamily::Va),
    hw("nvav1dec", CodecFamily::NvCodec),
    hw("v4l2slav1dec", CodecFamily::V4l2),
    hw("vulkanav1dec", CodecFamily::Vulkan),
    royalty_free("dav1ddec"),
    royalty_free("av1dec"),
];

/// Real-time encoders for `codec`, in declared preference order.
pub fn realtime_encoders(codec: StreamCodec) -> &'static [CodecElement] {
    match codec {
        StreamCodec::H264 => ENCODE_H264,
        StreamCodec::HevcMain10 => ENCODE_HEVC,
        StreamCodec::Vp9 => ENCODE_VP9,
        StreamCodec::Av1 => ENCODE_AV1,
    }
}

/// Real-time decoders for `codec`, in declared preference order.
pub fn realtime_decoders(codec: StreamCodec) -> &'static [CodecElement] {
    match codec {
        StreamCodec::H264 => DECODE_H264,
        StreamCodec::HevcMain10 => DECODE_HEVC,
        StreamCodec::Vp9 => DECODE_VP9,
        StreamCodec::Av1 => DECODE_AV1,
    }
}

/// The real-time hardware policy. Read from `TILO_MEDIA_HWENC` for encoders
/// and `TILO_MEDIA_HWDEC` for decoders, through the same parsers as the
/// export and playback paths, so one variable means one thing everywhere.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RealtimePolicy {
    /// Hardware first, then royalty-free software, then Cisco openh264.
    Auto,
    /// Software only.
    SoftwareOnly,
    /// This hardware family first; other hardware only after software.
    Prefer(CodecFamily),
}

impl RealtimePolicy {
    /// The encoder policy from `TILO_MEDIA_HWENC`.
    pub fn encoder_from_env() -> Self {
        match HwEncPolicy::from_env() {
            HwEncPolicy::Auto => RealtimePolicy::Auto,
            HwEncPolicy::Off => RealtimePolicy::SoftwareOnly,
            HwEncPolicy::Force(kind) => match kind {
                EncoderKind::Va => RealtimePolicy::Prefer(CodecFamily::Va),
                EncoderKind::NvCodec => RealtimePolicy::Prefer(CodecFamily::NvCodec),
                EncoderKind::V4l2 => RealtimePolicy::Prefer(CodecFamily::V4l2),
                EncoderKind::Qsv
                | EncoderKind::D3d
                | EncoderKind::VideoToolbox
                | EncoderKind::Software => RealtimePolicy::Auto,
            },
        }
    }

    /// The decoder policy from `TILO_MEDIA_HWDEC`.
    pub fn decoder_from_env() -> Self {
        match HwPolicy::from_env() {
            HwPolicy::Auto => RealtimePolicy::Auto,
            HwPolicy::Off => RealtimePolicy::SoftwareOnly,
            HwPolicy::Force(kind) => match kind {
                DecoderKind::Va => RealtimePolicy::Prefer(CodecFamily::Va),
                DecoderKind::NvCodec => RealtimePolicy::Prefer(CodecFamily::NvCodec),
                DecoderKind::V4l2 => RealtimePolicy::Prefer(CodecFamily::V4l2),
                DecoderKind::Vulkan => RealtimePolicy::Prefer(CodecFamily::Vulkan),
                DecoderKind::D3d | DecoderKind::VideoToolbox | DecoderKind::Software => {
                    RealtimePolicy::Auto
                }
            },
        }
    }

    /// Ordering key; lower is tried first. The declared order is kept inside
    /// a key by the stable sort in [`realtime_candidates`].
    fn order_key(self, e: &CodecElement) -> u8 {
        let tier = match e.tier {
            CodecTier::Hardware => 0,
            CodecTier::RoyaltyFree => 2,
            CodecTier::CiscoOpenH264 => 3,
            CodecTier::SystemProvided => 4,
        };
        match self {
            RealtimePolicy::Prefer(family) if e.is_hardware() && e.family != family => 5,
            RealtimePolicy::Prefer(_) | RealtimePolicy::Auto | RealtimePolicy::SoftwareOnly => tier,
        }
    }

    fn admits(self, e: &CodecElement) -> bool {
        !(self == RealtimePolicy::SoftwareOnly && e.is_hardware())
    }
}

/// The elements of `table` that `available` confirms, in the order `policy`
/// tries them.
///
/// `available` answers "is this element installed": the live registry in a
/// real build, a fabricated one in tests, which is the only way to prove the
/// hardware branch on a machine without that hardware.
pub fn realtime_candidates(
    table: &'static [CodecElement],
    policy: RealtimePolicy,
    available: impl Fn(&str) -> bool,
) -> Vec<&'static CodecElement> {
    let mut out: Vec<&'static CodecElement> = table
        .iter()
        .filter(|e| policy.admits(e) && available(e.element))
        .collect();
    out.sort_by_key(|e| policy.order_key(e));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry(names: &'static [&'static str]) -> impl Fn(&str) -> bool {
        move |n: &str| names.contains(&n)
    }

    fn names(v: &[&CodecElement]) -> Vec<&'static str> {
        v.iter().map(|e| e.element).collect()
    }

    #[test]
    fn realtime_encoder_tables_and_candidate_order() {
        // Phase `wire_labels_round_trip_and_keep_the_legacy_spellings`.
        {
            // "h264" and "hevc-main10" are what older peers already send.
            assert_eq!(StreamCodec::H264.wire_label(), "h264");
            assert_eq!(StreamCodec::HevcMain10.wire_label(), "hevc-main10");
            for c in StreamCodec::ALL {
                assert_eq!(StreamCodec::from_wire_label(c.wire_label()), Some(c));
            }
            assert_eq!(StreamCodec::from_wire_label("h266"), None);
        }
        // Phase `every_table_lists_hardware_before_software`.
        {
            for codec in StreamCodec::ALL {
                for table in [realtime_encoders(codec), realtime_decoders(codec)] {
                    let first_sw = table.iter().position(|e| !e.is_hardware());
                    if let Some(i) = first_sw {
                        assert!(
                            table[i..].iter().all(|e| !e.is_hardware()),
                            "{codec:?}: hardware listed after software in {:?}",
                            table.iter().map(|e| e.element).collect::<Vec<_>>()
                        );
                    }
                }
            }
        }
        // Phase `no_patented_software_encoder_is_ever_a_candidate`.
        {
            for codec in StreamCodec::ALL {
                for e in realtime_encoders(codec) {
                    assert_ne!(e.tier, CodecTier::SystemProvided, "{}", e.element);
                    if !e.is_hardware() && !codec.is_royalty_free() {
                        // The only software encoder for a patented codec is
                        // Cisco's separately distributed binary.
                        assert_eq!(e.tier, CodecTier::CiscoOpenH264, "{}", e.element);
                    }
                }
                assert!(
                    !realtime_encoders(codec)
                        .iter()
                        .any(|e| e.element == "x264enc" || e.element == "x265enc"),
                    "x264/x265 must never be selected"
                );
            }
            assert!(
                realtime_encoders(StreamCodec::HevcMain10)
                    .iter()
                    .all(CodecElement::is_hardware),
                "HEVC encode is hardware-only"
            );
        }
        // Phase `auto_prefers_hardware_then_royalty_free_then_cisco`.
        {
            let got = realtime_candidates(
                realtime_decoders(StreamCodec::Av1),
                RealtimePolicy::Auto,
                registry(&["av1dec", "dav1ddec", "vaav1dec"]),
            );
            assert_eq!(names(&got), ["vaav1dec", "dav1ddec", "av1dec"]);

            let got = realtime_candidates(
                realtime_decoders(StreamCodec::H264),
                RealtimePolicy::Auto,
                registry(&["avdec_h264", "openh264dec", "nvh264dec"]),
            );
            assert_eq!(names(&got), ["nvh264dec", "openh264dec", "avdec_h264"]);

            let got = realtime_candidates(
                realtime_encoders(StreamCodec::Av1),
                RealtimePolicy::Auto,
                registry(&["rav1enc", "svtav1enc", "av1enc"]),
            );
            assert_eq!(
                names(&got),
                ["av1enc", "rav1enc"],
                "libaom's real-time profile first; SVT-AV1 has no working low-delay mode"
            );
        }
        // Phase `software_only_never_offers_hardware`.
        {
            let got = realtime_candidates(
                realtime_encoders(StreamCodec::Vp9),
                RealtimePolicy::SoftwareOnly,
                registry(&["vavp9enc", "vp9enc"]),
            );
            assert_eq!(names(&got), ["vp9enc"]);
        }
        // Phase `a_preferred_family_leads_and_other_hardware_trails_software`.
        {
            let got = realtime_candidates(
                realtime_encoders(StreamCodec::H264),
                RealtimePolicy::Prefer(CodecFamily::NvCodec),
                registry(&["vah264enc", "nvh264enc", "openh264enc"]),
            );
            assert_eq!(names(&got), ["nvh264enc", "openh264enc", "vah264enc"]);
        }
        // Phase `an_empty_registry_yields_no_candidates_rather_than_a_guess`.
        {
            for codec in StreamCodec::ALL {
                assert!(
                    realtime_candidates(realtime_encoders(codec), RealtimePolicy::Auto, |_| false)
                        .is_empty()
                );
            }
        }
    }
}
