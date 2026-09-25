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

/// Largest decoded width accepted from a remote SPS.
pub const MAX_DECODED_WIDTH: usize = 4096;
/// Largest decoded height accepted from a remote SPS.
pub const MAX_DECODED_HEIGHT: usize = 2160;
/// Largest decoded image in pixels (a 4K UHD frame).
pub const MAX_DECODED_PIXELS: usize = MAX_DECODED_WIDTH * MAX_DECODED_HEIGHT;
/// Largest RGBA allocation made for one decoded frame.
pub const MAX_DECODED_RGBA_BYTES: usize = MAX_DECODED_PIXELS * 4;

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
}

/// Validates remote SPS dimensions before allocating a decoded RGBA image.
fn decoded_rgba_len(width: usize, height: usize) -> Option<usize> {
    if width == 0 || height == 0 || width > MAX_DECODED_WIDTH || height > MAX_DECODED_HEIGHT {
        return None;
    }
    let pixels = width.checked_mul(height)?;
    if pixels > MAX_DECODED_PIXELS {
        return None;
    }
    let needed = pixels.checked_mul(4)?;
    (needed <= MAX_DECODED_RGBA_BYTES).then_some(needed)
}

impl H264Decoder {
    /// # Panics
    ///
    /// Panics if the bundled openh264 backend fails to initialise. This is
    /// not reachable in practice with the vendored library, but the
    /// underlying `with_api_config` call is fallible; production code
    /// (the chat app's per-peer `remote_decoders`/`remote_screen_rx` maps)
    /// relies on [`H264Decoder`]/[`crate::session::VideoStreamReceiver`]
    /// staying [`Default`]-constructible via `HashMap::entry().or_default()`,
    /// so this stays infallible rather than returning `Result`.
    #[allow(
        clippy::expect_used,
        reason = "construction from the bundled openh264 library never fails in practice; \
                  keeping this infallible is required for the Default-based \
                  entry().or_default() pattern production code depends on"
    )]
    pub fn new() -> Self {
        let inner =
            Decoder::with_api_config(openh264::OpenH264API::from_source(), DecoderConfig::new())
                .expect("openh264 decoder construction is infallible with the bundled lib");
        Self { inner }
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
        // `width`/`height` come from the peer's SPS via openh264 — remote
        // input. Validate each independent bound and use checked arithmetic
        // before reserving. `try_reserve_exact` turns allocation failure into
        // a dropped frame instead of retaining or returning stale pixels.
        let needed = decoded_rgba_len(width, height)?;
        let mut rgba = Vec::new();
        rgba.try_reserve_exact(needed).ok()?;
        rgba.resize(needed, 0);
        yuv.write_rgba8(&mut rgba);
        Some(DecodedFrame {
            width: u32::try_from(width).ok()?,
            height: u32::try_from(height).ok()?,
            rgba,
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
                rgba.push(u8::try_from((x * 255) / W).unwrap_or(u8::MAX));
                rgba.push(u8::try_from((y * 255) / H).unwrap_or(u8::MAX));
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

    #[test]
    fn remote_sps_dimensions_are_checked_before_rgba_allocation() {
        assert_eq!(
            decoded_rgba_len(MAX_DECODED_WIDTH, MAX_DECODED_HEIGHT),
            Some(MAX_DECODED_RGBA_BYTES)
        );
        assert_eq!(decoded_rgba_len(0, 1), None);
        assert_eq!(decoded_rgba_len(MAX_DECODED_WIDTH + 1, 1), None);
        assert_eq!(decoded_rgba_len(1, MAX_DECODED_HEIGHT + 1), None);
        assert_eq!(decoded_rgba_len(usize::MAX, usize::MAX), None);
    }
}
