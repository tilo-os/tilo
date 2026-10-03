//! Video encoder for the streaming stack.
//!
//! [`VideoEncoder`] takes RGBA frames (the format vello/wgpu reads back into,
//! and what `GStreamer`'s `videoconvert` hands a `pipewiresrc` `ScreenCast`
//! capture) and returns compressed access units, ready to ship as
//! [`crate::stream::SurfaceFrame`] payloads. It is a thin shell over
//! [`media::RealtimeEncoder`], which picks the element: hardware first, then
//! royalty-free software (VP9, AV1), then Cisco's `openh264` binary loaded by
//! the system for H.264.
//!
//! [`VideoEncoder`] is `Send`-but-not-`Sync`: own one encoder per shared
//! source and encode synchronously — callers must not queue frames behind
//! it; encode and ship the bytes, or drop the frame.

use std::time::Duration;

use media::EncodedUnit;
use media::realtime::{EncoderConfig, InputFormat, InputFrame, RealtimeEncoder, StreamCodec};

/// Default target bitrate, in bits per second.
pub const DEFAULT_BITRATE_BPS: u64 = 4_000_000;

/// Error variants for the encoder wrapper.
#[derive(Debug)]
pub enum EncodeError {
    /// The media stack could not build or run an encoder.
    Backend(String),
    /// Caller passed a buffer that doesn't match the encoder's configured `width * height * 4` size.
    BufferSize { expected: usize, got: usize },
}

impl std::fmt::Display for EncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EncodeError::Backend(msg) => write!(f, "video encoder: {msg}"),
            EncodeError::BufferSize { expected, got } => write!(
                f,
                "rgba buffer size mismatch: expected {expected} bytes, got {got}"
            ),
        }
    }
}

impl std::error::Error for EncodeError {}

impl From<media::RealtimeError> for EncodeError {
    fn from(err: media::RealtimeError) -> Self {
        EncodeError::Backend(err.to_string())
    }
}

/// Streaming video encoder. Construct once per shared source; call
/// [`encode`](Self::encode) per frame.
pub struct VideoEncoder {
    inner: RealtimeEncoder,
    width: u32,
    height: u32,
    /// Configured frame rate; used to synthesise timestamps (see `frame_idx`).
    fps: u32,
    /// Monotonic frame index; each [`Self::encode`] call bumps it by one,
    /// giving a timestamp of `frame_idx / fps` seconds. Encoders need a
    /// forward-moving timestamp for rate control.
    frame_idx: u64,
}

impl std::fmt::Debug for VideoEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoEncoder")
            .field("element", &self.inner.element().element)
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

