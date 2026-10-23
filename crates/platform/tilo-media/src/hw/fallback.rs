//! Why a codec ran on the CPU, said once per process.
//!
//! Software decode or encode is a legitimate outcome — a codec the chip has no
//! block for, a stream larger than the block takes, a user who turned
//! hardware off — but it is never a silent one. Each path that ends in
//! software asks [`explain`] for the reason and hands it to [`report_once`],
//! which logs one warning per (direction, codec, reason) and stays quiet for
//! every later stream that falls back for the same reason.

use std::collections::BTreeSet;
use std::sync::Mutex;

use super::v4l2::{self, V4l2Device, V4l2Role};

/// Which way the codec runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Direction {
    Decode,
    Encode,
}

impl Direction {
    fn as_str(self) -> &'static str {
        match self {
            Direction::Decode => "decode",
            Direction::Encode => "encode",
        }
    }
}

/// The reason a stream did not reach video hardware.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FallbackReason {
    /// `TILO_MEDIA_HWDEC` / `TILO_MEDIA_HWENC` asked for software.
    PolicyOff,
    /// The stream is larger than the kernel's codec takes.
    ExceedsHardware {
        size: (u32, u32),
        limit: (u32, u32),
        device: String,
    },
    /// A hardware element was tried and refused this stream.
    HardwareRefused { element: String, detail: String },
    /// No hardware element for this codec is installed. `devices` names the
    /// V4L2 codec nodes that do exist, so "this chip's decoder has no VP9" is
    /// distinguishable from "this machine has no video hardware at all".
    NoHardware { devices: Vec<String> },
    /// Hardware for the codec is installed but a forced family outranked it.
    NotChosen { installed: Vec<String> },
}

impl FallbackReason {
    /// The variant, as the de-duplication key: two 4K streams refused by the
    /// same decoder are one reason, not two.
    fn key(&self) -> &'static str {
        match self {
            FallbackReason::PolicyOff => "policy",
            FallbackReason::ExceedsHardware { .. } => "size",
            FallbackReason::HardwareRefused { .. } => "refused",
            FallbackReason::NoHardware { .. } => "none",
            FallbackReason::NotChosen { .. } => "outranked",
        }
    }

    /// One sentence for the log and for an error shown to the user.
    pub fn describe(&self, direction: Direction, media_type: &str) -> String {
        let codec = v4l2::codec_label(media_type);
        let dir = direction.as_str();
        let size = match self {
            FallbackReason::ExceedsHardware { size: (w, h), .. } => format!(" {w}x{h}"),
            _ => String::new(),
        };
        format!(
            "{codec}{size} {dir} runs in software: {}",
            self.why(direction, media_type)
        )
    }

    /// The reason alone, without the "runs in software" framing.
    pub fn why(&self, direction: Direction, media_type: &str) -> String {
        let codec = v4l2::codec_label(media_type);
        let dir = direction.as_str();
        match self {
            FallbackReason::PolicyOff => "hardware is turned off by policy".to_string(),
            FallbackReason::ExceedsHardware {
                limit: (lw, lh),
                device,
                ..
            } => format!("{device} handles {codec} only up to {lw}x{lh}"),
            FallbackReason::HardwareRefused { element, detail } => {
                format!("{element} refused the stream ({detail})")
            }
            FallbackReason::NoHardware { devices } if devices.is_empty() => {
                format!("no hardware {codec} {dir}r on this device")
            }
            FallbackReason::NoHardware { devices } => format!(
                "no hardware {codec} {dir}r on this device (video hardware present: {})",
                devices.join("; ")
            ),
            FallbackReason::NotChosen { installed } => format!(
                "the forced hardware family has no {codec} {dir}r, and {} ranks below \
                 software under that policy",
                installed.join(", ")
            ),
        }
    }
}

