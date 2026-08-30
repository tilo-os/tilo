//! SDR H.264 decoder — the receive-side half of the streaming stack.
//!
//! Wraps openh264 into plain 8-bit RGBA, the same Annex-B→RGBA path the
//! compositor's `network::decoder::H264Decoder` runs for SDR streams —
//! kept standalone and colorimetry-free so a receiver (a chat call tile)
//! can blit straight into a `peniko::ImageData`. HDR/HEVC Main10 stays in
//! the compositor (ffmpeg-next behind a feature flag, P010 for a
//! tone-map shader); a call tile doesn't need it.

use openh264::decoder::{Decoder, DecoderConfig};
use openh264::formats::YUVSource;

/// One decoded picture: tightly-packed 8-bit RGBA plus dimensions.
/// `rgba` is row-major with no padding — ready to upload straight into a
/// GPU image / `peniko` blob.
#[derive(Clone, Debug)]
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Streaming H.264 decoder, SDR path. openh264 clamps colorimetry to
/// BT.709 + limited range internally, so the RGBA returned is already
/// display-ready sRGB-ish.
pub struct H264Decoder {
    inner: Decoder,
    rgba_scratch: Vec<u8>,
}

impl H264Decoder {
    pub fn new() -> Self {
        let inner =
            Decoder::with_api_config(openh264::OpenH264API::from_source(), DecoderConfig::new())
                .expect("openh264 decoder construction is infallible with the bundled lib");
        Self {
            inner,
            rgba_scratch: Vec::new(),
        }
    }

    /// Pushes an Annex-B access unit and tries to extract a decoded frame.
    /// `None` means "need more data" or "invalid bitstream" —
    /// indistinguishable to the caller, which just logs the packet as dropped.
    pub fn decode(&mut self, nalus: &[u8]) -> Option<DecodedFrame> {
        if nalus.is_empty() {
            return None;
        }
        let yuv = self.inner.decode(nalus).ok().flatten()?;
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
    use crate::encoder::H264Encoder;

    #[test]
    fn empty_input_yields_no_frame() {
        let mut dec = H264Decoder::new();
        assert!(dec.decode(&[]).is_none());
    }

    #[test]
    fn encode_then_decode_round_trips_dimensions() {
        const W: u32 = 64;
        const H: u32 = 48;
        let mut rgba = Vec::with_capacity((W * H * 4) as usize);
        for y in 0..H {
            for x in 0..W {
                rgba.push(((x * 255) / W) as u8);
                rgba.push(((y * 255) / H) as u8);
                rgba.push(0x40);
                rgba.push(0xFF);
            }
        }
        let mut enc = H264Encoder::new(W, H, 30).expect("encoder");
        let nalus = enc.encode(&rgba).expect("encode");
        let mut dec = H264Decoder::new();
        let frame = dec.decode(&nalus).expect("frame");
        assert_eq!((frame.width, frame.height), (W, H));
        assert_eq!(frame.rgba.len(), (W * H * 4) as usize);
    }
}
