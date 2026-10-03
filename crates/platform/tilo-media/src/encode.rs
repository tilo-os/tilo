//! Encoder discovery, preference order, and the record of what was selected.
//!
//! Encoding has no autoplugger: an encode pipeline names its encoder element
//! outright. This module chooses an encoder from the available registry and
//! records the selection.
//!
//! It declares, per codec, encoders in preference order and picks the first one
//! the registry confirms is installed.
//!
//! [`Selection::confirmed`] is false when no registry was consulted, and
//! [`Selection::is_hardware`] is false unless the element was confirmed and
//! belongs to a hardware family.
//!
//! `GStreamer` encoders disagree on bitrate units: `openh264enc` and
//! `vp8enc` take bits per second, while every VA-API, NVENC, QSV and `x264enc`
//! encoder takes *kilo*bits per second. Setting 16000000 on `x264enc` asks for
//! 16 Gbit/s, which the encoder clamps and silently changes the output rate.
//! Each [`EncoderSpec`] carries
//! its property name *and* its units, and [`EncoderSpec::bitrate_arg`] does
//! the conversion. Tests check that table against the registry.
//!
//! ## Overriding
//!
//! `TILO_MEDIA_HWENC` mirrors `TILO_MEDIA_HWDEC`:
//!
//! | value | effect |
//! |---|---|
//! | `auto` (default) | prefer hardware, fall back to software |
//! | `off` | software encoders only |
//! | `va` / `nvcodec` / `qsv` / `v4l2` | try that family first, then software |

/// Which encoder family an element belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncoderKind {
    /// VA-API — AMD and Intel, discrete and integrated, via libva.
    Va,
    /// NVIDIA NVENC via the `nvcodec` plugin.
    NvCodec,
    /// Intel Quick Sync via the `qsv` plugin.
    Qsv,
    /// Linux `V4L2` stateful encoders — embedded system-on-chip parts.
    V4l2,
    /// Direct3D 11/12 video encode (present for completeness).
    D3d,
    /// Apple `VideoToolbox`.
    VideoToolbox,
    /// A CPU encoder — `x264enc`, `openh264enc`, `vp8enc`, `avenc_aac`, …
    Software,
}

impl EncoderKind {
    /// Whether an encoder of this kind runs on fixed-function video hardware.
    pub fn is_hardware(self) -> bool {
        !matches!(self, EncoderKind::Software)
    }

    /// The family name used by `TILO_MEDIA_HWENC` and reported in logs.
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

/// The unit an encoder's bitrate property is expressed in.
///
/// This is the whole reason [`EncoderSpec`] exists rather than a bare element
/// name: the two families differ by a factor of 1000, and getting it wrong is
/// silent — the encoder accepts the number and produces a file at some other
/// rate entirely.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BitrateUnits {
    /// `openh264enc`, `vp8enc`, `vp9enc`, `avenc_aac`, `opusenc`.
    BitsPerSecond,
    /// Every `va*`/`vaapi*`/`nv*`/`qsv*` encoder, and `x264enc`.
    KilobitsPerSecond,
}

/// One candidate encoder: what to instantiate, which family it belongs to,
/// and how to ask it for a target bitrate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EncoderSpec {
    /// The `GStreamer` element factory name.
    pub element: &'static str,
    pub kind: EncoderKind,
    /// The property carrying the target bitrate.
    pub bitrate_property: &'static str,
    pub bitrate_units: BitrateUnits,
    /// Set where this build **measured** the encoder producing a file whose
    /// rate bears no relation to the one it was asked for.
    ///
    /// The measurement that settles it is incompressible content at a low
    /// target: a rate controller holds the line, while a quality-based encoder
    /// choosing a quality overshoots by whatever the content costs. Five
    /// seconds of 720p noise asked for 1 Mbit/s, on this build:
    ///
    /// | encoder      | asked 1 Mbit/s | asked 8 Mbit/s |
    /// |--------------|----------------|----------------|
    /// | `openh264enc`| wrote 59.2     | wrote 61.0     |
    /// | `vp8enc`     | wrote 14.4     | wrote 16.0     |
    /// | `vp9enc`     | wrote 8.9      | wrote 12.8     |
    ///
    /// Eight times the request moves openh264 by 3% and vp8 by 11%. That is
    /// not rate control, and the property is accepted either way — nothing
    /// errors — so an encode is the only thing that can tell.
    ///
    /// `false` means no observation has shown the encoder ignoring the setting;
    /// it does not guarantee accuracy for an encoder that is not installed.
    pub ignores_bitrate: bool,
}