impl VideoEncoder {
    /// Builds an encoder for `codec` at `width × height`, `fps` frames per
    /// second (advisory; cadence is enforced by [`crate::throttle`]).
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::Backend`] if no encoder for `codec` can run
    /// here (see [`media::RealtimeEncoder::new`]).
    pub fn new(codec: StreamCodec, width: u32, height: u32, fps: u32) -> Result<Self, EncodeError> {
        Self::with_options(codec, width, height, fps, DEFAULT_BITRATE_BPS, 0, false)
    }

    /// Constructs with an explicit target bitrate (bits per second), periodic
    /// keyframe cadence (`0` = only the first and requested ones) and a
    /// screen-content hint.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::Backend`] if no encoder for `codec` can run.
    pub fn with_options(
        codec: StreamCodec,
        width: u32,
        height: u32,
        fps: u32,
        bitrate_bps: u64,
        keyframe_interval_frames: u32,
        screen_content: bool,
    ) -> Result<Self, EncodeError> {
        let mut config = EncoderConfig::new(codec, width, height, fps);
        config.bitrate_bps = bitrate_bps;
        config.keyframe_interval = keyframe_interval_frames;
        config.input = InputFormat::Rgba;
        config.screen_content = screen_content;
        Ok(Self {
            inner: RealtimeEncoder::new(&config)?,
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

    /// The bitstream this encoder writes.
    pub fn codec(&self) -> StreamCodec {
        self.inner.config().codec
    }

    /// The `GStreamer` element doing the work, for logs and diagnostics.
    pub fn element(&self) -> &'static media::realtime::CodecElement {
        self.inner.element()
    }

    /// Make the next frame a keyframe.
    pub fn force_keyframe(&mut self) {
        self.inner.force_keyframe();
    }

    /// Change the target bitrate while running.
    pub fn set_bitrate(&mut self, bits_per_second: u64) {
        self.inner.set_bitrate(bits_per_second);
    }

    /// Encodes one RGBA frame (row-major, no padding, `width * height * 4`
    /// bytes) and returns every access unit the encoder has finished, in
    /// order, with parameter sets on keyframes. Usually that is this frame's
    /// unit; a frame the encoder is still working on comes back with the
    /// next call (or [`Self::finish`]). Keep every unit: later ones
    /// reference it.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::BufferSize`] if `rgba` isn't exactly `width *
    /// height * 4` bytes, or [`EncodeError::Backend`] if the encoder fails.
    #[tracing::instrument(level = "trace", skip(self, rgba), fields(width = self.width, height = self.height))]
    pub fn encode(&mut self, rgba: &[u8]) -> Result<Vec<EncodedUnit>, EncodeError> {
        let ts_ms = self
            .frame_idx
            .saturating_mul(1000)
            .checked_div(u64::from(self.fps))
            .unwrap_or(0);
        self.frame_idx = self.frame_idx.saturating_add(1);
        self.encode_at_ms(rgba, ts_ms)
    }

    /// Encodes one RGBA frame with a caller-supplied millisecond timestamp.
    ///
    /// # Errors
    ///
    /// As [`Self::encode`].
    pub fn encode_at_ms(
        &mut self,
        rgba: &[u8],
        ts_ms: u64,
    ) -> Result<Vec<EncodedUnit>, EncodeError> {
        let expected = (self.width as usize)
            .saturating_mul(self.height as usize)
            .saturating_mul(4);
        if rgba.len() != expected {
            return Err(EncodeError::BufferSize {
                expected,
                got: rgba.len(),
            });
        }
        Ok(self
            .inner
            .encode(InputFrame::Packed(rgba), Duration::from_millis(ts_ms))?)
    }

    /// Ends the stream and returns the units of frames still in flight.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::Backend`] if the encoder fails to drain.
    pub fn finish(self) -> Result<Vec<EncodedUnit>, EncodeError> {
        Ok(self.inner.finish()?)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::decoder::VideoDecoder;
    use media::realtime::RealtimePolicy;

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

    /// Deterministic RGBA gradient — smooth in both axes for a fair
    /// intra-prediction test.
    pub(crate) fn gradient_frame(width: u32, height: u32) -> Vec<u8> {
        let capacity = width.saturating_mul(height).saturating_mul(4) as usize;
        let mut out = Vec::with_capacity(capacity);
        let channel = |numerator: u32, denominator: u32| -> u8 {
            let scaled = numerator
                .saturating_mul(200)
                .checked_div(denominator.max(1))
                .unwrap_or(0);
            u8::try_from(scaled.saturating_add(20)).unwrap_or(u8::MAX)
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

    /// The royalty-free software path: no GPU, no Cisco binary.
    fn software_encoder(codec: StreamCodec, w: u32, h: u32, interval: u32) -> VideoEncoder {
        let mut config = EncoderConfig::new(codec, w, h, 30);
        config.policy = RealtimePolicy::SoftwareOnly;
        config.keyframe_interval = interval;
        VideoEncoder {
            inner: RealtimeEncoder::new(&config).expect("royalty-free software encoder"),
            width: w,
            height: h,
            fps: 30,
            frame_idx: 0,
        }
    }

    /// A gradient round-trips at >= 35 dB PSNR, a mis-sized buffer is caught, and the configured
    /// keyframe interval emits periodic keyframes.
    /// Runs each phase below in order; every phase builds its own fixture.
    #[test]
    fn the_encoder_keeps_quality_checks_buffer_size_and_paces_keyframes() {
        gradient_round_trip_psnr_at_least_35_db();
        buffer_size_mismatch_is_caught();
        configured_keyframe_interval_emits_periodic_keyframes();
    }

    fn gradient_round_trip_psnr_at_least_35_db() {
        const W: u32 = 128;
        const H: u32 = 96;
        let original = gradient_frame(W, H);
        let mut enc = software_encoder(StreamCodec::Vp9, W, H, 0);
        let mut units = enc.encode(&original).expect("encode");
        units.extend(enc.finish().expect("finish"));
        let [unit] = units.as_slice() else {
            panic!("one frame in, one unit out: {}", units.len());
        };
        assert!(!unit.data.is_empty(), "encoder produced empty bitstream");
        let mut dec = VideoDecoder::new(StreamCodec::Vp9);
        let frame = dec.decode(&unit.data).expect("decoder yields a frame");
        assert_eq!((frame.width, frame.height), (W, H));
        let strip_alpha = |buf: &[u8]| -> Vec<u8> {
            buf.chunks_exact(4)
                .flat_map(|c| [c[0], c[1], c[2]])
                .collect()
        };
        let score = psnr(&strip_alpha(&original), &strip_alpha(&frame.rgba));
        assert!(
            score >= 35.0,
            "PSNR {score:.2} dB below 35.0 dB floor — encoder pipeline regression"
        );
    }

    fn buffer_size_mismatch_is_caught() {
        let mut enc = software_encoder(StreamCodec::Vp9, 32, 32, 0);
        let too_small = vec![0u8; 32 * 32 * 3]; // RGB, not RGBA
        let err = enc.encode(&too_small).unwrap_err();
        assert!(matches!(err, EncodeError::BufferSize { .. }));
    }

    fn configured_keyframe_interval_emits_periodic_keyframes() {
        const W: u32 = 64;
        const H: u32 = 64;
        const INTERVAL: u32 = 4;
        const FRAMES: u32 = INTERVAL * 2 + 1;
        let mut frame = gradient_frame(W, H);
        let mut enc = software_encoder(StreamCodec::Vp9, W, H, INTERVAL);
        let mut units = Vec::new();
        for frame_index in 0..FRAMES {
            // Change the picture so the encoder cannot skip it.
            frame[0] = u8::try_from(frame_index).unwrap();
            units.extend(
                enc.encode_at_ms(&frame, u64::from(frame_index) * 33)
                    .expect("encode"),
            );
        }
        units.extend(enc.finish().expect("finish"));
        assert_eq!(
            units.len(),
            usize::try_from(FRAMES).unwrap(),
            "one unit per frame"
        );
        let keyframes: Vec<u32> = (0..)
            .zip(&units)
            .filter(|(_, unit)| unit.keyframe)
            .map(|(index, _)| index)
            .collect();
        assert_eq!(keyframes, [0, INTERVAL, INTERVAL * 2]);
    }
}