/// Explain why `media_type` ended in software.
///
/// * `policy_software` — the policy asked for software.
/// * `hardware_installed` — hardware elements for this codec that the
///   `GStreamer` registry has, whether or not they were used.
/// * `refused` — hardware elements that were tried and failed, with why.
/// * `size` — the picture size, when known.
/// * `devices` — the V4L2 memory-to-memory nodes on this machine.
pub fn explain(
    direction: Direction,
    media_type: &str,
    policy_software: bool,
    hardware_installed: &[String],
    refused: &[(String, String)],
    size: Option<(u32, u32)>,
    devices: &[V4l2Device],
) -> FallbackReason {
    if policy_software {
        return FallbackReason::PolicyOff;
    }
    // The kernel's own limit is the most precise answer when it applies.
    if let Some((w, h)) = size {
        let limit = devices
            .iter()
            .filter(|d| role_matches(d.role, direction))
            .filter_map(|d| Some((d, d.coded_format(media_type)?.max_size?)))
            .max_by_key(|(_, (lw, lh))| u64::from(*lw).saturating_mul(u64::from(*lh)));
        if let Some((device, limit)) = limit
            && !v4l2::fits(limit, w, h)
        {
            return FallbackReason::ExceedsHardware {
                size: (w, h),
                limit,
                device: device.card.clone(),
            };
        }
    }
    if let Some((element, detail)) = refused.first() {
        return FallbackReason::HardwareRefused {
            element: element.clone(),
            detail: detail.clone(),
        };
    }
    if hardware_installed.is_empty() {
        return FallbackReason::NoHardware {
            devices: devices
                .iter()
                .filter(|d| role_matches(d.role, direction))
                .map(V4l2Device::describe)
                .collect(),
        };
    }
    FallbackReason::NotChosen {
        installed: hardware_installed.to_vec(),
    }
}

fn role_matches(role: V4l2Role, direction: Direction) -> bool {
    match direction {
        Direction::Decode => matches!(role, V4l2Role::Decoder { .. }),
        Direction::Encode => role == V4l2Role::Encoder,
    }
}