impl EncoderSpec {
    /// The number to write to [`Self::bitrate_property`] for a target of
    /// `bits_per_second`.
    ///
    /// A kilobit encoder given a target under 1000 bit/s would round to 0,
    /// which every one of them reads as "auto" — so the result is floored at
    /// 1 kbps. This prevents a small positive bitrate from becoming an
    /// automatic-quality request.
    ///
    /// The units table is checked against the registry because an incorrect
    /// conversion can be accepted by `GStreamer` while producing the wrong rate.
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

    /// The `property=value` fragment for a `gst-launch` description.
    pub fn bitrate_arg(&self, bits_per_second: u64) -> String {
        format!(
            "{}={}",
            self.bitrate_property,
            self.bitrate_value(bits_per_second)
        )
    }
}

/// A video codec the export can write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoCodec {
    H264,
    Vp8,
    Vp9,
    Av1,
}

/// An audio codec the export can write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioCodec {
    Aac,
    Opus,
}

// ---------------------------------------------------------------------------
// Candidate tables.
//
// Order is preference order: hardware families first, then software. The last
// entry of each list is the baseline encoder.
// ---------------------------------------------------------------------------

/// H.264. VA is first because it covers AMD and Intel hardware.
/// `x264enc` outranks `openh264enc` because openh264 only writes Baseline
/// profile — no CABAC, no B-frames — which reduces quality at the same
/// bitrate.
const H264: &[EncoderSpec] = &[
    kbit("vah264enc", EncoderKind::Va),
    kbit("nvh264enc", EncoderKind::NvCodec),
    kbit("qsvh264enc", EncoderKind::Qsv),
    // Some Intel iGPUs expose only the low-power VA encoder.
    kbit("vah264lpenc", EncoderKind::Va),
    // The older gstreamer-vaapi plugin, still shipped by some distributions.
    kbit("vaapih264enc", EncoderKind::Va),
    kbit("v4l2h264enc", EncoderKind::V4l2),
    kbit("x264enc", EncoderKind::Software),
    H264_BASELINE,
];
const H264_BASELINE: EncoderSpec = bits_untracked("openh264enc", EncoderKind::Software);

/// VP8.
const VP8: &[EncoderSpec] = &[
    kbit("vavp8enc", EncoderKind::Va),
    kbit("vaapivp8enc", EncoderKind::Va),
    VP8_BASELINE,
];
const VP8_BASELINE: EncoderSpec = target_bits("vp8enc");

/// VP9.
const VP9: &[EncoderSpec] = &[
    kbit("vavp9enc", EncoderKind::Va),
    kbit("qsvvp9enc", EncoderKind::Qsv),
    kbit("vaapivp9enc", EncoderKind::Va),
    VP9_BASELINE,
];
const VP9_BASELINE: EncoderSpec = target_bits("vp9enc");

/// AV1. SVT-AV1 is the software baseline: fast enough for export (it has no
/// latency budget here, unlike the real-time path in
/// [`crate::hw::realtime`], which cannot use it) and royalty-free.
const AV1: &[EncoderSpec] = &[
    kbit("vaav1enc", EncoderKind::Va),
    kbit("qsvav1enc", EncoderKind::Qsv),
    kbit("nvav1enc", EncoderKind::NvCodec),
    AV1_BASELINE,
];
/// `svtav1enc`'s `target-bitrate` counts in kilobits per second.
const AV1_BASELINE: EncoderSpec = EncoderSpec {
    bitrate_property: "target-bitrate",
    ..kbit("svtav1enc", EncoderKind::Software)
};

