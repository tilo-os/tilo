//! Encoder discovery and the record of what was selected. Encoding has no
//! autoplugger, so this module declares a preference order per codec and picks
//! the first encoder the registry has.
//!
//! `GStreamer` encoders disagree on bitrate units: `openh264enc` and `vp8enc` take
//! bits per second, while every VA-API, NVENC, QSV and `x264enc` encoder takes
//! kilobits. Each [`EncoderSpec`] carries its property name and its units, and
//! [`EncoderSpec::bitrate_arg`] converts.
//!
//! `TILO_MEDIA_HWENC`: `auto` (the default) prefers hardware and falls back to
//! software, `off` takes software only, and `va`, `nvcodec`, `qsv` or `v4l2` tries
//! that family first, then software.

#[cfg(gst_encode)]
pub mod graph;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncoderKind {
    Va,
    NvCodec,
    Qsv,
    V4l2,
    D3d,
    VideoToolbox,
    Software,
}

impl EncoderKind {
    pub fn is_hardware(self) -> bool {
        !matches!(self, EncoderKind::Software)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            EncoderKind::Va => "va",
            EncoderKind::NvCodec => "nvcodec",
            EncoderKind::Qsv => "qsv",
            EncoderKind::V4l2 => "v4l2",
            EncoderKind::D3d => "d3d11",
            EncoderKind::VideoToolbox => "videotoolbox",
            EncoderKind::Software => "software",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BitrateUnits {
    BitsPerSecond,
    KilobitsPerSecond,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncoderSpec {
    pub element: &'static str,
    pub kind: EncoderKind,
    pub bitrate_property: &'static str,
    pub bitrate_units: BitrateUnits,
    /// Set where this build measured the encoder ignoring its bitrate setting: the
    /// property is accepted either way, so only an encode can tell. `false` means no
    /// observation has shown it, not that the rate is accurate.
    pub ignores_bitrate: bool,
    /// Properties every encode sets, as `gst-launch` `property=value` pairs; empty
    /// for most encoders. `x264enc` applies its speed preset's B-frames otherwise,
    /// which delays its first frame so an MP4 presents its first picture two frames
    /// in, behind the audio; asking for none keeps every export starting at zero.
    pub fixed_args: &'static str,
}

impl EncoderSpec {
    /// The number to write to [`Self::bitrate_property`] for `bits_per_second`.
    /// Floored at 1 kbps, because a kilobit encoder given 0 reads it as "auto".
    pub fn bitrate_value(&self, bits_per_second: u64) -> u64 {
        match self.bitrate_units {
            BitrateUnits::BitsPerSecond => bits_per_second,
            BitrateUnits::KilobitsPerSecond => {
                if bits_per_second == 0 {
                    0
                } else {
                    (bits_per_second / 1000).max(1)
                }
            }
        }
    }

    pub fn bitrate_arg(&self, bits_per_second: u64) -> String {
        format!(
            "{}={}",
            self.bitrate_property,
            self.bitrate_value(bits_per_second)
        )
    }

    pub fn launch_args(&self, bits_per_second: u64) -> String {
        let bitrate = self.bitrate_arg(bits_per_second);
        if self.fixed_args.is_empty() {
            bitrate
        } else {
            format!("{bitrate} {}", self.fixed_args)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoCodec {
    H264,
    Vp8,
    Vp9,
    Av1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioCodec {
    Aac,
    Opus,
}

// VA-API first, as it covers AMD and Intel hardware. `x264enc` outranks
// `openh264enc`, which writes Baseline profile only (no CABAC) and is worse at
// the same bitrate. Neither writes B-frames: see [`EncoderSpec::fixed_args`].
const H264: &[EncoderSpec] = &[
    kbit("vah264enc", EncoderKind::Va),
    kbit("nvh264enc", EncoderKind::NvCodec),
    kbit("qsvh264enc", EncoderKind::Qsv),
    // Some Intel iGPUs expose only the low-power VA encoder.
    kbit("vah264lpenc", EncoderKind::Va),
    // The older gstreamer-vaapi plugin, still shipped by some distributions.
    kbit("vaapih264enc", EncoderKind::Va),
    kbit("v4l2h264enc", EncoderKind::V4l2),
    EncoderSpec {
        fixed_args: "bframes=0",
        ..kbit("x264enc", EncoderKind::Software)
    },
    H264_BASELINE,
];
const H264_BASELINE: EncoderSpec = bits_untracked("openh264enc", EncoderKind::Software);

const VP8: &[EncoderSpec] = &[
    kbit("vavp8enc", EncoderKind::Va),
    kbit("vaapivp8enc", EncoderKind::Va),
    VP8_BASELINE,
];
const VP8_BASELINE: EncoderSpec = target_bits("vp8enc");

const VP9: &[EncoderSpec] = &[
    kbit("vavp9enc", EncoderKind::Va),
    kbit("qsvvp9enc", EncoderKind::Qsv),
    kbit("vaapivp9enc", EncoderKind::Va),
    VP9_BASELINE,
];
const VP9_BASELINE: EncoderSpec = target_bits("vp9enc");

const AV1: &[EncoderSpec] = &[
    kbit("vaav1enc", EncoderKind::Va),
    kbit("qsvav1enc", EncoderKind::Qsv),
    kbit("nvav1enc", EncoderKind::NvCodec),
    AV1_BASELINE,
];
const AV1_BASELINE: EncoderSpec = EncoderSpec {
    bitrate_property: "target-bitrate",
    ..kbit("svtav1enc", EncoderKind::Software)
};

// AAC: no hardware encoders exist, so the choice is quality. `fdkaacenc`
// (Fraunhofer) is the better one; `avenc_aac` is always there with the `libav`
// plugin.
const AAC: &[EncoderSpec] = &[
    bits("fdkaacenc", EncoderKind::Software),
    bits("avenc_aac", EncoderKind::Software),
    AAC_BASELINE,
];
const AAC_BASELINE: EncoderSpec = bits("voaacenc", EncoderKind::Software);

const OPUS: &[EncoderSpec] = &[OPUS_BASELINE];
const OPUS_BASELINE: EncoderSpec = bits("opusenc", EncoderKind::Software);

const fn bits(element: &'static str, kind: EncoderKind) -> EncoderSpec {
    EncoderSpec {
        element,
        kind,
        bitrate_property: "bitrate",
        bitrate_units: BitrateUnits::BitsPerSecond,
        ignores_bitrate: false,
        fixed_args: "",
    }
}

/// [`bits`] for an encoder measured not to track the rate it is given: see
/// [`EncoderSpec::ignores_bitrate`].
const fn bits_untracked(element: &'static str, kind: EncoderKind) -> EncoderSpec {
    EncoderSpec {
        ignores_bitrate: true,
        ..bits(element, kind)
    }
}

const fn kbit(element: &'static str, kind: EncoderKind) -> EncoderSpec {
    EncoderSpec {
        element,
        kind,
        bitrate_property: "bitrate",
        bitrate_units: BitrateUnits::KilobitsPerSecond,
        ignores_bitrate: false,
        fixed_args: "",
    }
}

const fn target_bits(element: &'static str) -> EncoderSpec {
    EncoderSpec {
        element,
        kind: EncoderKind::Software,
        bitrate_property: "target-bitrate",
        bitrate_units: BitrateUnits::BitsPerSecond,
        ignores_bitrate: true,
        fixed_args: "",
    }
}

impl VideoCodec {
    pub fn candidates(self) -> &'static [EncoderSpec] {
        match self {
            VideoCodec::H264 => H264,
            VideoCodec::Vp8 => VP8,
            VideoCodec::Vp9 => VP9,
            VideoCodec::Av1 => AV1,
        }
    }

    pub fn baseline(self) -> &'static EncoderSpec {
        match self {
            VideoCodec::H264 => &H264_BASELINE,
            VideoCodec::Vp8 => &VP8_BASELINE,
            VideoCodec::Vp9 => &VP9_BASELINE,
            VideoCodec::Av1 => &AV1_BASELINE,
        }
    }
}

impl AudioCodec {
    pub fn candidates(self) -> &'static [EncoderSpec] {
        match self {
            AudioCodec::Aac => AAC,
            AudioCodec::Opus => OPUS,
        }
    }

    pub fn baseline(self) -> &'static EncoderSpec {
        match self {
            AudioCodec::Aac => &AAC_BASELINE,
            AudioCodec::Opus => &OPUS_BASELINE,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HwEncPolicy {
    Auto,
    /// Software only.
    Off,
    /// One family first, then software, then other hardware: a forced family that
    /// cannot encode this codec falls back to the CPU rather than to another GPU.
    Force(EncoderKind),
}

impl HwEncPolicy {
    // An unrecognised value is [`HwEncPolicy::Auto`] with a warning: a typo
    // must not silently disable hardware encode.
    pub fn from_env() -> Self {
        match std::env::var("TILO_MEDIA_HWENC") {
            Ok(v) => Self::parse(&v),
            Err(_) => HwEncPolicy::Auto,
        }
    }

    // Exposed so parsing is testable without mutating the process environment.
    pub fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "" | "auto" | "1" | "on" => HwEncPolicy::Auto,
            "off" | "0" | "none" | "sw" | "software" => HwEncPolicy::Off,
            "va" | "vaapi" => HwEncPolicy::Force(EncoderKind::Va),
            "nvcodec" | "nvenc" | "nvidia" => HwEncPolicy::Force(EncoderKind::NvCodec),
            "qsv" | "quicksync" => HwEncPolicy::Force(EncoderKind::Qsv),
            "v4l2" => HwEncPolicy::Force(EncoderKind::V4l2),
            other => {
                tracing::warn!(
                    "media: TILO_MEDIA_HWENC={other:?} is not a known value; using auto"
                );
                HwEncPolicy::Auto
            }
        }
    }

    fn admits(self, kind: EncoderKind) -> bool {
        match self {
            // A preference, not an exclusion: if the forced encoder is absent the
            // others are still worth trying.
            HwEncPolicy::Auto | HwEncPolicy::Force(_) => true,
            HwEncPolicy::Off => !kind.is_hardware(),
        }
    }

    fn tier(self, kind: EncoderKind) -> u8 {
        match self {
            HwEncPolicy::Auto | HwEncPolicy::Off => 0,
            HwEncPolicy::Force(want) => {
                if kind == want {
                    0
                } else if kind.is_hardware() {
                    2
                } else {
                    1
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    pub spec: &'static EncoderSpec,
    /// Whether a registry confirmed this element is installed. `false` means the
    /// caller could not look — no `GStreamer` linked, or every candidate missing —
    /// and [`Self::spec`] is the declared baseline. Never read it as "software will
    /// run", but as "nothing has been observed".
    pub confirmed: bool,
    /// Candidates ahead of the chosen one that the registry did not have, in the
    /// order they were tried. Diagnostics for "why is this not using my GPU".
    pub passed_over: Vec<&'static str>,
}

impl Selection {
    /// Whether the encode will run on video hardware. False for a software encoder
    /// and for an unconfirmed choice, so "not yet known" never reads as yes.
    pub fn is_hardware(&self) -> bool {
        self.confirmed && self.spec.kind.is_hardware()
    }

    /// Whether a size predicted from the requested bitrate means anything: false
    /// when the chosen encoder is [recorded as ignoring its
    /// bitrate](EncoderSpec::ignores_bitrate), and when nothing was confirmed
    /// present ([`Self::confirmed`]). Either way, do not quote a size.
    pub fn bitrate_is_predictive(&self) -> bool {
        self.confirmed && !self.spec.ignores_bitrate
    }

    pub fn describe(&self) -> String {
        if !self.confirmed {
            return format!(
                "{} (baseline; not found in a registry, nothing observed)",
                self.spec.element
            );
        }
        format!(
            "{} ({}, {})",
            self.spec.element,
            self.spec.kind.as_str(),
            if self.spec.kind.is_hardware() {
                "hardware"
            } else {
                "software"
            },
        )
    }
}

/// Pick an encoder for `codec`, trying each candidate against `available`, which
/// answers "is this element factory installed". A parameter so the preference
/// order is testable against a fabricated registry, including ones this machine
/// cannot have.
pub fn select_video(
    codec: VideoCodec,
    policy: HwEncPolicy,
    available: impl Fn(&str) -> bool,
) -> Selection {
    select(codec.candidates(), codec.baseline(), policy, available)
}

pub fn select_audio(
    codec: AudioCodec,
    policy: HwEncPolicy,
    available: impl Fn(&str) -> bool,
) -> Selection {
    select(codec.candidates(), codec.baseline(), policy, available)
}

fn select(
    candidates: &'static [EncoderSpec],
    baseline: &'static EncoderSpec,
    policy: HwEncPolicy,
    available: impl Fn(&str) -> bool,
) -> Selection {
    // Stable sort by policy tier keeps the declared preference order inside each
    // tier, so `Auto` walks the table as written.
    let mut order: Vec<&'static EncoderSpec> = candidates
        .iter()
        .filter(|s| policy.admits(s.kind))
        .collect();
    order.sort_by_key(|s| policy.tier(s.kind));

    let mut passed_over = Vec::new();
    for spec in order {
        if available(spec.element) {
            return Selection {
                spec,
                confirmed: true,
                passed_over,
            };
        }
        passed_over.push(spec.element);
    }
    Selection {
        spec: baseline,
        confirmed: false,
        passed_over,
    }
}

/// Which encoders an export selected: a record of what was chosen, not a claim
/// about what the machine can do.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EncodeReport {
    pub video: Option<Selection>,
    pub audio: Option<Selection>,
}

impl EncodeReport {
    pub fn is_hardware(&self) -> bool {
        self.video.as_ref().is_some_and(Selection::is_hardware)
    }

    pub fn describe(&self) -> String {
        let v = self.video.as_ref().map_or_else(
            || "no video encoder selected".to_string(),
            Selection::describe,
        );
        match &self.audio {
            Some(a) => format!("video: {v}; audio: {}", a.describe()),
            None => format!("video: {v}; audio: none"),
        }
    }
}

/// Whether `name` is an element factory in this process's registry: the real
/// answer to [`select_video`]'s `available` question.
///
/// Initialises `GStreamer` first, because an uninitialised lookup reports every
/// encoder missing.
#[cfg(any(gst_decode, gst_encode, gst_realtime))]
pub fn registry_has(name: &str) -> bool {
    if gstreamer::init().is_ok() {
        crate::extensions::refresh_registry();
    }
    gstreamer::ElementFactory::find(name).is_some()
}

#[cfg(test)]
#[allow(
    clippy::cast_precision_loss,
    reason = "byte counts and bitrates widened to f64 only to print them as \
              Mbit/s in a failure message; the assertions themselves compare \
              integers"
)]
mod tests {
    use super::*;

    fn registry(names: &'static [&'static str]) -> impl Fn(&str) -> bool {
        move |n: &str| names.contains(&n)
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn encoder_choice_ranks_hardware_policy_and_software_fallbacks() {
        {
            // Proved against a fabricated registry: this machine has no H.264
            // hardware encoder.
            for (present, want, family) in [
                (
                    &["vah264enc", "x264enc", "openh264enc"][..],
                    "vah264enc",
                    EncoderKind::Va,
                ),
                (
                    &["nvh264enc", "x264enc", "openh264enc"][..],
                    "nvh264enc",
                    EncoderKind::NvCodec,
                ),
                (
                    &["qsvh264enc", "openh264enc"][..],
                    "qsvh264enc",
                    EncoderKind::Qsv,
                ),
                (
                    &["vah264lpenc", "openh264enc"][..],
                    "vah264lpenc",
                    EncoderKind::Va,
                ),
                (
                    &["vaapih264enc", "openh264enc"][..],
                    "vaapih264enc",
                    EncoderKind::Va,
                ),
            ] {
                let sel = select_video(VideoCodec::H264, HwEncPolicy::Auto, registry(present));
                assert_eq!(sel.spec.element, want, "with {present:?} installed");
                assert_eq!(sel.spec.kind, family);
                assert!(
                    sel.confirmed,
                    "the registry confirmed it: {}",
                    sel.describe()
                );
                assert!(
                    sel.is_hardware(),
                    "{want} is a hardware encoder: {}",
                    sel.describe()
                );
            }
        }
        {
            // x264enc is preferred over openh264enc: openh264 writes Baseline
            // profile only, so it is measurably worse at the same bitrate.
            let sel = select_video(
                VideoCodec::H264,
                HwEncPolicy::Auto,
                registry(&["x264enc", "openh264enc"]),
            );
            assert_eq!(sel.spec.element, "x264enc", "{}", sel.describe());
            assert!(sel.confirmed);
            assert!(!sel.is_hardware(), "x264 is software: {}", sel.describe());
            assert!(
                sel.passed_over.contains(&"vah264enc") && sel.passed_over.contains(&"nvh264enc"),
                "the hardware encoders it looked for are reported: {:?}",
                sel.passed_over,
            );

            let sel = select_video(
                VideoCodec::H264,
                HwEncPolicy::Auto,
                registry(&["openh264enc"]),
            );
            assert_eq!(sel.spec.element, "openh264enc", "{}", sel.describe());
            assert!(!sel.is_hardware());
        }
        {
            // The state that must not be mistaken for a yes: nothing was found.
            let sel = select_video(VideoCodec::H264, HwEncPolicy::Auto, |_| false);
            assert!(!sel.confirmed, "nothing was confirmed");
            assert!(
                !sel.is_hardware(),
                "an unobserved selection is not a hardware claim"
            );
            assert_eq!(
                sel.spec.element, "openh264enc",
                "it falls back to the baseline"
            );
            assert!(
                sel.describe().contains("nothing observed"),
                "the description says it never looked: {}",
                sel.describe(),
            );

            // Even when the baseline itself is a hardware element, an unconfirmed selection
            // must not report hardware.
            let unconfirmed = Selection {
                spec: &H264[0],
                confirmed: false,
                passed_over: Vec::new(),
            };
            assert!(
                unconfirmed.spec.kind.is_hardware(),
                "vah264enc is a hardware element"
            );
            assert!(
                !unconfirmed.is_hardware(),
                "unconfirmed outranks the family: {}",
                unconfirmed.describe(),
            );
        }
        {
            let full = registry(&["vah264enc", "nvh264enc", "x264enc", "openh264enc"]);
            let auto = select_video(VideoCodec::H264, HwEncPolicy::Auto, &full);
            assert!(
                auto.is_hardware(),
                "auto takes the GPU: {}",
                auto.describe()
            );

            let off = select_video(VideoCodec::H264, HwEncPolicy::Off, &full);
            assert_eq!(off.spec.element, "x264enc", "{}", off.describe());
            assert!(!off.is_hardware(), "off means software: {}", off.describe());
        }
        {
            let both = registry(&["vah264enc", "nvh264enc", "openh264enc"]);
            let forced = select_video(
                VideoCodec::H264,
                HwEncPolicy::Force(EncoderKind::NvCodec),
                &both,
            );
            assert_eq!(
                forced.spec.element,
                "nvh264enc",
                "forced family wins: {}",
                forced.describe()
            );

            let only_va = registry(&["vah264enc", "openh264enc"]);
            let forced = select_video(
                VideoCodec::H264,
                HwEncPolicy::Force(EncoderKind::NvCodec),
                &only_va,
            );
            assert_eq!(
                forced.spec.element,
                "openh264enc",
                "software before another vendor's hardware: {}",
                forced.describe(),
            );
            assert!(!forced.is_hardware());
        }
        {
            for codec in [
                VideoCodec::H264,
                VideoCodec::Vp8,
                VideoCodec::Vp9,
                VideoCodec::Av1,
            ] {
                let c = codec.candidates();
                assert!(!c.is_empty(), "{codec:?} has candidates");
                let first_sw = c.iter().position(|s| !s.kind.is_hardware());
                if let Some(i) = first_sw {
                    assert!(
                        c[i..].iter().all(|s| !s.kind.is_hardware()),
                        "{codec:?} lists hardware after software: {:?}",
                        c.iter().map(|s| s.element).collect::<Vec<_>>(),
                    );
                }
            }
        }
        {
            assert_eq!(HwEncPolicy::parse("auto"), HwEncPolicy::Auto);
            assert_eq!(HwEncPolicy::parse(""), HwEncPolicy::Auto);
            assert_eq!(HwEncPolicy::parse("off"), HwEncPolicy::Off);
            assert_eq!(HwEncPolicy::parse(" OFF "), HwEncPolicy::Off);
            assert_eq!(
                HwEncPolicy::parse("va"),
                HwEncPolicy::Force(EncoderKind::Va)
            );
            assert_eq!(
                HwEncPolicy::parse("nvenc"),
                HwEncPolicy::Force(EncoderKind::NvCodec)
            );
            assert_eq!(
                HwEncPolicy::parse("qsv"),
                HwEncPolicy::Force(EncoderKind::Qsv)
            );
            // A typo must not read as "off", which would silently disable the GPU.
            assert_eq!(HwEncPolicy::parse("vaapii"), HwEncPolicy::Auto);
        }
        {
            let both = select_audio(
                AudioCodec::Aac,
                HwEncPolicy::Auto,
                registry(&["fdkaacenc", "avenc_aac"]),
            );
            assert_eq!(
                both.spec.element, "fdkaacenc",
                "Fraunhofer is the better AAC encoder"
            );

            let libav = select_audio(AudioCodec::Aac, HwEncPolicy::Auto, registry(&["avenc_aac"]));
            assert_eq!(
                libav.spec.element, "avenc_aac",
                "and libav's is the fallback"
            );
            assert!(libav.confirmed);

            let opus = select_audio(AudioCodec::Opus, HwEncPolicy::Auto, registry(&["opusenc"]));
            assert_eq!(opus.spec.element, "opusenc");
        }
    }

    #[test]
    fn bitrate_units_and_size_predictions_follow_the_chosen_encoder() {
        {
            let target = 16_000_000u64; // 16 Mbit/s, the 1080p preset.

            let openh264 = H264.iter().find(|s| s.element == "openh264enc").unwrap();
            assert_eq!(
                openh264.bitrate_arg(target),
                "bitrate=16000000",
                "openh264enc counts in bits per second",
            );

            let x264 = H264.iter().find(|s| s.element == "x264enc").unwrap();
            assert_eq!(
                x264.bitrate_arg(target),
                "bitrate=16000",
                "x264enc counts in kilobits — the same number would ask it for 16 Gbit/s",
            );
            assert_eq!(x264.launch_args(target), "bitrate=16000 bframes=0");
            assert_eq!(openh264.launch_args(target), "bitrate=16000000");

            let va = H264.iter().find(|s| s.element == "vah264enc").unwrap();
            assert_eq!(
                va.bitrate_arg(target),
                "bitrate=16000",
                "VA-API counts in kilobits"
            );

            let vp8 = VP8.iter().find(|s| s.element == "vp8enc").unwrap();
            assert_eq!(
                vp8.bitrate_arg(target),
                "target-bitrate=16000000",
                "libvpx spells the property differently and counts in bits",
            );
        }
        {
            let va = H264.iter().find(|s| s.element == "vah264enc").unwrap();
            // 0 is the encoders' own "auto" and must survive as such.
            assert_eq!(va.bitrate_value(0), 0);
            // Anything genuinely requested must not become it.
            for bps in [1u64, 500, 999] {
                assert_eq!(
                    va.bitrate_value(bps),
                    1,
                    "{bps} bit/s must ask for the smallest real rate, not auto",
                );
            }
            assert_eq!(va.bitrate_value(8_000_000), 8_000);

            // The highest export preset is far below the cap, so real requests pass
            // through untouched.
            assert_eq!(va.bitrate_value(45_000_000), 45_000);
        }
        {
            let tracking = H264.iter().find(|s| s.element == "x264enc").unwrap();
            let untracked = H264.iter().find(|s| s.element == "openh264enc").unwrap();
            assert!(
                untracked.ignores_bitrate,
                "openh264enc is the measured exception"
            );
            assert!(
                !tracking.ignores_bitrate,
                "nothing has been observed about x264enc"
            );

            let sel = |spec: &'static EncoderSpec, confirmed: bool| Selection {
                spec,
                confirmed,
                passed_over: Vec::new(),
            };
            assert!(
                sel(tracking, true).bitrate_is_predictive(),
                "a confirmed rate-controlling encoder can be quoted a size",
            );
            assert!(
                !sel(untracked, true).bitrate_is_predictive(),
                "an encoder measured to ignore its bitrate must not be quoted a size",
            );
            // Unconfirmed is "nothing observed", never "probably fine".
            assert!(
                !sel(tracking, false).bitrate_is_predictive(),
                "an unconfirmed encoder has not been observed, so nothing follows from it",
            );
        }
        {
            let empty = EncodeReport::default();
            assert!(!empty.is_hardware());
            assert!(
                empty.describe().contains("no video encoder selected"),
                "{}",
                empty.describe(),
            );
        }
    }

    #[cfg(feature = "encode")]
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn installed_encoder_elements_match_the_bitrate_table() {
        // The kilobit half of the units table, proved by encoding. `vaav1enc` is a real
        // VA-API encoder here and shares `GstVaBaseEnc`'s kilobit `bitrate` property with
        // `vah264enc`: two rates 20x apart and the resulting file sizes show the unit,
        // where bits would turn 8000 into a few kilobytes. Skipped only where the element
        // is absent (no VA-API device).
        fn a_va_encoder_reads_its_bitrate_as_kilobits() {
            use gstreamer::prelude::*;

            gstreamer::init().expect("gstreamer init");
            let spec = AV1.iter().find(|s| s.element == "vaav1enc").unwrap();
            assert_eq!(spec.bitrate_units, BitrateUnits::KilobitsPerSecond);
            if gstreamer::ElementFactory::find(spec.element).is_none() {
                eprintln!("encode: no vaav1enc on this machine; kilobit encode proof not run");
                return;
            }

            let dir = std::env::temp_dir().join(format!("tilo-media-kbps-{}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("scratch dir");
            let seconds = 5u64;
            let mut sizes = Vec::new();
            // The two requests in bits per second, the units the rest of the codebase
            // speaks; `bitrate_value` converts.
            for bps in [400_000u64, 8_000_000] {
                let out = dir.join(format!("{bps}.mkv"));
                let desc = format!(
                    "videotestsrc pattern=ball num-buffers={frames} \
                     ! video/x-raw,width=640,height=360,framerate=30/1 \
                     ! videoconvert ! {el} {arg} ! av1parse ! matroskamux ! filesink name=probe_output",
                    frames = seconds * 30,
                    el = spec.element,
                    arg = spec.bitrate_arg(bps),
                );
                let pipeline = match gstreamer::parse::launch(&desc) {
                    Ok(p) => p.downcast::<gstreamer::Pipeline>().expect("a pipeline"),
                    Err(e) => {
                        eprintln!("encode: vaav1enc pipeline would not build ({e}); proof not run");
                        return;
                    }
                };
                pipeline
                    .by_name("probe_output")
                    .expect("output sink")
                    .set_property("location", out.to_str().expect("UTF-8 scratch path"));
                if pipeline.set_state(gstreamer::State::Playing).is_err() {
                    eprintln!("encode: vaav1enc would not start; proof not run");
                    let _ = pipeline.set_state(gstreamer::State::Null);
                    return;
                }
                let bus = pipeline.bus().expect("bus");
                let msg = bus.timed_pop_filtered(
                    gstreamer::ClockTime::from_seconds(120),
                    &[gstreamer::MessageType::Eos, gstreamer::MessageType::Error],
                );
                let _ = pipeline.set_state(gstreamer::State::Null);
                match msg {
                    Some(m) if m.type_() == gstreamer::MessageType::Eos => {}
                    other => {
                        eprintln!("encode: vaav1enc did not reach EOS ({other:?}); proof not run");
                        return;
                    }
                }
                sizes.push(std::fs::metadata(&out).map_or(0, |m| m.len()));
            }

            let (low, high) = (sizes[0], sizes[1]);
            std::fs::remove_dir_all(&dir).ok();

            let as_if_bits = 8_000u64 * seconds / 8;
            assert!(
                high > as_if_bits * 20,
                "5s at the 8 Mbit/s setting produced {high} bytes; if the property were read \
                 as bits per second it would be near {as_if_bits}",
            );
            // 20x the request, near enough 20x the bytes: a wide band, because rate
            // control is the driver's business and this proves the unit.
            let ratio = high as f64 / low.max(1) as f64;
            assert!(
                (5.0..40.0).contains(&ratio),
                "20x the bitrate gave {ratio:.1}x the bytes ({low} -> {high}); the property \
                 is not being read as a rate at all",
            );
            eprintln!(
                "encode: vaav1enc kilobits confirmed — 400 kbit/s -> {low} bytes, \
                 8 Mbit/s -> {high} bytes ({ratio:.1}x)",
            );
        }
        // The table's property names and units, checked against the elements this machine
        // has. A wrong property name kills the export; a wrong unit is worse, because
        // nothing rejects it and the file is encoded at 1000x or 1/1000x the quoted rate.
        // Fails when it can check nothing at all, because a check that passes without
        // looking is how a wrong table survives.
        {
            use gstreamer::prelude::*;

            gstreamer::init().expect("gstreamer init");

            let all: Vec<&EncoderSpec> = H264
                .iter()
                .chain(VP8)
                .chain(VP9)
                .chain(AV1)
                .chain(AAC)
                .chain(OPUS)
                .collect();

            let mut checked_property = Vec::new();
            let mut checked_units = Vec::new();
            for spec in &all {
                let Some(factory) = gstreamer::ElementFactory::find(spec.element) else {
                    continue; // Not installed here; nothing to check.
                };
                let Ok(element) = factory.create().build() else {
                    continue; // Present but not instantiable (no device, no licence).
                };
                let pspec = element
                    .find_property(spec.bitrate_property)
                    .unwrap_or_else(|| {
                        panic!(
                            "{} has no property {:?} — the export pipeline would fail to build",
                            spec.element, spec.bitrate_property,
                        )
                    });
                checked_property.push(spec.element);

                // Property blurbs state the unit in words. Classify only the unambiguous
                // ones; an unclassifiable blurb is left alone rather than guessed at.
                let blurb = pspec.blurb().unwrap_or_default().to_ascii_lowercase();
                let says_kilobits = blurb.contains("kbps") || blurb.contains("kbit");
                let says_bits = !says_kilobits
                    && (blurb.contains("bits per second")
                        || blurb.contains("bits/sec")
                        || blurb.contains("bits/s")
                        || blurb.contains("in bps"));
                if !says_kilobits && !says_bits {
                    continue;
                }
                let declared = spec.bitrate_units;
                let observed = if says_kilobits {
                    BitrateUnits::KilobitsPerSecond
                } else {
                    BitrateUnits::BitsPerSecond
                };
                assert_eq!(
                    declared, observed,
                    "{}.{} is documented as {blurb:?}, but the table declares {declared:?} — \
                     a 1000x bitrate error the encoder would accept in silence",
                    spec.element, spec.bitrate_property,
                );
                checked_units.push(spec.element);
            }

            assert!(
                !checked_property.is_empty(),
                "this test verified nothing: none of {:?} is installed, so it cannot \
                 observe its subject and must not report success",
                all.iter().map(|s| s.element).collect::<Vec<_>>(),
            );
            assert!(
                !checked_units.is_empty(),
                "the bitrate property exists on {checked_property:?} but not one of them \
                 documented its unit, so the units column went unverified",
            );
            eprintln!(
                "encode: bitrate property verified on {checked_property:?}; \
                 units verified on {checked_units:?}",
            );
        }
        a_va_encoder_reads_its_bitrate_as_kilobits();
        // The [`EncoderSpec::ignores_bitrate`] measurements, re-run: a claim in a table
        // decays, and every marked encoder installed here is handed incompressible content
        // at a low target. Both matter: a rate controller holds a low target whatever the
        // content costs, while a quality chooser overshoots, and on easy content the two
        // are indistinguishable. Fails when a marked encoder is installed and no longer
        // overshoots; a missing one is a property of the machine.
        {
            use gstreamer::prelude::*;

            gstreamer::init().expect("gstreamer init");

            let mux = |element: &str| match element {
                "openh264enc" => Some("h264parse ! mp4mux"),
                "vp8enc" | "vp9enc" => Some("webmmux"),
                _ => None,
            };
            let marked: Vec<&EncoderSpec> = H264
                .iter()
                .chain(VP8)
                .chain(VP9)
                .chain(AV1)
                .filter(|s| s.ignores_bitrate)
                .collect();
            assert!(!marked.is_empty(), "the table marks at least one encoder");

            let dir = std::env::temp_dir().join(format!("tilo-media-rate-{}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("scratch dir");
            let seconds = 1u64;
            let asked_bps = 1_000_000u64;
            let mut checked = Vec::new();
            let mut absent = Vec::new();
            for spec in &marked {
                if gstreamer::ElementFactory::find(spec.element).is_none() {
                    absent.push(spec.element);
                    continue;
                }
                let Some(mux) = mux(spec.element) else {
                    absent.push(spec.element);
                    continue;
                };
                let out = dir.join(format!("{}.bin", spec.element));
                // `snow` is white noise: nothing to predict, nothing to compress, so the
                // encoder cannot meet a low target by getting lucky.
                let desc = format!(
                    "videotestsrc pattern=snow num-buffers={frames} \
                     ! video/x-raw,width=640,height=360,framerate=30/1 \
                     ! videoconvert ! {el} {arg} ! {mux} ! filesink name=probe_output",
                    frames = seconds * 30,
                    el = spec.element,
                    arg = spec.bitrate_arg(asked_bps),
                );
                let Ok(pipeline) = gstreamer::parse::launch(&desc) else {
                    panic!(
                        "{} is installed but its pipeline would not build: {desc}",
                        spec.element
                    );
                };
                let pipeline = pipeline
                    .downcast::<gstreamer::Pipeline>()
                    .expect("a pipeline");
                pipeline
                    .by_name("probe_output")
                    .expect("output sink")
                    .set_property("location", out.to_str().expect("UTF-8 scratch path"));
                pipeline
                    .set_state(gstreamer::State::Playing)
                    .unwrap_or_else(|e| {
                        panic!("{} is installed, so it must run: {e}", spec.element)
                    });
                let bus = pipeline.bus().expect("bus");
                let msg = bus.timed_pop_filtered(
                    gstreamer::ClockTime::from_seconds(300),
                    &[gstreamer::MessageType::Eos, gstreamer::MessageType::Error],
                );
                let _ = pipeline.set_state(gstreamer::State::Null);
                assert!(
                    matches!(&msg, Some(m) if m.type_() == gstreamer::MessageType::Eos),
                    "the {} encode did not finish: {msg:?}",
                    spec.element,
                );
                let bytes = std::fs::metadata(&out).map_or(0, |m| m.len());
                assert!(bytes > 0, "{} wrote nothing", spec.element);
                let wrote_bps = bytes * 8 / seconds;
                let over = wrote_bps as f64 / asked_bps as f64;
                eprintln!(
                    "encode: {} asked {:.2} Mbit/s, wrote {:.2} Mbit/s ({over:.1}x)",
                    spec.element,
                    asked_bps as f64 / 1e6,
                    wrote_bps as f64 / 1e6,
                );
                assert!(
                    over > 2.0,
                    "{} kept within {over:.1}x of a {} bit/s target on noise, so it does \
                     appear to rate-control after all. The table records it as ignoring the \
                     setting, which means a size estimate is being withheld that could now be \
                     given — clear `ignores_bitrate` on it.",
                    spec.element,
                    asked_bps,
                );
                checked.push(spec.element);
            }
            std::fs::remove_dir_all(&dir).ok();

            assert!(
                !checked.is_empty(),
                "this test verified nothing: none of {marked:?} could be exercised here \
                 (absent: {absent:?}), so it cannot observe its subject and must not \
                 report success",
            );
            eprintln!("encode: overshoot confirmed on {checked:?}; not exercised: {absent:?}");
        }
    }
}
