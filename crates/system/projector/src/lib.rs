//! Wire contract shared by the Tilo shell and projection broker.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};

pub mod control;

const VIDEO_PACKET_MAGIC: [u8; 8] = *b"TLOPRJ01";
const MAX_VIDEO_PACKET_BYTES: usize = 16 * 1024 * 1024;

/// Wireless-display protocols understood by the projection broker.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectionProtocol {
    Miracast,
    Chromecast,
    AirPlay,
    FCast,
}

impl ProjectionProtocol {
    pub const ALL: [Self; 4] = [Self::Miracast, Self::Chromecast, Self::AirPlay, Self::FCast];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Miracast => "Miracast",
            Self::Chromecast => "Google Cast",
            Self::AirPlay => "AirPlay",
            Self::FCast => "FCast",
        }
    }
}

/// Desktop layout requested for the remote display.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectionMode {
    #[default]
    Mirror,
    Extend,
    ExternalOnly,
}

/// A user ceiling for a display stream. This deliberately describes display
/// capabilities rather than wireless-protocol presets so the same preference
/// can be reused for wired outputs later.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum DisplayResolutionLimit {
    #[default]
    Native,
    P2160,
    P1440,
    P1080,
    P720,
}

impl DisplayResolutionLimit {
    pub const ALL: [Self; 5] = [
        Self::Native,
        Self::P2160,
        Self::P1440,
        Self::P1080,
        Self::P720,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Native => "Native",
            Self::P2160 => "4K",
            Self::P1440 => "1440p",
            Self::P1080 => "1080p",
            Self::P720 => "720p",
        }
    }

    pub const fn dimensions(self) -> Option<(u32, u32)> {
        match self {
            Self::Native => None,
            Self::P2160 => Some((3840, 2160)),
            Self::P1440 => Some((2560, 1440)),
            Self::P1080 => Some((1920, 1080)),
            Self::P720 => Some((1280, 720)),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct DisplayStreamPolicy {
    /// Continuously fit the stream to receiver, encoder and link feedback.
    pub adaptive: bool,
    pub resolution_limit: DisplayResolutionLimit,
    pub max_fps: u32,
}

impl Default for DisplayStreamPolicy {
    fn default() -> Self {
        Self {
            adaptive: true,
            resolution_limit: DisplayResolutionLimit::Native,
            max_fps: 60,
        }
    }
}

impl DisplayStreamPolicy {
    /// Resolves the user's ceiling against an advertised receiver limit. An
    /// unknown native limit remains conservative until feedback permits a
    /// probe upward.
    pub fn ceiling(self, receiver: Option<(u32, u32)>) -> CaptureProfile {
        let requested = self.resolution_limit.dimensions();
        let (width, height) = match (requested, receiver) {
            (Some(a), Some(b)) => {
                if a.0.saturating_mul(a.1) <= b.0.saturating_mul(b.1) {
                    a
                } else {
                    b
                }
            }
            (Some(value), None) | (None, Some(value)) => value,
            (None, None) => (1920, 1080),
        };
        let fps = self.max_fps.clamp(24, 120);
        // `width`/`height` may originate from a receiver-advertised capability
        // over the network, so this is untrusted-magnitude data. `checked_mul`
        // rejects an overflowing (malformed) advertisement outright by
        // falling back to the bitrate floor, rather than saturating to a
        // plausible-looking but made-up value.
        let pixels_per_second = u64::from(width)
            .checked_mul(u64::from(height))
            .and_then(|value| value.checked_mul(u64::from(fps)))
            .unwrap_or(0);
        let bitrate_kbps = (pixels_per_second / 12_000).clamp(2_500, 40_000) as u32;
        CaptureProfile::new(width, height, fps, bitrate_kbps)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CaptureProfile {
    pub max_width: u32,
    pub max_height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
}

impl CaptureProfile {
    pub const fn new(max_width: u32, max_height: u32, fps: u32, bitrate_kbps: u32) -> Self {
        Self {
            max_width,
            max_height,
            fps,
            bitrate_kbps,
        }
    }
}

impl ProjectionMode {
    pub const ALL: [Self; 3] = [Self::Mirror, Self::Extend, Self::ExternalOnly];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Mirror => "Duplicate",
            Self::Extend => "Extend",
            Self::ExternalOnly => "Only there",
        }
    }
}

/// One receiver found by Wi-Fi Direct or local-network discovery.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ProjectionDevice {
    pub id: String,
    pub name: String,
    pub protocol: ProjectionProtocol,
    #[serde(default)]
    pub detail: String,
    #[serde(default = "mirror_only")]
    pub modes: Vec<ProjectionMode>,
    #[serde(default)]
    pub addresses: Vec<String>,
    #[serde(default)]
    pub port: u16,
}

fn mirror_only() -> Vec<ProjectionMode> {
    vec![ProjectionMode::Mirror]
}

impl ProjectionDevice {
    pub fn supports(&self, mode: ProjectionMode) -> bool {
        self.modes.contains(&mode)
    }
}

/// Options passed atomically to the broker when connecting.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ProjectionOptions {
    pub mode: ProjectionMode,
    pub audio: bool,
    pub stream_policy: DisplayStreamPolicy,
    /// Optional receiver PIN/password. It is never serialised with the
    /// options: it crosses to the broker only as the separate, wiped
    /// [`control::Credential`] of a connect request.
    #[serde(skip)]
    pub credential: Option<String>,
}