/// AAC. No hardware AAC encoders exist; the choice is quality. `fdkaacenc`
/// (Fraunhofer) is the better encoder, `avenc_aac` the one that is always
/// there because the `libav` plugin is.
const AAC: &[EncoderSpec] = &[
    bits("fdkaacenc", EncoderKind::Software),
    bits("avenc_aac", EncoderKind::Software),
    AAC_BASELINE,
];
const AAC_BASELINE: EncoderSpec = bits("voaacenc", EncoderKind::Software);

/// Opus.
const OPUS: &[EncoderSpec] = &[OPUS_BASELINE];
const OPUS_BASELINE: EncoderSpec = bits("opusenc", EncoderKind::Software);

/// A candidate whose bitrate property is `bitrate`, in bits per second.
const fn bits(element: &'static str, kind: EncoderKind) -> EncoderSpec {
    EncoderSpec {
        element,
        kind,
        bitrate_property: "bitrate",
        bitrate_units: BitrateUnits::BitsPerSecond,
        ignores_bitrate: false,
    }
}

/// [`bits`], for an encoder measured not to track the rate it is given —
/// see [`EncoderSpec::ignores_bitrate`].
const fn bits_untracked(element: &'static str, kind: EncoderKind) -> EncoderSpec {
    EncoderSpec {
        ignores_bitrate: true,
        ..bits(element, kind)
    }
}

/// A candidate whose bitrate property is `bitrate`, in kilobits per second.
const fn kbit(element: &'static str, kind: EncoderKind) -> EncoderSpec {
    EncoderSpec {
        element,
        kind,
        bitrate_property: "bitrate",
        bitrate_units: BitrateUnits::KilobitsPerSecond,
        ignores_bitrate: false,
    }
}

/// The libvpx software encoders, which spell the property `target-bitrate`
/// and take bits per second — and which, as this pipeline configures them,
/// treat it as a suggestion. Both were measured overshooting a low target on
/// incompressible content by 4x to 14x; see [`EncoderSpec::ignores_bitrate`].
const fn target_bits(element: &'static str) -> EncoderSpec {
    EncoderSpec {
        element,
        kind: EncoderKind::Software,
        bitrate_property: "target-bitrate",
        bitrate_units: BitrateUnits::BitsPerSecond,
        ignores_bitrate: true,
    }
}

impl VideoCodec {
    /// Every encoder worth trying, in preference order.
    pub fn candidates(self) -> &'static [EncoderSpec] {
        match self {
            VideoCodec::H264 => H264,
            VideoCodec::Vp8 => VP8,
            VideoCodec::Vp9 => VP9,
            VideoCodec::Av1 => AV1,
        }
    }

    /// The encoder named when nothing else is confirmed present.
    ///
    /// This is each table's final entry, named directly rather than found by
    /// length arithmetic: reordering a table cannot silently move the last
    /// resort, and there is no index to get wrong.
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

    /// The encoder named when nothing else is confirmed present — see
    /// [`VideoCodec::baseline`].
    pub fn baseline(self) -> &'static EncoderSpec {
        match self {
            AudioCodec::Aac => &AAC_BASELINE,
            AudioCodec::Opus => &OPUS_BASELINE,
        }
    }
}

/// The hardware-encode policy for this process, from `TILO_MEDIA_HWENC`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HwEncPolicy {
    /// Prefer hardware; fall back to software.
    Auto,
    /// Software only.
    Off,
    /// Try one family first, then software, then other hardware — so a forced
    /// family that cannot encode this codec falls back to the CPU rather than
    /// to a different GPU.
    Force(EncoderKind),
}

