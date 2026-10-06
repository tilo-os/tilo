//! Video decoder — the receive-side half of the streaming stack.
//!
//! [`VideoDecoder`] turns access units into plain 8-bit RGBA a call tile can
//! blit straight into a `peniko::ImageData`. It wraps
//! [`media::RealtimeDecoder`], which picks a hardware decoder first. The
//! compositor's HDR receive path asks the media stack for P010 planes
//! instead; a call tile doesn't need them.

use media::realtime::{DecodeOutput, DecoderConfig, RealtimeDecoder, StreamCodec, StreamColor};

/// Largest decoded width accepted from a remote stream.
pub const MAX_DECODED_WIDTH: usize = 4096;
/// Largest decoded height accepted from a remote stream.
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

/// Validates remote dimensions before accepting a decoded RGBA image.
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

/// Streaming decoder for one codec. The media pipeline is started on the
/// first access unit, so constructing one (including through [`Default`],
/// which decodes H.264) is free and never fails; a machine that cannot
/// decode the codec yields no frames and logs why once.
pub struct VideoDecoder {
    codec: StreamCodec,
    display_size: Option<(u32, u32)>,
    inner: Option<RealtimeDecoder>,
    failed: bool,
}

impl std::fmt::Debug for VideoDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoDecoder")
            .field("codec", &self.codec)
            .field(
                "element",
                &self
                    .inner
                    .as_ref()
                    .and_then(RealtimeDecoder::element)
                    .map(|e| e.element),
            )
            .finish_non_exhaustive()
    }
}

impl VideoDecoder {
    pub fn new(codec: StreamCodec) -> Self {
        Self {
            codec,
            display_size: None,
            inner: None,
            failed: false,
        }
    }

    /// The codec this decoder expects.
    pub fn codec(&self) -> StreamCodec {
        self.codec
    }

    /// Crop decoded pictures to the size the sender announced; see
    /// [`media::DecoderConfig::display_size`].
    pub fn set_display_size(&mut self, size: Option<(u32, u32)>) {
        self.display_size = size;
        if let Some(d) = self.inner.as_mut() {
            d.set_display_size(size);
        }
    }

    /// Pushes one access unit and returns the picture, if one is ready.
    /// `None` means "need a keyframe first", "invalid bitstream", or "cannot
    /// decode this codec here" — indistinguishable to the caller, which just
    /// logs the packet as dropped.
    pub fn decode(&mut self, unit: &[u8]) -> Option<DecodedFrame> {
        self.decode_with_hint(unit, None)
    }

    /// [`Self::decode`] with the transport's keyframe flag.
    pub fn decode_with_hint(
        &mut self,
        unit: &[u8],
        keyframe: Option<bool>,
    ) -> Option<DecodedFrame> {
        if unit.is_empty() || self.failed {
            return None;
        }
        if self.inner.is_none() {
            let config = DecoderConfig {
                output: DecodeOutput::Rgba,
                display_size: self.display_size,
                ..DecoderConfig::new(self.codec)
            };
            match RealtimeDecoder::new(config) {
                Ok(mut d) => {
                    // Every call encoder writes `EncoderConfig`'s default
                    // colour. Say so rather than leave it to the decoder:
                    // not every encoder signals it and not every decoder
                    // reads it, and a guess shifts every colour of an SD
                    // stream.
                    d.set_stream_color(Some(StreamColor::BT709));
                    self.inner = Some(d);
                }
                Err(e) => {
                    tracing::warn!(codec = self.codec.wire_label(), "no video decoder: {e}");
                    self.failed = true;
                    return None;
                }
            }
        }
        let picture = match self.inner.as_mut()?.decode(unit, keyframe) {
            Ok(p) => p?,
            Err(e) => {
                tracing::warn!(
                    codec = self.codec.wire_label(),
                    "video decoder stopped: {e}"
                );
                self.failed = true;
                return None;
            }
        };
        let frame = picture.frame;
        // Dimensions come from the peer's bitstream: validate each bound.
        decoded_rgba_len(frame.width as usize, frame.height as usize)?;
        let rgba = frame.to_rgba()?;
        Some(DecodedFrame {
            width: frame.width,
            height: frame.height,
            rgba: std::sync::Arc::unwrap_or_clone(rgba),
        })
    }
}

impl Default for VideoDecoder {
    /// An H.264 decoder: the codec every peer of the legacy wire sends.
    fn default() -> Self {
        Self::new(StreamCodec::H264)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoder::VideoEncoder;

    /// Empty input yields no frame, an encoded frame decodes to its dimensions, and remote
    /// dimensions are checked.
    /// Runs each phase below in order; every phase builds its own fixture.
    #[test]
    fn the_decoder_yields_frames_only_for_real_input_of_checked_dimensions() {
        empty_input_yields_no_frame();
        encode_then_decode_round_trips_dimensions();
        remote_dimensions_are_checked();
    }

    fn empty_input_yields_no_frame() {
        let mut dec = VideoDecoder::new(StreamCodec::Vp9);
        assert!(dec.decode(&[]).is_none());
    }

    fn encode_then_decode_round_trips_dimensions() {
        const W: u32 = 64;
        const H: u32 = 48;
        let rgba = crate::encoder::tests::gradient_frame(W, H);
        let mut enc = VideoEncoder::with_options(StreamCodec::Vp9, W, H, 30, 1_000_000, 0, false)
            .expect("VP9 encoder");
        let mut units = enc.encode(&rgba).expect("encode");
        units.extend(enc.finish().expect("finish"));
        let unit = units.first().expect("one unit for the frame");
        let mut dec = VideoDecoder::new(StreamCodec::Vp9);
        let frame = dec.decode(&unit.data).expect("frame");
        assert_eq!((frame.width, frame.height), (W, H));
        assert_eq!(frame.rgba.len(), (W * H * 4) as usize);
    }

    fn remote_dimensions_are_checked() {
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
