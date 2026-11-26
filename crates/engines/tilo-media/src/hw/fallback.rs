//! Why a codec ran on the CPU, said once per process.
//!
//! Software is a legitimate outcome — a codec the chip has no block for, a stream
//! larger than the block takes, hardware turned off — but never a silent one: each
//! path asks [`explain`] and hands it to [`report_once`], which logs one warning per
//! (direction, codec, reason).

use std::collections::BTreeSet;
use std::sync::Mutex;

use super::v4l2::{self, V4l2Device, V4l2Role};

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
    PolicyOff,
    ExceedsHardware {
        size: (u32, u32),
        limit: (u32, u32),
        device: String,
    },
    HardwareRefused {
        element: String,
        detail: String,
    },
    /// No hardware element for this codec is installed. `devices` names the
    /// V4L2 codec nodes that do exist, so "this chip's decoder has no VP9" is
    /// distinguishable from "this machine has no video hardware at all".
    NoHardware {
        devices: Vec<String>,
    },
    NotChosen {
        installed: Vec<String>,
    },
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

pub fn report_once(direction: Direction, media_type: &str, reason: &FallbackReason) -> bool {
    static SEEN: Mutex<BTreeSet<(Direction, String, &'static str)>> = Mutex::new(BTreeSet::new());
    let key = (direction, media_type.to_string(), reason.key());
    let fresh = SEEN.lock().map_or(true, |mut seen| seen.insert(key));
    if fresh {
        tracing::warn!("media: {}", reason.describe(direction, media_type));
    }
    fresh
}

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