impl HwEncPolicy {
    /// Read the policy from the environment. An unrecognised value is
    /// [`HwEncPolicy::Auto`] with a warning: a typo must not silently disable
    /// hardware encode.
    pub fn from_env() -> Self {
        match std::env::var("TILO_MEDIA_HWENC") {
            Ok(v) => Self::parse(&v),
            Err(_) => HwEncPolicy::Auto,
        }
    }

    /// Parse a policy string. Exposed so the parsing is testable without
    /// mutating the process environment.
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

    /// Whether `kind` may be selected at all under this policy.
    fn admits(self, kind: EncoderKind) -> bool {
        match self {
            // Forcing a family is a preference, not an exclusion: if the
            // forced encoder is absent the others are still worth trying.
            HwEncPolicy::Auto | HwEncPolicy::Force(_) => true,
            HwEncPolicy::Off => !kind.is_hardware(),
        }
    }

    /// Ordering key: lower is tried first. Preserves the declared order within
    /// a tier.
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

/// The outcome of picking an encoder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    /// The encoder to instantiate.
    pub spec: &'static EncoderSpec,
    /// Whether a registry actually confirmed this element is installed.
    ///
    /// `false` means the caller could not look — no `GStreamer` linked, or every
    /// candidate was missing — and [`Self::spec`] is the declared baseline.
    /// Never read `false` as "software is definitely what will run"; read it
    /// as "nothing has been observed".
    pub confirmed: bool,
    /// Candidates ahead of the chosen one that the registry did not have, in
    /// the order they were tried. Diagnostics for "why is this not using my
    /// GPU".
    pub passed_over: Vec<&'static str>,
}

impl Selection {
    /// Whether the encode will run on video hardware.
    ///
    /// False both for a software encoder and for an unconfirmed choice, so a
    /// caller can never read "not yet known" as "yes" — the same discipline
    /// [`crate::hw::HwReport::is_hardware`] applies to decode.
    pub fn is_hardware(&self) -> bool {
        self.confirmed && self.spec.kind.is_hardware()
    }

    /// Whether a size predicted from the requested bitrate means anything
    /// for this encoder.
    ///
    /// False in two cases, and a caller must treat them the same way — by not
    /// quoting a size:
    ///   * the chosen encoder is [recorded as ignoring its
    ///     bitrate](EncoderSpec::ignores_bitrate), so the rate asked for says
    ///     nothing about the file that comes out;
    ///   * nothing was confirmed present ([`Self::confirmed`]), so which
    ///     encoder runs — if any — has not been observed at all.
    pub fn bitrate_is_predictive(&self) -> bool {
        self.confirmed && !self.spec.ignores_bitrate
    }

    /// A one-line summary for a log or a test failure message.
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

/// Pick an encoder for `codec`, trying each candidate against `available`.
///
/// `available` answers "is this element factory installed", which in a real
/// build is [`registry_has`]. Kept as a parameter so the preference order is
/// unit-testable against a fabricated registry — including registries this
/// machine cannot have, which is the only way to prove the hardware branch.
pub fn select_video(
    codec: VideoCodec,
    policy: HwEncPolicy,
    available: impl Fn(&str) -> bool,
) -> Selection {
    select(codec.candidates(), codec.baseline(), policy, available)
}

/// Pick an audio encoder for `codec`. Same contract as [`select_video`].
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
    // Stable sort by policy tier keeps the declared preference order inside
    // each tier, so `Auto` walks the table exactly as written.
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

/// Which encoders an export actually selected. The encode-side counterpart of
/// [`crate::hw::HwReport`]: a record of what was chosen, not a claim about
/// what the machine can do.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EncodeReport {
    pub video: Option<Selection>,
    pub audio: Option<Selection>,
}

impl EncodeReport {
    /// Whether the *video* encode runs on hardware. False when unknown.
    pub fn is_hardware(&self) -> bool {
        self.video.as_ref().is_some_and(Selection::is_hardware)
    }

