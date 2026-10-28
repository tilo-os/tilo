//! Video decoder for peer-sync surface streaming.
//!
//! [`VideoDecoder`] wraps [`media::RealtimeDecoder`] (hardware first, then
//! royalty-free software, then Cisco's `openh264` for H.264) for every wire
//! codec: `"h264"`, `"hevc-main10"`, `"vp9"` and `"av1"`. SDR streams decode
//! to 8-bit RGBA the `PeerView` blit path consumes directly; 10-bit streams
//! decode to P010 planes for the receiver's tone-map without downcasting.
//!
//! The media pipeline starts on the first access unit, so constructing a
//! decoder never fails for a known codec; a machine that cannot decode the
//! codec yields no frames and logs why once. A decoder that never waits (the
//! compositor loop's) starts its element on a media thread, so that first
//! unit costs the caller no start-up either. The sender only picks a codec
//! every receiver advertised (see `media::realtime::negotiate`).

use media::realtime::{DecodeOutput, DecoderConfig, PictureWake, RealtimeDecoder, StreamColor};
use media::{DecodedPicture, RealtimeError};
use media::{FramePayload, PlanarFormat, StreamCodec};

use neighbour_wire::colorimetry::{
    ColorPrimaries, ColorRange, Colorimetry, MatrixCoefficients, TransferCharacteristics,
};

/// One decoded picture handed back to `PeerView`.
#[derive(Debug, Clone)]
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    /// The pixel payload — either 8-bit RGBA or 10-bit P010 planes.
    pub pixels: DecodedPixels,
    /// Colorimetry the decoder reported for the stream.
    pub colorimetry: Colorimetry,
}

/// Discriminated pixel data — the shape depends on the decoder output.
#[derive(Debug, Clone)]
pub enum DecodedPixels {
    /// 8-bit RGBA, row-major, no padding, `width * height * 4` bytes.
    Rgba8(Vec<u8>),
    /// 10-bit P010 planes. Each `u16` stores the sample in its upper bits;
    /// `y_stride`/`uv_stride` are in u16 units.
    P010 {
        y: Vec<u16>,
        /// Interleaved UV at half resolution in both axes (4:2:0).
        uv: Vec<u16>,
        y_stride: u32,
        uv_stride: u32,
    },
}

impl DecodedPixels {
    /// Convenience — is this an SDR RGBA frame?
    pub fn is_rgba(&self) -> bool {
        matches!(self, DecodedPixels::Rgba8(_))
    }

    /// Convenience — is this a 10-bit HDR planes frame?
    pub fn is_p010(&self) -> bool {
        matches!(self, DecodedPixels::P010 { .. })
    }
}

/// Why a codec label failed to construct a decoder.
#[derive(Debug)]
pub enum DecoderError {
    /// Wire label wasn't one of the known codecs.
    UnknownCodec(String),
}

impl std::fmt::Display for DecoderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecoderError::UnknownCodec(c) => {
                write!(f, "unknown codec label on the wire: {c:?}")
            }
        }
    }
}

impl std::error::Error for DecoderError {}

/// Streaming decoder for one shared surface.
pub struct VideoDecoder {
    codec: StreamCodec,
    output: DecodeOutput,
    display_size: Option<(u32, u32)>,
    inner: Option<RealtimeDecoder>,
    failed: bool,
    last_colorimetry: Option<Colorimetry>,
    /// The colorimetry the sender announced for the stream.
    stream_colorimetry: Option<Colorimetry>,
    /// How long a decode waits for its picture; `None` keeps the media
    /// decoder's own wait. See [`Self::set_picture_wait`].
    picture_wait: Option<std::time::Duration>,
    picture_wake: Option<PictureWake>,
}

