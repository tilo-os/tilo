//! H.264 software encoder.
//!
//! Wraps the [`openh264`] crate: takes RGBA frames (the format vello/wgpu
//! reads back into, and what `GStreamer`'s `videoconvert` hands a
//! `pipewiresrc` `ScreenCast` capture) and returns a tagged-Annex-B NALU byte
//! blob ready to ship as a [`crate::stream::SurfaceFrame`] payload. The
//! wrapper is thin — bitrate/GOP/threading are fixed at construction.
//!
//! [`H264Encoder`] is `Send`-but-not-`Sync`: the underlying `SVCEncoder`
//! mutates per [`encode`](H264Encoder::encode) call, so own one encoder per
//! shared source. It holds exactly one frame-sized RGBA scratch buffer plus
//! openh264's own (internally bounded) reference picture pool — callers
//! must not queue frames behind it; encode synchronously and ship the
//! bytes, or drop the frame.
//!
//! Round-trip quality is asserted by
//! [`tests::gradient_round_trip_psnr_at_least_35_db`] against a 35.0 dB
//! PSNR floor.

use openh264::Error as OpenH264Error;
use openh264::Timestamp;
use openh264::encoder::{BitRate, Encoder, EncoderConfig, FrameRate, IntraFramePeriod};
use openh264::formats::{RgbaSliceU8, YUVBuffer};

/// Error variants for the encoder wrapper, wrapping the underlying openh264 error so callers don't depend on that crate directly.
#[derive(Debug)]
pub enum EncodeError {
    /// Underlying openh264 backend returned an error.
    Backend(String),
    /// Caller passed a buffer that doesn't match the encoder's configured `width * height * 4` size.
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

/// Streaming H.264 encoder. Construct once per shared source; call
/// [`encode`](Self::encode) per frame. Converts RGBA → I420 internally
/// (openh264 only consumes YUV420); the returned bytes are the full
/// Annex-B NAL payload, including SPS/PPS metadata on the first call.
pub struct H264Encoder {
    encoder: Encoder,
    width: u32,
    height: u32,
    /// Configured frame rate; used to synthesise the encode timestamp (see `frame_idx`).
    fps: u32,
    /// Monotonic frame index; each `encode` call bumps it by one, giving a
    /// timestamp of `frame_idx / fps` seconds. openh264 needs a forward-moving
    /// timestamp — without it, it assumes every frame arrived at t=0 and skips
    /// all but the first (empty-NAL output).
    frame_idx: u64,
}

impl H264Encoder {
    /// Builds an encoder targeting `width × height` at `fps` frames per second.
    /// `fps` is advisory (rate-control bookkeeping); cadence is enforced
    /// by the caller via wall-clock throttling in [`crate::throttle`].
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::Backend`] if the underlying openh264 encoder
    /// fails to initialise.
    pub fn new(width: u32, height: u32, fps: u32) -> Result<Self, EncodeError> {
        Self::new_with_bitrate_bps(width, height, fps, 4_000_000)
    }

    /// Constructs with an explicit target bitrate (bits per second).
    ///
    /// `enable_skip_frame = false` (openh264 defaults `true`) keeps the frame
    /// budget — skip-frames on would drop frames under low-bitrate,
    /// high-fps load and ship empty NALs the receiver can't decode. Drop
    /// policy belongs to the transport substrate ([`crate::stream`]).
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::Backend`] if the underlying openh264 encoder
    /// fails to initialise.
    pub fn new_with_bitrate_bps(
        width: u32,
        height: u32,
        fps: u32,
        target_bitrate_bps: u32,
    ) -> Result<Self, EncodeError> {
        Self::new_with_bitrate_and_keyframe_interval(width, height, fps, target_bitrate_bps, 0)
    }