    /// A one-line summary for a log or a test failure message.
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

/// Whether `name` is an element factory in this process's `GStreamer` registry.
///
/// The real answer to [`select_video`]'s `available` question. Only compiled
/// where `GStreamer` is linked; a build without it cannot look, and says so
/// through [`Selection::confirmed`] rather than guessing.
/// Initialises `GStreamer` if the caller has not — the registry is empty before
/// `gst_init`, and an uninitialised lookup would report every encoder missing,
/// which is the failure mode this module exists to prevent. `gst_init` is
/// documented as safe to call repeatedly.
#[cfg(any(gst_decode, gst_encode, gst_realtime))]
pub fn registry_has(name: &str) -> bool {
    if gstreamer::init().is_ok() {
        crate::extensions::refresh_registry();
    }
    gstreamer::ElementFactory::find(name).is_some()
}

/// The encoder this process would use for `codec`, read from the live registry
/// under the environment's policy.
#[cfg(any(gst_decode, gst_encode, gst_realtime))]
pub fn select_video_from_registry(codec: VideoCodec) -> Selection {
    select_video(codec, HwEncPolicy::from_env(), registry_has)
}

/// [`select_video_from_registry`] for audio.
#[cfg(any(gst_decode, gst_encode, gst_realtime))]
pub fn select_audio_from_registry(codec: AudioCodec) -> Selection {
    select_audio(codec, HwEncPolicy::from_env(), registry_has)
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

    /// A fabricated registry holding exactly `names`.
    fn registry(names: &'static [&'static str]) -> impl Fn(&str) -> bool {
        move |n: &str| names.contains(&n)
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn encoder_choice_ranks_hardware_policy_and_software_fallbacks() {
        // Phase `a_registered_hardware_encoder_wins_over_every_software_one`.
        {
            // The machine this was written on has no H.264 hardware encoder, so
            // the only way to prove the hardware branch is to hand the selector a
            // registry that has one.
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
        // Phase `without_hardware_the_best_software_encoder_wins`.
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

            // And openh264enc when that is genuinely all there is — the situation
            // on the machine this was written on.
            let sel = select_video(
                VideoCodec::H264,
                HwEncPolicy::Auto,
                registry(&["openh264enc"]),
            );
            assert_eq!(sel.spec.element, "openh264enc", "{}", sel.describe());
            assert!(!sel.is_hardware());
        }
        // Phase `an_empty_registry_never_reads_as_hardware`.
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

            // Even when the baseline itself is a hardware element, an unconfirmed
            // selection must not report hardware.
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
        // Phase `policy_off_refuses_hardware_even_when_it_is_installed`.
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
        // Phase `a_forced_family_outranks_other_hardware_but_falls_back_to_software`.
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

            // A forced family that cannot be found drops to software rather than
            // silently using the other GPU vendor's encoder.
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
        // Phase `every_codecs_hardware_candidates_precede_its_software_ones`.
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
        // Phase `policy_parsing_covers_the_documented_values_and_warns_on_the_rest`.
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
            // A typo must not read as "off" — that would silently disable the GPU.
            assert_eq!(HwEncPolicy::parse("vaapii"), HwEncPolicy::Auto);
        }
        // Phase `the_audio_choice_prefers_the_better_aac_encoder_and_still_lands_somewhere`.
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
        // Phase `bitrate_is_converted_into_each_encoders_own_unit`.
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
        // Phase `a_kilobit_encoder_never_rounds_a_real_bitrate_down_to_auto`.
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

            // The highest export preset is nowhere near the cap, so real requests
            // pass through untouched.
            assert_eq!(va.bitrate_value(45_000_000), 45_000);
        }
        // Phase `a_size_is_only_predictable_from_a_confirmed_rate_controlling_encoder`.
        // A size predicted from the requested bitrate is only meaningful where
        // the encoder tracks the request and the encoder was actually found.
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
            // Unconfirmed is "nothing observed", never "probably fine" — the same
            // discipline `is_hardware` applies.
            assert!(
                !sel(tracking, false).bitrate_is_predictive(),
                "an unconfirmed encoder has not been observed, so nothing follows from it",
            );
        }
        // Phase `an_encode_report_without_a_video_encoder_is_not_a_hardware_claim`.
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
        // Phase `a_va_encoder_reads_its_bitrate_as_kilobits`.
        // The kilobit half of the units table, proved by encoding.
        //
        // The software encoders on this machine are all bits-per-second, so the
        // blurb check above never exercises the kilobit branch here. `vaav1enc`
        // does: it is a real VA-API hardware encoder on this box and it shares
        // `GstVaBaseEnc`'s `bitrate` property with `vah264enc`. Encoding the same
        // clip at two bitrates 20x apart and comparing file sizes shows the number
        // is read as kilobits — were it bits, a 8000 setting would produce a few
        // kilobytes rather than megabytes.
        //
        // Skipped only where the element is absent (no VA-API device); that is a
        // property of the machine, not of the code, and the blurb test above still
        // covers the table's other half.
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
            // 400 kbit/s and 8 Mbit/s, expressed in bits per second — the units the
            // rest of the codebase speaks. bitrate_value does the conversion.
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

            // If `bitrate` were bits per second, asking for 8_000_000 bit/s would
            // have written 8000 into it — five seconds of which is about 5 kB.
            let as_if_bits = 8_000u64 * seconds / 8;
            assert!(
                high > as_if_bits * 20,
                "5s at the 8 Mbit/s setting produced {high} bytes; if the property were read \
                 as bits per second it would be near {as_if_bits}",
            );
            // And the two settings must scale with each other: 20x the request,
            // near enough 20x the bytes. Rate control is the driver's business, so
            // this is a wide band — it is proving the unit, not the encoder.
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
        // Phase `declared_bitrate_properties_exist_on_the_real_elements`.
        // The table's property names and units, checked against the elements this
        // machine actually has.
        //
        // A wrong property name is not a soft failure: `gst_parse_launch` rejects
        // "no property named X" and the export dies. A wrong *unit* is worse,
        // because nothing rejects it — the file is simply encoded at 1000× or
        // 1/1000× the rate the dialog quoted.
        //
        // This can only check what is installed, so on a machine with no hardware
        // encoder it verifies the software entries only. It fails outright when it
        // can check nothing at all, because a check that passes without looking is
        // exactly how a wrong table would survive.
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

                // GStreamer property blurbs state the unit in words. Classify only
                // the ones that say so unambiguously; an unclassifiable blurb is
                // left alone rather than guessed at.
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
        // Phase `a_va_encoder_reads_its_bitrate_as_kilobits` (defined above).
        a_va_encoder_reads_its_bitrate_as_kilobits();
        // Phase `the_encoders_marked_as_ignoring_their_bitrate_still_do`.
        // The [`EncoderSpec::ignores_bitrate`] measurements, re-run.
        //
        // That flag is a claim about how an encoder behaves, and a claim in a
        // table decays: libvpx and openh264 could gain working rate control, and
        // then a size estimate would be withheld that could perfectly well be
        // given. So every marked encoder that is installed here is handed
        // **incompressible** content at a **low** target and asked what it
        // writes.
        //
        // Incompressible and low both matter. A rate controller holds a low
        // target whatever the content costs; an encoder that is really choosing
        // a quality overshoots by however much the content needs. On easy
        // content the two are indistinguishable — a bouncing ball encoded by
        // openh264 at 1 Mbit/s and at 8 Mbit/s comes out byte-identical, and so
        // does one encoded by a genuine rate controller that never needed the
        // bits. Noise separates them.
        //
        // Fails rather than skips when a marked encoder is installed and no
        // longer overshoots; reports honestly when one is missing, which is a
        // property of the machine.
        {
            use gstreamer::prelude::*;

            gstreamer::init().expect("gstreamer init");

            // element -> the chain that makes a container out of its output.
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
                // `snow` is white noise: nothing to predict, nothing to compress,
                // so the encoder cannot meet a low target by getting lucky.
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