impl VideoDecoder {
    /// A decoder for `codec`, producing RGBA (`ten_bit = false`) or P010.
    pub fn new(codec: StreamCodec, ten_bit: bool) -> Self {
        Self {
            codec,
            output: if ten_bit {
                DecodeOutput::P010
            } else {
                DecodeOutput::Rgba
            },
            display_size: None,
            inner: None,
            failed: false,
            last_colorimetry: None,
            stream_colorimetry: None,
            picture_wait: None,
            picture_wake: None,
        }
    }

    /// An SDR H.264 decoder, what a stream without a codec label carries.
    pub fn h264() -> Self {
        Self::new(StreamCodec::H264, false)
    }

    /// Pick a decoder from the wire label and announced bit depth
    /// (`SurfaceStreamStart.bit_depth`; `0` = infer from the codec).
    ///
    /// # Errors
    ///
    /// Returns [`DecoderError::UnknownCodec`] for an unrecognised label.
    pub fn for_stream(codec: &str, bit_depth: u8) -> Result<Self, DecoderError> {
        let parsed = StreamCodec::from_wire_label(codec)
            .ok_or_else(|| DecoderError::UnknownCodec(codec.to_string()))?;
        let ten_bit = match bit_depth {
            0 => parsed.bit_depth() == 10,
            depth => depth > 8,
        };
        Ok(Self::new(parsed, ten_bit))
    }

    /// The codec this decoder expects.
    pub fn codec(&self) -> StreamCodec {
        self.codec
    }

    /// Whether frames come out as 10-bit planes.
    pub fn is_ten_bit(&self) -> bool {
        self.output == DecodeOutput::P010
    }

    /// The colorimetry the sender announced (`SurfaceStreamStart`). It
    /// drives the conversion and is what decoded frames report: measured,
    /// `GStreamer`'s VA-API encoders write no colour description, so without
    /// it a decoder guesses from the picture size (BT.601 below HD) and a PQ
    /// stream decodes as SDR.
    pub fn set_stream_colorimetry(&mut self, colorimetry: Option<Colorimetry>) {
        self.stream_colorimetry = colorimetry;
        if let Some(d) = self.inner.as_mut() {
            d.set_stream_color(colorimetry.map(stream_color));
        }
    }

    /// Crop pictures to the size the sender announced
    /// (`SurfaceStreamStart.width`/`height`); some hardware encoders pad.
    pub fn set_display_size(&mut self, size: Option<(u32, u32)>) {
        self.display_size = size;
        if let Some(d) = self.inner.as_mut() {
            d.set_display_size(size);
        }
    }

    /// How long [`Self::decode`] waits for its unit's picture once the
    /// stream is running. A compositor loop sets zero: the unit is handed
    /// over and its picture comes out of a later [`Self::decode`] or
    /// [`Self::take_late_frame`] (see `RealtimeDecoder::set_picture_wait`).
    pub fn set_picture_wait(&mut self, wait: std::time::Duration) {
        self.picture_wait = Some(wait);
        if let Some(d) = self.inner.as_mut() {
            d.set_picture_wait(wait);
        }
    }

    /// Run `wake` on a media thread each time a picture comes out, so the
    /// loop collects it with [`Self::take_late_frame`] without polling.
    pub fn set_picture_wake(&mut self, wake: Option<PictureWake>) {
        self.picture_wake.clone_from(&wake);
        if let Some(d) = self.inner.as_mut() {
            d.set_picture_wake(wake);
        }
    }

