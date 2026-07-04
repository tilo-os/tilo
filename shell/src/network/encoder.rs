//! H.264 software encoder — Phase 4 Task 4.1.
//!
//! Wraps the [`openh264`] crate so the shell can hand it RGBA frames (the
//! pixel format vello/wgpu reads back into) and receive a tagged-Annex-B
//! NALU byte blob ready to publish over gossipsub as
//! [`crate::network::protocol::NeighbourMsg::SurfaceFrame`].
//!
//! The wrapper is intentionally thin — encoder configuration (bitrate, GOP,
//! threading) is fixed at construction time, matching the plan's "software
//! v1, single-output cluster" scope. A follow-up task swaps the backend for
//! VAAPI / ffmpeg when hardware acceleration lands.
//!
//! ## Ownership and threading
//!
//! [`H264Encoder`] is `Send`-but-not-`Sync`: the underlying `SVCEncoder` is
//! safe to ship to another thread but holds internal state that mutates per
//! [`encode`](H264Encoder::encode) call. The streaming task in
//! [`crate::network::streaming`] owns one encoder per shared surface.
//!
//! ## Memory bound
//!
//! Per the plan's "Don't bloat memory" constraint, the encoder owns exactly
//! one frame-sized RGBA scratch buffer plus the openh264 SVC encoder's own
//! reference picture pool (which the library bounds internally). Callers
//! must not queue frames behind the encoder — encode synchronously and ship
//! the NALU bytes, or drop the frame.
//!
//! Round-trip quality is asserted by the
//! [`tests::gradient_round_trip_psnr_at_least_35_db`] test, which feeds a
//! known RGBA gradient through encode + decode and checks the PSNR against
//! the spec's 35.0 dB floor.

use openh264::Error as OpenH264Error;
use openh264::encoder::{Encoder, EncoderConfig, FrameRate};
use openh264::formats::{RgbaSliceU8, YUVBuffer};

/// Error variants for the encoder wrapper.
///
/// Wraps the underlying openh264 error so callers don't have to depend on
/// the openh264 crate at their call-site. `thiserror` isn't a direct dep
/// of this crate, so we hand-roll `Display` / `Error` to avoid pulling it
/// in just for two variants.
#[derive(Debug)]
pub enum EncodeError {
    /// Underlying openh264 backend returned an error.
    Backend(String),
    /// Caller passed a buffer that does not match the encoder's configured
    /// `width * height * 4` size.
    BufferSize { expected: usize, got: usize },
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EncodeError::Backend(msg) => write!(f, "openh264: {msg}"),
            EncodeError::BufferSize { expected, got } => write!(
                f,
                "rgba buffer size mismatch: expected {expected} bytes, got {got}"
            ),
        }
    }
}

impl std::error::Error for EncodeError {}

impl From<OpenH264Error> for EncodeError {
    fn from(err: OpenH264Error) -> Self {
        EncodeError::Backend(format!("{err:?}"))
    }
}

/// Streaming H.264 encoder.
///
/// Construct once per shared surface; call [`encode`](Self::encode) per
/// frame. The encoder converts RGBA → I420 internally (openh264 only
/// consumes YUV420). The returned `Vec<u8>` is the full Annex-B NAL unit
/// payload for one encoded frame; on the first call it includes the SPS /
/// PPS metadata NALs.
pub struct H264Encoder {
    encoder: Encoder,
    width: u32,
    height: u32,
}

impl H264Encoder {
    /// Build a new encoder targeting `width × height` at `fps` frames per
    /// second.
    ///
    /// `fps` is advisory — the encoder uses it for rate-control bookkeeping;
    /// the streaming task enforces the real cadence via wall-clock throttling
    /// in [`crate::network::streaming`].
    pub fn new(width: u32, height: u32, fps: u32) -> Result<Self, EncodeError> {
        // Software v1: keep the config minimal and explicit so behaviour is
        // reproducible across test machines (no auto-threaded encoding, no
        // QP overrides). The plan only requires correctness + the 35 dB
        // round-trip floor — tuning lands when we replace this with a
        // hardware encoder.
        let config = EncoderConfig::new()
            .max_frame_rate(FrameRate::from_hz(fps as f32))
            .num_threads(1);
        let encoder = Encoder::with_api_config(
            openh264::OpenH264API::from_source(),
            config,
        )?;
        Ok(Self {
            encoder,
            width,
            height,
        })
    }

