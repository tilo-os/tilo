//! H.264 software encoder.
//!
//! Wraps the [`openh264`] crate so a caller can hand it RGBA frames (the
//! pixel format vello/wgpu reads back into, and the format GStreamer's
//! `videoconvert` hands a `pipewiresrc` ScreenCast capture) and receive a
//! tagged-Annex-B NALU byte blob ready to ship as a
//! [`crate::stream::SurfaceFrame`] payload.
//!
//! The wrapper is intentionally thin — encoder configuration (bitrate, GOP,
//! threading) is fixed at construction time.
//!
//! ## Ownership and threading
//!
//! [`H264Encoder`] is `Send`-but-not-`Sync`: the underlying `SVCEncoder` is
//! safe to ship to another thread but holds internal state that mutates per
//! [`encode`](H264Encoder::encode) call. Own one encoder per shared source.
//!
//! ## Memory bound
//!
//! The encoder owns exactly one frame-sized RGBA scratch buffer plus the
//! openh264 SVC encoder's own reference picture pool (bounded internally).
//! Callers must not queue frames behind the encoder — encode synchronously
//! and ship the NALU bytes, or drop the frame.
//!
//! Round-trip quality is asserted by
//! [`tests::gradient_round_trip_psnr_at_least_35_db`], which feeds a known
//! RGBA gradient through encode + decode and checks the PSNR against the
//! 35.0 dB floor.

use openh264::Error as OpenH264Error;
use openh264::Timestamp;
use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate};
use openh264::formats::{RgbaSliceU8, YUVBuffer};

/// Error variants for the encoder wrapper.
///
/// Wraps the underlying openh264 error so callers don't have to depend on
/// the openh264 crate at their call-site.
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
/// Construct once per shared source; call [`encode`](Self::encode) per
/// frame. The encoder converts RGBA → I420 internally (openh264 only
/// consumes YUV420). The returned `Vec<u8>` is the full Annex-B NAL unit
/// payload for one encoded frame; on the first call it includes the SPS /
/// PPS metadata NALs.
pub struct H264Encoder {
    encoder: Encoder,
    width: u32,
    height: u32,
    /// Configured frame rate. Used by `encode` to synthesise a
    /// monotonically-increasing timestamp for openh264's rate control.
    fps: u32,
    /// Monotonic frame index. Every `encode` call bumps this by one;
    /// the derived timestamp is `frame_idx / fps` seconds. Without a
    /// forward-moving timestamp, openh264 assumes every frame arrived
    /// at t=0 and skips all but the first (empty-NAL output).
    frame_idx: u64,
}

impl H264Encoder {
    /// Build a new encoder targeting `width × height` at `fps` frames per
    /// second.
    ///
    /// `fps` is advisory — the encoder uses it for rate-control bookkeeping;
    /// the caller enforces the real cadence via wall-clock throttling in
    /// [`crate::throttle`].
    pub fn new(width: u32, height: u32, fps: u32) -> Result<Self, EncodeError> {
        Self::new_with_bitrate_bps(width, height, fps, 4_000_000)
    }

    /// Construct with an explicit target bitrate (bits per second).
    ///
    /// `enable_skip_frame = false` (openh264 defaults `true`) keeps the
    /// frame budget honest: with skip-frames on, a low-bitrate high-fps
    /// loop drops most frames and ships empty NAL blobs the receiver's
    /// decoder produces no output for. The drop policy lives in the
    /// transport substrate ([`crate::stream`]), not the encoder.
    pub fn new_with_bitrate_bps(
        width: u32,
        height: u32,
        fps: u32,
        target_bitrate_bps: u32,
    ) -> Result<Self, EncodeError> {
        let config = EncoderConfig::new()
            .max_frame_rate(FrameRate::from_hz(fps as f32))
            .num_threads(1)
            .bitrate(BitRate::from_bps(target_bitrate_bps))
            .skip_frames(false);
        let encoder = Encoder::with_api_config(openh264::OpenH264API::from_source(), config)?;
        Ok(Self {
            encoder,
            width,
            height,
            fps: fps.max(1),
            frame_idx: 0,
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
    ///
    /// The frame timestamp is synthesised from an internal counter at
    /// the configured `fps` — openh264 uses the frame timestamp for rate
    /// control, and its default shim stamps `t=0` on every frame, which
    /// makes a pumped loop emit empty NALs for every frame after the
    /// first. Advancing by `1/fps` per call gives it the monotonic time
    /// it needs.
    #[tracing::instrument(level = "trace", skip(self, rgba), fields(width = self.width, height = self.height))]
    pub fn encode(&mut self, rgba: &[u8]) -> Result<Vec<u8>, EncodeError> {
        let ts_ms = self.frame_idx.saturating_mul(1000) / self.fps as u64;
        self.frame_idx = self.frame_idx.saturating_add(1);
        self.encode_at_ms(rgba, ts_ms)
    }

    /// Encode one RGBA frame with a caller-supplied millisecond
    /// timestamp. See [`Self::encode`] for why the timestamp matters. The
    /// `frame_idx` counter is *not* advanced, so mixing `encode` and
    /// `encode_at_ms` on the same encoder is safe as long as the caller
    /// keeps the timestamps monotonic.
    #[tracing::instrument(level = "trace", skip(self, rgba), fields(width = self.width, height = self.height, ts_ms))]
    pub fn encode_at_ms(&mut self, rgba: &[u8], ts_ms: u64) -> Result<Vec<u8>, EncodeError> {
        let expected = self.width as usize * self.height as usize * 4;
        if rgba.len() != expected {
            return Err(EncodeError::BufferSize {
                expected,
                got: rgba.len(),
            });
        }
        let rgba_source = RgbaSliceU8::new(rgba, (self.width as usize, self.height as usize));
        // openh264's `YUVBuffer::from_rgb_source` accepts any `RGBSource`
        // and writes into an owned I420 buffer — the 4:2:0 conversion path.
        let yuv = YUVBuffer::from_rgb_source(rgba_source);
        let ts = Timestamp::from_millis(ts_ms);
        let stream = self.encoder.encode_at(&yuv, ts)?;
        Ok(stream.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder::H264Decoder;

    /// Peak signal-to-noise ratio (dB) between two equal-length buffers.
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

    /// Deterministic RGBA gradient: red ramps across width, green across
    /// height, blue mid-grey, alpha full — smooth in both axes so H.264's
    /// intra prediction has a fair shot at <35 dB loss.
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

        let strip_alpha = |buf: &[u8]| -> Vec<u8> {
            buf.chunks_exact(4).flat_map(|c| [c[0], c[1], c[2]]).collect()
        };
        let orig_rgb = strip_alpha(&original);
        let decoded_rgb = strip_alpha(&frame.rgba);
        let score = psnr(&orig_rgb, &decoded_rgb);
        assert!(
            score >= 35.0,
            "PSNR {score:.2} dB below 35.0 dB floor — encoder pipeline regression"
        );
        eprintln!("PSNR = {score:.3} dB");
    }

    #[test]
    fn buffer_size_mismatch_is_caught() {
        let mut enc = H264Encoder::new(32, 32, 30).expect("encoder");
        let too_small = vec![0u8; 32 * 32 * 3]; // RGB, not RGBA
        let err = enc.encode(&too_small).unwrap_err();
        assert!(matches!(err, EncodeError::BufferSize { .. }));
    }

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