    /// Push one access unit; returns the decoded frame if a picture is
    /// ready. `None` covers "waiting for a keyframe", corrupt input and "no
    /// decoder for this codec here" alike — callers log the packet as dropped
    /// — unless [`Self::picture_pending`] says the unit is still decoding.
    pub fn decode(&mut self, unit: &[u8]) -> Option<DecodedFrame> {
        if unit.is_empty() || self.failed {
            return None;
        }
        if self.inner.is_none() {
            let config = DecoderConfig {
                output: self.output,
                display_size: self.display_size,
                ..DecoderConfig::new(self.codec)
            };
            // A caller that must never wait does not wait for the element to
            // start either: it starts on a media thread, and the wake says
            // when it is up.
            let started = if self.picture_wait.is_some_and(|wait| wait.is_zero()) {
                RealtimeDecoder::new_without_waiting(config)
            } else {
                RealtimeDecoder::new(config)
            };
            match started {
                Ok(mut d) => {
                    d.set_stream_color(self.stream_colorimetry.map(stream_color));
                    if let Some(wait) = self.picture_wait {
                        d.set_picture_wait(wait);
                    }
                    d.set_picture_wake(self.picture_wake.clone());
                    self.inner = Some(d);
                }
                Err(e) => {
                    tracing::warn!(codec = self.codec.wire_label(), "no video decoder: {e}");
                    self.failed = true;
                    return None;
                }
            }
        }
        let picture = self.inner.as_mut()?.decode(unit, None);
        self.frame_from(picture)
    }

    /// Whether an accepted unit's picture is still being decoded: it took
    /// longer than the decoder's wait and arrives with a later
    /// [`Self::decode`] or [`Self::take_late_frame`]. A slow picture, not a
    /// rejected unit.
    pub fn picture_pending(&self) -> bool {
        !self.failed
            && self
                .inner
                .as_ref()
                .is_some_and(RealtimeDecoder::picture_pending)
    }

    /// When a decoder that never waits gives up on an element that has shown
    /// no picture (see `RealtimeDecoder::startup_deadline`). Nothing else
    /// announces that moment, so a caller wakes for it and collects with
    /// [`Self::take_late_frame`].
    pub fn startup_deadline(&self) -> Option<std::time::Instant> {
        if self.failed {
            return None;
        }
        self.inner
            .as_ref()
            .and_then(RealtimeDecoder::startup_deadline)
    }

    /// The newest picture that finished after its `decode` returned, without
    /// waiting or decoding anything new.
    pub fn take_late_frame(&mut self) -> Option<DecodedFrame> {
        if !self.picture_pending() {
            return None;
        }
        let picture = self.inner.as_mut()?.take_late_picture();
        self.frame_from(picture)
    }