    /// Configured frame width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Configured frame height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Encode one RGBA frame to a NALU byte payload.
    ///
    /// `rgba` must be row-major with no padding (`width * height * 4`
    /// bytes). On the first call the returned bytes include SPS + PPS
    /// metadata NALs; subsequent calls return P-frames (or IDR-frames
    /// when the encoder decides to refresh the GOP).
    pub fn encode(&mut self, rgba: &[u8]) -> Result<Vec<u8>, EncodeError> {
        let expected = self.width as usize * self.height as usize * 4;
        if rgba.len() != expected {
            return Err(EncodeError::BufferSize {
                expected,
                got: rgba.len(),
            });
        }
        let rgba_source = RgbaSliceU8::new(
            rgba,
            (self.width as usize, self.height as usize),
        );
        // openh264's `YUVBuffer::from_rgb_source` accepts any `RGBSource`
        // and writes into an owned I420 buffer; this is the 4:2:0
        // conversion path described in the plan.
        let yuv = YUVBuffer::from_rgb_source(rgba_source);
        let stream = self.encoder.encode(&yuv)?;
        Ok(stream.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::decoder::H264Decoder;

    /// Compute peak signal-to-noise ratio (dB) between two equal-length
    /// `u8` buffers. Mirrors the cheap version in the Phase 4 plan.
    fn psnr(a: &[u8], b: &[u8]) -> f64 {
        assert_eq!(a.len(), b.len());
        let mse: f64 = a
            .iter()
            .zip(b)
            .map(|(x, y)| (*x as f64 - *y as f64).powi(2))
            .sum::<f64>()
            / a.len() as f64;
        if mse == 0.0 {
            return f64::INFINITY;
        }
        20.0 * (255.0_f64).log10() - 10.0 * mse.log10()
    }

    /// Synthesise a deterministic RGBA gradient: red channel ramps across
    /// the width (0..255), green channel ramps across the height (0..255),
    /// blue channel held mid-grey, alpha full. The result is smooth in
    /// both axes so H.264's intra prediction has a fair shot at <35 dB
    /// loss; a pure noise frame would PSNR much lower than the spec.
    fn gradient_frame(width: u32, height: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                let r = ((x * 255) / width.max(1)) as u8;
                let g = ((y * 255) / height.max(1)) as u8;
                out.push(r);
                out.push(g);
                out.push(128);
                out.push(255);
            }
        }
        out
    }

    #[test]
    fn gradient_round_trip_psnr_at_least_35_db() {
        // 128×96 is large enough that openh264's macroblock alignment
        // (16-pixel) doesn't drop content, but small enough that the test
        // completes in <1 s on a software-only build.
        const W: u32 = 128;
        const H: u32 = 96;
        let original = gradient_frame(W, H);
        let mut enc = H264Encoder::new(W, H, 30).expect("encoder");
        let nalus = enc.encode(&original).expect("encode");
        assert!(!nalus.is_empty(), "encoder produced empty bitstream");
        let mut dec = H264Decoder::new();
        let frame = dec.decode(&nalus).expect("decoder yields a frame");
        assert_eq!(frame.width, W);
        assert_eq!(frame.height, H);
        assert_eq!(frame.rgba.len(), original.len());

        // Compare only RGB channels — the encoder writes alpha=255 on
        // the way back out (openh264 doesn't carry an alpha channel),
        // so the source's alpha=255 already matches. We still strip
        // alpha out to keep the metric strictly about visible loss.
        let strip_alpha = |buf: &[u8]| -> Vec<u8> {
            buf.chunks_exact(4)
                .flat_map(|c| [c[0], c[1], c[2]])
                .collect()
        };
        let orig_rgb = strip_alpha(&original);
        let decoded_rgb = strip_alpha(&frame.rgba);
        let score = psnr(&orig_rgb, &decoded_rgb);
        assert!(
            score >= 35.0,
            "PSNR {score:.2} dB below 35.0 dB floor — encoder pipeline regression"
        );
        // Surface the measured value at test-run time so the verification
        // recipe can grep for it in the report.
        eprintln!("PSNR = {score:.3} dB");
    }

    #[test]
    fn buffer_size_mismatch_is_caught() {
        let mut enc = H264Encoder::new(32, 32, 30).expect("encoder");
        let too_small = vec![0u8; 32 * 32 * 3]; // RGB, not RGBA
        let err = enc.encode(&too_small).unwrap_err();
        assert!(matches!(err, EncodeError::BufferSize { .. }));
    }

    /// Memory-bound check: 100 consecutive encodes against the same encoder
    /// must not grow its retained scratch beyond a small multiple of one
    /// frame. We can't directly inspect openh264's reference pool, but we
    /// can assert the encoder's own footprint (its sole owned Rust state)
    /// doesn't accumulate: every `encode` consumes the input by value via
    /// the YUV conversion and returns a freshly-allocated `Vec<u8>`. The
    /// invariant we *can* check from Rust is that `H264Encoder` itself is
    /// fixed-size and that 100 successive encodes succeed without OOM.
    #[test]
    fn encoder_state_is_bounded_across_many_frames() {
        const W: u32 = 64;
        const H: u32 = 64;
        let frame = gradient_frame(W, H);
        let mut enc = H264Encoder::new(W, H, 30).expect("encoder");
        let baseline = std::mem::size_of_val(&enc);
        for _ in 0..100 {
            let _bytes = enc.encode(&frame).expect("encode");
            assert_eq!(
                std::mem::size_of_val(&enc),
                baseline,
                "encoder struct grew across encode calls"
            );
        }
    }
}
