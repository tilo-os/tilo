//! Openh264 (Cisco software) backend for the [`Encoder`] trait.
//!
//! Thin adapter over the existing [`crate::network::encoder::H264Encoder`]
//! wrapper — that module was written before the trait existed, so this
//! backend just measures wall-clock + reports keyframe status while
//! delegating the actual encode to the SVC encoder.
//!
//! The SVC encoder still owns its I420 scratch pool; this backend adds
//! no per-frame allocations beyond what the wrapper already needs.

use std::time::Instant;

use super::{Encoder, EncoderError, EncoderInit, EncoderKind, FrameInput, FrameStats};
use crate::network::encoder::{EncodeError, H264Encoder};

/// Cisco openh264 software backend — the always-available fallback.
pub struct OpenH264Backend {
    inner: H264Encoder,
    /// The first `encode` call includes SPS + PPS metadata NALs. We
    /// treat frame 0 as a keyframe for the stats field even though
    /// openh264 doesn't hand us an explicit "this NAL was IDR" flag —
    /// the receiver's decoder ignores the field, this is purely for the
    /// keyframe counter in `PeerSurfaceSnapshot`.
    frames_encoded: u64,
    width: u32,
    height: u32,
}

impl OpenH264Backend {
    /// Construct the backend. Bitrate + keyframe interval are advisory
    /// on the openh264 path — the SVC encoder handles GOP structure
    /// internally.
    pub fn new(init: EncoderInit) -> Result<Self, EncoderError> {
        // Openh264 is H.264 8-bit only; HDR / HEVC pipelines never reach
        // us because probe() reports unavailable, but be defensive.
        if init.codec != super::Codec::H264 {
            return Err(EncoderError::Unavailable {
                kind: EncoderKind::Openh264,
                reason: format!(
                    "openh264 backend does not support {:?}; use a hardware backend",
                    init.codec
                ),
            });
        }
        if init.hdr.is_some() || init.colorimetry.is_hdr() {
            return Err(EncoderError::Unavailable {
                kind: EncoderKind::Openh264,
                reason: "openh264 does not support HDR (10-bit, PQ/HLG)".to_string(),
            });
        }
        // Bitrate from the caller (kbps) → openh264's bps. Prior wiring
        // called ::new() and inherited the library's 120 kbps default,
        // which skip-framed most of the stream on any real workload.
        let bitrate_bps = (init.bitrate_kbps as u32).saturating_mul(1000).max(500_000);
        let inner = H264Encoder::new_with_bitrate_bps(
            init.width,
            init.height,
            init.fps.max(1),
            bitrate_bps,
        )
        .map_err(|e: EncodeError| EncoderError::ConfigFailed {
            kind: EncoderKind::Openh264,
            detail: format!("{e:?}"),
        })?;
        Ok(Self {
            inner,
            frames_encoded: 0,
            width: init.width,
            height: init.height,
        })
    }
}

impl Encoder for OpenH264Backend {
    fn kind(&self) -> EncoderKind {
        EncoderKind::Openh264
    }
    fn width(&self) -> u32 {
        self.width
    }
    fn height(&self) -> u32 {
        self.height
    }
    fn encode(
        &mut self,
        input: FrameInput<'_>,
        out: &mut Vec<u8>,
    ) -> Result<FrameStats, EncoderError> {
        // Software only handles RGBA. Anything else is a caller bug —
        // the routing lives in `SharedFlatpakSession::encode_next_frame`
        // which checks `Encoder::kind()` before staging DMABUF/NV12.
        let rgba = match input {
            FrameInput::Rgba(bytes) => bytes,
            FrameInput::Nv12 { .. } | FrameInput::DmabufFd { .. } => {
                return Err(EncoderError::EncodeFailed {
                    kind: EncoderKind::Openh264,
                    detail: "openh264 backend only accepts RGBA input".to_string(),
                });
            }
        };
        let start = Instant::now();
        let bytes = self.inner.encode(rgba).map_err(|e| EncoderError::EncodeFailed {
            kind: EncoderKind::Openh264,
            detail: format!("{e:?}"),
        })?;
        let elapsed = start.elapsed();
        let is_keyframe = self.frames_encoded == 0;
        self.frames_encoded = self.frames_encoded.saturating_add(1);
        let stats = FrameStats {
            encode_wallclock: elapsed,
            keyframe: is_keyframe,
            bytes: bytes.len(),
        };
        out.extend_from_slice(&bytes);
        Ok(stats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                out.push(((x * 255) / w.max(1)) as u8);
                out.push(((y * 255) / h.max(1)) as u8);
                out.push(128);
                out.push(255);
            }
        }
        out
    }

    #[test]
    fn openh264_backend_encodes_rgba_frames() {
        const W: u32 = 64;
        const H: u32 = 64;
        let init = EncoderInit::sdr_h264(W, H, 30, 5000, 1);
        let mut backend = OpenH264Backend::new(init).expect("construct");
        let frame = gradient(W, H);
        let mut out = Vec::new();
        let stats = backend
            .encode(FrameInput::Rgba(&frame), &mut out)
            .expect("encode");
        assert!(!out.is_empty(), "openh264 backend produced empty NAL stream");
        assert_eq!(stats.bytes, out.len());
        assert!(stats.keyframe, "first frame must be reported as keyframe");
        assert!(stats.encode_wallclock.as_nanos() > 0);
    }

    #[test]
    fn openh264_backend_rejects_dmabuf_and_nv12() {
        let init = EncoderInit::sdr_h264(32, 32, 30, 5000, 1);
        let mut backend = OpenH264Backend::new(init).expect("construct");
        let mut out = Vec::new();
        // NV12 refusal.
        let y = vec![0u8; 32 * 32];
        let uv = vec![0u8; 32 * 32 / 2];
        let err = backend
            .encode(FrameInput::Nv12 { y: &y, uv: &uv }, &mut out)
            .unwrap_err();
        matches!(err, EncoderError::EncodeFailed { .. });
        // DMABUF refusal — openh264 has no zero-copy import path.
        let err2 = backend
            .encode(
                FrameInput::DmabufFd {
                    plane_fds: [-1, -1, -1, -1],
                    plane_offsets: [0, 0, 0, 0],
                    plane_strides: [128, 0, 0, 0],
                    num_planes: 1,
                    drm_fourcc: 0x34324152,
                    drm_modifier: 0,
                    width: 32,
                    height: 32,
                },
                &mut out,
            )
            .unwrap_err();
        matches!(err2, EncoderError::EncodeFailed { .. });
    }
}