    fn frame_from(
        &mut self,
        picture: Result<Option<DecodedPicture>, RealtimeError>,
    ) -> Option<DecodedFrame> {
        let picture = match picture {
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
        let colorimetry = colorimetry_from(picture.color);
        self.last_colorimetry = Some(colorimetry);
        let frame = picture.frame;
        let pixels = match &frame.payload {
            FramePayload::Planar(image) if image.format == PlanarFormat::P010 => {
                let [y_plane, uv_plane] = image.planes.as_slice() else {
                    return None;
                };
                DecodedPixels::P010 {
                    y: u16_samples(&y_plane.bytes),
                    uv: u16_samples(&uv_plane.bytes),
                    y_stride: y_plane.stride / 2,
                    uv_stride: uv_plane.stride / 2,
                }
            }
            _ => DecodedPixels::Rgba8(std::sync::Arc::unwrap_or_clone(frame.to_rgba()?)),
        };
        Some(DecodedFrame {
            width: frame.width,
            height: frame.height,
            pixels,
            colorimetry,
        })
    }

    /// Colorimetry recorded from the most recent decoded frame; `None`
    /// until the first frame arrives.
    pub fn last_colorimetry(&self) -> Option<Colorimetry> {
        self.last_colorimetry
    }
}

impl Default for VideoDecoder {
    fn default() -> Self {
        Self::h264()
    }
}

/// Little-endian 16-bit samples of a P010 plane (samples in the high bits).
fn u16_samples(bytes: &[u8]) -> Vec<u16> {
    bytes
        .chunks_exact(2)
        .map(|pair| match pair {
            &[a, b] => u16::from_le_bytes([a, b]),
            _ => 0,
        })
        .collect()
}

/// The wire's colorimetry as CICP code points.
fn stream_color(c: Colorimetry) -> StreamColor {
    StreamColor {
        primaries: c.primaries.cicp(),
        transfer: c.transfer.cicp(),
        matrix: c.matrix.cicp(),
        full_range: c.range == ColorRange::Full,
    }
}

/// Map the decoder's CICP code points onto the wire's colorimetry.
pub fn colorimetry_from(color: StreamColor) -> Colorimetry {
    Colorimetry {
        primaries: match color.primaries {
            1 => ColorPrimaries::Bt709,
            9 => ColorPrimaries::Bt2020,
            12 => ColorPrimaries::DisplayP3,
            _ => ColorPrimaries::Unspecified,
        },
        transfer: match color.transfer {
            1 | 6 | 14 | 15 => TransferCharacteristics::Bt709,
            13 => TransferCharacteristics::Srgb,
            16 => TransferCharacteristics::SmpteSt2084,
            18 => TransferCharacteristics::AribStdB67,
            _ => TransferCharacteristics::Unspecified,
        },
        matrix: match color.matrix {
            0 => MatrixCoefficients::Identity,
            1 => MatrixCoefficients::Bt709,
            9 | 10 => MatrixCoefficients::Bt2020NonConstant,
            _ => MatrixCoefficients::Unspecified,
        },
        range: if color.full_range {
            ColorRange::Full
        } else {
            ColorRange::Limited
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoder_output_shapes_for_empty_and_decoded_input() {
        {
            let mut dec = VideoDecoder::h264();
            assert!(dec.decode(&[]).is_none());
        }
        {
            let rgba = DecodedPixels::Rgba8(vec![0u8; 4]);
            assert!(rgba.is_rgba());
            assert!(!rgba.is_p010());
            let p010 = DecodedPixels::P010 {
                y: vec![0u16; 16],
                uv: vec![0u16; 8],
                y_stride: 4,
                uv_stride: 4,
            };
            assert!(p010.is_p010());
        }
    }

    #[test]
    fn wire_labels_and_cicp_select_codec_output_and_colorimetry() {
        {
            let d = VideoDecoder::for_stream("h264", 0).expect("h264");
            assert_eq!(d.codec(), StreamCodec::H264);
            assert!(!d.is_ten_bit());
            let d = VideoDecoder::for_stream("hevc-main10", 0).expect("hevc");
            assert!(d.is_ten_bit(), "HEVC Main10 decodes to P010");
            let d = VideoDecoder::for_stream("av1", 10).expect("av1");
            assert!(d.is_ten_bit(), "a 10-bit AV1 stream decodes to P010");
            let d = VideoDecoder::for_stream("vp9", 0).expect("vp9");
            assert!(!d.is_ten_bit());
            match VideoDecoder::for_stream("h266", 0) {
                Err(DecoderError::UnknownCodec(label)) => assert_eq!(label, "h266"),
                Ok(_) => panic!("an unknown label must not build a decoder"),
            }
        }
        {
            assert_eq!(
                colorimetry_from(StreamColor::BT709),
                Colorimetry::sdr_bt709()
            );
            for c in [
                Colorimetry::sdr_bt709(),
                Colorimetry::hdr10(),
                Colorimetry::hdr_hlg(),
            ] {
                assert_eq!(colorimetry_from(stream_color(c)), c, "round trip {c:?}");
            }
            let pq = colorimetry_from(StreamColor::BT2100_PQ);
            assert_eq!(pq.transfer, TransferCharacteristics::SmpteSt2084);
            assert_eq!(pq.primaries, ColorPrimaries::Bt2020);
            assert_eq!(pq.matrix, MatrixCoefficients::Bt2020NonConstant);
            assert!(pq.is_hdr());
        }
        {
            let mut dec = VideoDecoder::new(StreamCodec::Av1, true);
            dec.set_stream_colorimetry(Some(Colorimetry::hdr10()));
            assert_eq!(dec.stream_colorimetry, Some(Colorimetry::hdr10()));
        }
    }
}