    /// Constructs with explicit bitrate and periodic IDR cadence. A zero
    /// `keyframe_interval_frames` preserves `OpenH264`'s automatic cadence.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::Backend`] if the underlying openh264 encoder
    /// fails to initialise.
    #[allow(
        clippy::cast_precision_loss,
        reason = "fps is a frame rate — always small (tens to low hundreds) and \
                  exactly representable as f32"
    )]
    pub fn new_with_bitrate_and_keyframe_interval(
        width: u32,
        height: u32,
        fps: u32,
        target_bitrate_bps: u32,
        keyframe_interval_frames: u32,
    ) -> Result<Self, EncodeError> {
        let config = EncoderConfig::new()
            .max_frame_rate(FrameRate::from_hz(fps as f32))
            .num_threads(1)
            .bitrate(BitRate::from_bps(target_bitrate_bps))
            .intra_frame_period(IntraFramePeriod::from_num_frames(keyframe_interval_frames))
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

    /// Encodes one RGBA frame (row-major, no padding, `width * height * 4`
    /// bytes) to a NALU byte payload — SPS/PPS on the first call, P/IDR frames
    /// after. The timestamp comes from the internal frame counter; see the
    /// `frame_idx` field for why it must be monotonic.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::BufferSize`] if `rgba` isn't exactly `width *
    /// height * 4` bytes, or [`EncodeError::Backend`] if the underlying
    /// openh264 encoder fails.
    #[tracing::instrument(level = "trace", skip(self, rgba), fields(width = self.width, height = self.height))]
    pub fn encode(&mut self, rgba: &[u8]) -> Result<Vec<u8>, EncodeError> {
        // `self.fps` is set via `fps.max(1)` at construction, so this never
        // divides by zero; `checked_div` avoids relying on that invariant here.
        let ts_ms = self
            .frame_idx
            .saturating_mul(1000)
            .checked_div(u64::from(self.fps))
            .unwrap_or(0);
        self.frame_idx = self.frame_idx.saturating_add(1);
        self.encode_at_ms(rgba, ts_ms)
    }

    /// Encodes one RGBA frame with a caller-supplied millisecond timestamp
    /// (see [`Self::encode`]). Does not advance `frame_idx`, so mixing the two
    /// calls on one encoder is safe as long as timestamps stay monotonic.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::BufferSize`] if `rgba` isn't exactly `width *
    /// height * 4` bytes, or [`EncodeError::Backend`] if the underlying
    /// openh264 encoder fails.
    #[tracing::instrument(level = "trace", skip(self, rgba), fields(width = self.width, height = self.height, ts_ms))]
    pub fn encode_at_ms(&mut self, rgba: &[u8], ts_ms: u64) -> Result<Vec<u8>, EncodeError> {
        let expected = (self.width as usize)
            .saturating_mul(self.height as usize)
            .saturating_mul(4);
        if rgba.len() != expected {
            return Err(EncodeError::BufferSize {
                expected,
                got: rgba.len(),
            });
        }
        let rgba_source = RgbaSliceU8::new(rgba, (self.width as usize, self.height as usize));
        // Converts RGBA to an owned I420 (4:2:0) buffer via `RGBSource`.
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
    #[allow(
        clippy::cast_precision_loss,
        reason = "test buffers are at most a few million bytes, far inside f64's exact integer range"
    )]
    fn psnr(a: &[u8], b: &[u8]) -> f64 {
        assert_eq!(a.len(), b.len());
        let mse: f64 = a
            .iter()
            .zip(b)
            .map(|(x, y)| (f64::from(*x) - f64::from(*y)).powi(2))
            .sum::<f64>()
            / a.len() as f64;
        if mse == 0.0 {
            return f64::INFINITY;
        }
        20.0 * (255.0_f64).log10() - 10.0 * mse.log10()
    }

    /// Deterministic RGBA gradient (red across width, green across height,
    /// blue mid-grey) — smooth in both axes for a fair intra-prediction test.
    fn gradient_frame(width: u32, height: u32) -> Vec<u8> {
        let capacity = width.saturating_mul(height).saturating_mul(4) as usize;
        let mut out = Vec::with_capacity(capacity);
        let channel = |numerator: u32, denominator: u32| -> u8 {
            let scaled = numerator
                .saturating_mul(255)
                .checked_div(denominator.max(1))
                .unwrap_or(0);
            u8::try_from(scaled).unwrap_or(u8::MAX)
        };
        for y in 0..height {
            for x in 0..width {
                out.push(channel(x, width));
                out.push(channel(y, height));
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
    fn configured_keyframe_interval_emits_periodic_idr_frames() {
        const W: u32 = 64;
        const H: u32 = 64;
        const INTERVAL: u32 = 4;
        let mut frame = gradient_frame(W, H);
        let mut enc =
            H264Encoder::new_with_bitrate_and_keyframe_interval(W, H, 30, 2_000_000, INTERVAL)
                .expect("encoder");
        let mut keyframes = Vec::new();
        for frame_index in 0..=INTERVAL * 2 {
            // Change the picture so OpenH264 cannot optimise the access unit
            // away while the test advances through two GOP boundaries.
            frame[0] = u8::try_from(frame_index).unwrap();
            let bytes = enc.encode(&frame).expect("encode");
            if crate::session::is_h264_keyframe(&bytes) {
                keyframes.push(frame_index);
            }
        }
        assert_eq!(keyframes, [0, INTERVAL, INTERVAL * 2]);
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
