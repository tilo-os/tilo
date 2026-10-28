//! How a shared window is encoded: the bitrate cap, the rate, the encoder
//! preferred and HDR, as the compositor's `desktop.toml` sets them.

use serde::{Deserialize, Serialize};

/// Video settings for shared application windows.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(default)]
pub struct FlatpakShareConfig {
    /// Video bitrate cap in kbps.
    pub max_bitrate_kbps: u32,
    /// The rate a changing window streams at where no display reports its
    /// refresh; elsewhere it follows the display the window is on.
    pub target_fps: u32,
    pub encoder: EncoderChoice,
    /// HDR uses AV1 or HEVC Main10 when a hardware 10-bit encoder is
    /// available and every viewer can decode it.
    #[serde(default)]
    pub hdr: Option<crate::hdr_metadata::HdrConfig>,
}

impl Default for FlatpakShareConfig {
    fn default() -> Self {
        Self {
            max_bitrate_kbps: 8000,
            target_fps: 60,
            encoder: EncoderChoice::Auto,
            hdr: None,
        }
    }
}

/// Preferred video encoder, as a preference for the media stack's element
/// selection (see [`EncoderChoice::policy`]).
///
/// `"vaapi"` / `"nvenc"` / `"v4l2"` prefer that hardware family, falling back
/// to software.
#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EncoderChoice {
    /// Hardware first, then royalty-free software, then Cisco's openh264.
    Auto,
    /// Software encoders only.
    Software,
    /// Prefer VA-API; other hardware only after software.
    Vaapi,
    /// Prefer NVENC; other hardware only after software.
    Nvenc,
    /// Prefer the kernel's V4L2 codec blocks (ARM boards); other hardware only
    /// after software.
    V4l2,
}