impl Default for ProjectionOptions {
    fn default() -> Self {
        Self {
            mode: ProjectionMode::Mirror,
            audio: true,
            stream_policy: DisplayStreamPolicy::default(),
            credential: None,
        }
    }
}

/// One Annex-B H.264 access unit sent from the compositor to the broker.
///
/// The broker is intentionally a separate process, so this format is small,
/// versioned and length-bounded rather than sharing Rust object memory across
/// a trust boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VideoPacket {
    pub width: u32,
    pub height: u32,
    pub presentation_time_us: u64,
    pub keyframe: bool,
    pub annex_b: Vec<u8>,
}

impl VideoPacket {
    /// # Errors
    ///
    /// Returns an error if `annex_b` exceeds 16 MiB, or if writing to `writer`
    /// fails.
    pub fn write_to(&self, mut writer: impl Write) -> io::Result<()> {
        if self.annex_b.len() > MAX_VIDEO_PACKET_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "projection video packet exceeds 16 MiB",
            ));
        }
        // The length check above already bounds `annex_b.len()` well within
        // `u32::MAX` (16 MiB vs. ~4 GiB), so this narrowing cannot truncate.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "annex_b.len() is bounded to MAX_VIDEO_PACKET_BYTES (16 MiB) above"
        )]
        let payload_len = self.annex_b.len() as u32;
        writer.write_all(&VIDEO_PACKET_MAGIC)?;
        writer.write_all(&self.width.to_be_bytes())?;
        writer.write_all(&self.height.to_be_bytes())?;
        writer.write_all(&self.presentation_time_us.to_be_bytes())?;
        writer.write_all(&[u8::from(self.keyframe)])?;
        writer.write_all(&payload_len.to_be_bytes())?;
        writer.write_all(&self.annex_b)
    }

    /// # Errors
    ///
    /// Returns an error if the stream does not begin with the expected magic
    /// bytes, if the keyframe flag byte is neither 0 nor 1, if the declared
    /// payload length exceeds 16 MiB, or if reading from `reader` fails
    /// (including a short read before the declared payload is complete).
    pub fn read_from(mut reader: impl Read) -> io::Result<Self> {
        let mut magic = [0u8; 8];
        reader.read_exact(&mut magic)?;
        if magic != VIDEO_PACKET_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid projection video packet magic",
            ));
        }
        let width = read_u32(&mut reader)?;
        let height = read_u32(&mut reader)?;
        let presentation_time_us = read_u64(&mut reader)?;
        let mut keyframe = [0u8; 1];
        reader.read_exact(&mut keyframe)?;
        if keyframe[0] > 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid projection keyframe flag",
            ));
        }
        let payload_len = read_u32(&mut reader)? as usize;
        if payload_len > MAX_VIDEO_PACKET_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "projection video packet exceeds 16 MiB",
            ));
        }
        let mut annex_b = vec![0; payload_len];
        reader.read_exact(&mut annex_b)?;
        Ok(Self {
            width,
            height,
            presentation_time_us,
            keyframe: keyframe[0] == 1,
            annex_b,
        })
    }
}

fn read_u32(reader: &mut impl Read) -> io::Result<u32> {
    let mut bytes = [0u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_be_bytes(bytes))
}

fn read_u64(reader: &mut impl Read) -> io::Result<u64> {
    let mut bytes = [0u8; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_be_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_packets_round_trip_and_bound_untrusted_lengths() {
        // Phase `video_packet_round_trips_without_touching_the_codec_payload`.
        {
            let packet = VideoPacket {
                width: 1280,
                height: 720,
                presentation_time_us: 33_333,
                keyframe: true,
                annex_b: vec![0, 0, 0, 1, 0x67, 0x42, 0xc0, 0x1f],
            };
            let mut wire = Vec::new();
            packet.write_to(&mut wire).expect("encode packet");
            assert_eq!(
                VideoPacket::read_from(wire.as_slice()).expect("decode packet"),
                packet
            );
        }
        // Phase `video_packet_rejects_untrusted_oversized_length_before_allocating`.
        {
            let mut wire = Vec::new();
            wire.extend_from_slice(&VIDEO_PACKET_MAGIC);
            wire.extend_from_slice(&1280u32.to_be_bytes());
            wire.extend_from_slice(&720u32.to_be_bytes());
            wire.extend_from_slice(&0u64.to_be_bytes());
            wire.push(0);
            wire.extend_from_slice(&u32::MAX.to_be_bytes());
            let error = VideoPacket::read_from(wire.as_slice()).expect_err("oversize rejected");
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        }
    }

    #[test]
    fn stream_ceilings_follow_user_and_receiver_capabilities() {
        // Phase `stream_ceiling_is_the_minimum_of_user_and_receiver_capabilities`.
        {
            let policy = DisplayStreamPolicy {
                adaptive: true,
                resolution_limit: DisplayResolutionLimit::P2160,
                max_fps: 60,
            };
            let profile = policy.ceiling(Some((1920, 1080)));
            assert_eq!((profile.max_width, profile.max_height), (1920, 1080));

            let policy = DisplayStreamPolicy {
                resolution_limit: DisplayResolutionLimit::P720,
                ..policy
            };
            let profile = policy.ceiling(Some((3840, 2160)));
            assert_eq!((profile.max_width, profile.max_height), (1280, 720));
        }
        // Phase `native_policy_uses_receiver_capability_without_a_wireless_preset`.
        {
            let profile = DisplayStreamPolicy::default().ceiling(Some((2560, 1440)));
            assert_eq!((profile.max_width, profile.max_height), (2560, 1440));
            assert_eq!(profile.fps, 60);
        }
    }
}