/// Log `reason` once per process for this direction, codec and kind of
/// reason. Returns whether this call logged.
pub fn report_once(direction: Direction, media_type: &str, reason: &FallbackReason) -> bool {
    static SEEN: Mutex<BTreeSet<(Direction, String, &'static str)>> = Mutex::new(BTreeSet::new());
    let key = (direction, media_type.to_string(), reason.key());
    let fresh = SEEN.lock().map_or(true, |mut seen| seen.insert(key));
    if fresh {
        tracing::warn!("media: {}", reason.describe(direction, media_type));
    }
    fresh
}

/// Log, once per codec, that a stream cannot be decoded at all: no hardware
/// takes it (`hardware_why`) and no software decoder is installed. Returns
/// the sentence, for the error shown to the user.
pub fn report_unplayable(media_type: &str, hardware_why: &str) -> String {
    static SEEN: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
    let codec = v4l2::codec_label(media_type);
    let text = format!(
        "{codec} cannot be decoded on this device: {hardware_why}, and no software \
         {codec} decoder is installed"
    );
    if SEEN
        .lock()
        .map_or(true, |mut seen| seen.insert(media_type.to_string()))
    {
        tracing::warn!("media: {text}");
    }
    text
}

/// Explain and report a real-time codec that settled on a software element.
///
/// `table` is the codec's full preference table, `installed` answers whether
/// an element is in the registry, and `tried` holds the `"element: error"`
/// lines of candidates that failed before the chosen one.
pub fn report_realtime(
    direction: Direction,
    codec: super::realtime::StreamCodec,
    policy: super::realtime::RealtimePolicy,
    table: &[super::realtime::CodecElement],
    installed: impl Fn(&str) -> bool,
    tried: &[String],
    size: Option<(u32, u32)>,
) {
    let Some(media_type) = v4l2::element_media_type(codec.wire_label()) else {
        return;
    };
    let hardware: Vec<String> = table
        .iter()
        .filter(|e| e.is_hardware() && installed(e.element))
        .map(|e| e.element.to_string())
        .collect();
    let refused: Vec<(String, String)> = tried
        .iter()
        .filter_map(|line| {
            let (element, why) = line.split_once(": ")?;
            hardware
                .iter()
                .any(|h| h == element)
                .then(|| (element.to_string(), why.to_string()))
        })
        .collect();
    let reason = explain(
        direction,
        media_type,
        policy == super::realtime::RealtimePolicy::SoftwareOnly,
        &hardware,
        &refused,
        size,
        v4l2::devices(),
    );
    report_once(direction, media_type, &reason);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hw::v4l2::{CodedFormat, MEDIA_AV1, MEDIA_H264, MEDIA_HEVC, MEDIA_JPEG};

    fn hantro() -> Vec<V4l2Device> {
        vec![
            V4l2Device {
                path: "/dev/video1".into(),
                driver: "hantro-vpu".into(),
                card: "rockchip,rk3568-vpu-dec".into(),
                role: V4l2Role::Decoder { stateless: true },
                coded: vec![CodedFormat {
                    fourcc: 0,
                    media_type: MEDIA_H264,
                    max_size: Some((1920, 1088)),
                }],
                raw: vec![],
            },
            V4l2Device {
                path: "/dev/video2".into(),
                driver: "hantro-vpu".into(),
                card: "rockchip,rk3568-vepu-enc".into(),
                role: V4l2Role::Encoder,
                coded: vec![CodedFormat {
                    fourcc: 0,
                    media_type: MEDIA_JPEG,
                    max_size: None,
                }],
                raw: vec![],
            },
        ]
    }

    #[test]
    fn a_software_fallback_names_the_real_reason() {
        let devices = hantro();
        let installed = vec!["v4l2slh264dec".to_string()];

        // 4K H.264 on the G1: the kernel's limit is the reason, even though
        // the decoder also failed when tried.
        let refused = vec![(
            "v4l2slh264dec".to_string(),
            "Failed to configure H264 decoder".to_string(),
        )];
        let r = explain(
            Direction::Decode,
            MEDIA_H264,
            false,
            &installed,
            &refused,
            Some((3840, 2160)),
            &devices,
        );
        assert_eq!(
            r,
            FallbackReason::ExceedsHardware {
                size: (3840, 2160),
                limit: (1920, 1088),
                device: "rockchip,rk3568-vpu-dec".into()
            }
        );
        assert_eq!(
            r.describe(Direction::Decode, MEDIA_H264),
            "H.264 3840x2160 decode runs in software: rockchip,rk3568-vpu-dec handles \
             H.264 only up to 1920x1088"
        );

        // A 1080p stream that still failed is a refusal, not a size problem.
        let r = explain(
            Direction::Decode,
            MEDIA_H264,
            false,
            &installed,
            &refused,
            Some((1920, 1080)),
            &devices,
        );
        assert!(matches!(r, FallbackReason::HardwareRefused { .. }), "{r:?}");

        // HEVC: no decoder at all, and the log says what hardware there is.
        let r = explain(
            Direction::Decode,
            MEDIA_HEVC,
            false,
            &[],
            &[],
            None,
            &devices,
        );
        let text = r.describe(Direction::Decode, MEDIA_HEVC);
        assert!(text.contains("no hardware HEVC decoder"), "{text}");
        assert!(text.contains("rk3568-vpu-dec"), "{text}");
        assert!(
            !text.contains("vepu"),
            "an encoder is not listed as decode hardware: {text}"
        );

        // Encode: the VEPU only does JPEG, so H.264 encode is software.
        let r = explain(
            Direction::Encode,
            MEDIA_H264,
            false,
            &[],
            &[],
            Some((1920, 1080)),
            &devices,
        );
        let text = r.describe(Direction::Encode, MEDIA_H264);
        assert!(text.contains("no hardware H.264 encoder"), "{text}");
        assert!(text.contains("JPEG"), "{text}");

        assert_eq!(
            explain(
                Direction::Decode,
                MEDIA_H264,
                true,
                &installed,
                &[],
                None,
                &devices
            ),
            FallbackReason::PolicyOff
        );

        // Installed hardware that was never tried was outranked by a policy.
        assert!(matches!(
            explain(
                Direction::Decode,
                MEDIA_H264,
                false,
                &installed,
                &[],
                Some((1280, 720)),
                &devices
            ),
            FallbackReason::NotChosen { .. }
        ));
    }

    #[test]
    fn the_same_reason_is_logged_once() {
        let reason = FallbackReason::NoHardware { devices: vec![] };
        assert!(report_once(Direction::Decode, MEDIA_AV1, &reason));
        assert!(!report_once(Direction::Decode, MEDIA_AV1, &reason));
        // A different reason for the same codec is news.
        assert!(report_once(
            Direction::Decode,
            MEDIA_AV1,
            &FallbackReason::PolicyOff
        ));
        assert!(report_once(Direction::Encode, MEDIA_AV1, &reason));
    }
}
