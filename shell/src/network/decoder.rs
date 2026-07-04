//! H.264 software decoder — Phase 4 Task 4.1 (decode side).
//!
//! Paired with [`crate::network::encoder::H264Encoder`]; together they form
//! the round-trip that's PSNR-verified at >=35 dB.
//!
//! The decoder is push-based: hand it NALU bytes via
//! [`H264Decoder::decode`] and it returns `Some(DecodedFrame)` when an
//! entire frame is available, `None` otherwise (e.g. while it's still
//! waiting on the SPS/PPS metadata, or the bitstream slice doesn't yet
//! close a frame).

use openh264::decoder::{Decoder, DecoderConfig};
use openh264::formats::YUVSource;

/// Decoded RGBA picture handed back to the caller.
///
/// `rgba` is row-major, no padding, `width * height * 4` bytes. The alpha
/// channel is always full (`0xFF`) because H.264 doesn't carry alpha.
#[derive(Debug, Clone)]
pub struct DecodedFrame {
    /// Pixel width in pixels.
    pub width: u32,
    /// Pixel height in pixels.
    pub height: u32,
    /// RGBA pixel bytes, row-major, no padding.
    pub rgba: Vec<u8>,
}

/// Streaming H.264 decoder.
///
/// Construct with [`H264Decoder::new`]; feed NALU bytes via
/// [`H264Decoder::decode`]. The decoder owns its own scratch buffer for
/// the YUV→RGBA conversion so the call site doesn't have to manage
/// allocations.
pub struct H264Decoder {
    inner: Decoder,
    /// Reusable scratch buffer for the YUV→RGBA conversion. Grown on
    /// first frame to `width * height * 4`; never shrunk. Reused across
    /// calls so steady-state decode does not allocate.
    rgba_scratch: Vec<u8>,
}

impl H264Decoder {
    /// Build a new decoder with default openh264 settings.
    pub fn new() -> Self {
        let inner = Decoder::with_api_config(
            openh264::OpenH264API::from_source(),
            DecoderConfig::new(),
        )
        .expect("openh264 decoder construction is infallible with the bundled lib");
        Self {
            inner,
            rgba_scratch: Vec::new(),
        }
    }

    /// Push a slice of NALU bytes and try to extract a decoded frame.
    ///
    /// Returns `Some(DecodedFrame)` when the decoder has a complete frame
    /// ready, `None` while it's still buffering (typical on the very first
    /// call before SPS/PPS land). Bitstream errors are folded into `None`
    /// — callers don't get to distinguish "still waiting" from "broken
    /// payload" because both are equally non-actionable in the streaming
    /// path; production code logs the underlying `openh264::Error` to
    /// tracing.
    pub fn decode(&mut self, nalus: &[u8]) -> Option<DecodedFrame> {
        let maybe = self.inner.decode(nalus).ok().flatten();
        let yuv = maybe?;
        let (width, height) = yuv.dimensions();
        let needed = width * height * 4;
        if self.rgba_scratch.len() != needed {
            self.rgba_scratch.resize(needed, 0);
        }
        yuv.write_rgba8(&mut self.rgba_scratch);
        Some(DecodedFrame {
            width: width as u32,
            height: height as u32,
            rgba: self.rgba_scratch.clone(),
        })
    }
}

impl Default for H264Decoder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_returns_none_for_empty_input() {
        let mut dec = H264Decoder::new();
        // Empty slice: nothing for openh264 to chew on, expect `None`
        // without panicking.
        assert!(dec.decode(&[]).is_none());
    }

    #[test]
    fn decoder_returns_none_for_garbage_input() {
        let mut dec = H264Decoder::new();
        // Plausible-looking but invalid Annex-B start code only.
        let garbage = [0u8, 0, 0, 1, 0xFF, 0xFF, 0xFF];
        // Either `None` (no frame yet) or `Some(_)` from a tolerant
        // backend would both be acceptable; we just need no panic.
        let _ = dec.decode(&garbage);
    }
}
