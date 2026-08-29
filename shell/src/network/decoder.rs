//! Video decoders for peer-sync surface streaming.
//!
//! [`H264Decoder`] — pure-software openh264 for the SDR pipeline, producing
//! 8-bit RGBA the `PeerView` blit path consumes directly. [`HevcDecoder`] —
//! libavcodec software HEVC Main10 for the HDR pipeline, producing P010
//! planes for the receiver's tone-map compute shader without downcasting
//! to 8-bit.
//!
//! The peer view picks its decoder at construction from the
//! `SurfaceStreamStart.codec` wire label (`"h264"` / `"hevc-main10"`); the
//! [`VideoDecoder`] enum wraps both so `PeerView` doesn't fork its
//! receive-side plumbing.
//!
//! The HEVC path is behind the `hdr-decode` cargo feature (default-on); a
//! build without it accepts SDR normally and rejects HEVC with a clear
//! error.
//!
//! **Compiling the feature is not the same as having the codec.**
//! Whether the linked libavcodec carries an HEVC decoder is a property of
//! the distro's build: Fedora's `libavcodec-free` is configured
//! `--disable-decoder='h264,hevc,vc1,vvc'`, so [`HevcDecoder::new`] fails
//! there regardless of cargo features. `PeerView::new_for_stream` falls
//! back to H.264 in that case, and the sender never emits HEVC without a
//! hardware encoder, so HDR share is an enhancement gated on both ends, never a baseline dependency.
use openh264::decoder::{Decoder, DecoderConfig};
use openh264::formats::YUVSource;

use crate::collab::colorimetry::{
    ColorPrimaries, ColorRange, Colorimetry, MatrixCoefficients, TransferCharacteristics,
};

/// One decoded picture handed back to `PeerView`.
///
/// Two shapes: the SDR path yields already-converted RGBA (openh264 does
/// the Y'CbCr→RGB matrix internally), the HDR path yields P010 planes so the tone-map shader preserves 10-bit precision.
#[derive(Debug, Clone)]
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    /// The pixel payload — either 8-bit RGBA or 10-bit P010 planes.
    pub pixels: DecodedPixels,
    /// Colorimetry read off the decoder's per-frame side data. Typically
    /// matches the wire-level `Colorimetry` hint for H.264; for HEVC HDR it's
    /// authoritative, since streams often tag the VUI with the real transfer even when the container hint is legacy-default.
    pub colorimetry: Colorimetry,
}

/// Discriminated pixel data — the shape depends on the codec that produced it.
#[derive(Debug, Clone)]
pub enum DecodedPixels {
    /// 8-bit sRGB / BT.709 RGBA, row-major, no padding, `width * height * 4` bytes.
    Rgba8(Vec<u8>),
    /// 10-bit P010 planes. Each `u16` stores the sample in its upper bits
    /// (matches libavcodec's `AV_PIX_FMT_P010LE`); `y_stride`/`uv_stride` are in u16 units.
    P010 {
        y: Vec<u16>,
        /// Interleaved UV: two samples per pixel-pair (U then V), half-resolution horizontally + vertically (4:2:0).
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

/// The two decoders wrapped for uniform push/pull from `PeerView`.
///
/// Constructed via [`Self::for_wire_codec`] using the
/// `SurfaceStreamStart.codec` label, so an SDR peer never spins up the HEVC path and vice-versa.
pub enum VideoDecoder {
    /// H.264 8-bit via openh264. Wire label `"h264"`.
    H264(H264Decoder),
    /// HEVC Main10 10-bit via libavcodec. Wire label `"hevc-main10"`; only present in builds with the `hdr-decode` cargo feature.
    #[cfg(feature = "hdr-decode")]
    Hevc(HevcDecoder),
}

/// Why a codec label failed to construct a decoder.
#[derive(Debug)]
pub enum DecoderError {
    /// Wire label wasn't one of the known codecs.
    UnknownCodec(String),
    /// The build was compiled without the feature that would supply this decoder (typically HEVC without `hdr-decode`).
    FeatureDisabled { codec: String, reason: String },
    /// Backend failed to initialise (typically HEVC when libavcodec isn't linked against the decoder).
    Backend(String),
}

impl std::fmt::Display for DecoderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecoderError::UnknownCodec(c) => {
                write!(f, "unknown codec label on the wire: {c:?}")
            }
            DecoderError::FeatureDisabled { codec, reason } => {
                write!(f, "codec {codec} not compiled into this build: {reason}")
            }
            DecoderError::Backend(msg) => write!(f, "decoder backend failed: {msg}"),
        }
    }
}

impl std::error::Error for DecoderError {}

impl VideoDecoder {
    /// Pick a decoder from the wire label. Returns
    /// [`DecoderError::UnknownCodec`] for an unrecognised string,
    /// [`DecoderError::FeatureDisabled`] when the build lacks the required feature.
    pub fn for_wire_codec(codec: &str) -> Result<Self, DecoderError> {
        match codec {
            "h264" => Ok(VideoDecoder::H264(H264Decoder::new())),
            "hevc-main10" => {
                #[cfg(feature = "hdr-decode")]
                {
                    HevcDecoder::new()
                        .map(VideoDecoder::Hevc)
                        .map_err(DecoderError::Backend)
                }
                #[cfg(not(feature = "hdr-decode"))]
                {
                    Err(DecoderError::FeatureDisabled {
                        codec: codec.to_string(),
                        reason:
                            "`hdr-decode` feature not enabled — libavcodec HEVC decoder unavailable"
                                .to_string(),
                    })
                }
            }
            other => Err(DecoderError::UnknownCodec(other.to_string())),
        }
    }

    /// Push NALU bytes; returns the decoded frame if a picture is ready.
    /// Failure returns `None` — callers log the packet as dropped without distinguishing "buffered, waiting on keyframe" from "corrupt".
    pub fn decode(&mut self, nalus: &[u8]) -> Option<DecodedFrame> {
        match self {
            VideoDecoder::H264(d) => d.decode(nalus),
            #[cfg(feature = "hdr-decode")]
            VideoDecoder::Hevc(d) => d.decode(nalus),
        }
    }

    /// Colorimetry recorded from the most recent decoded frame. The wire
    /// `SurfaceStreamStart.colorimetry` is authoritative for session intent,
    /// but the decoder's own VUI read can catch drift (e.g. a legacy peer emitting `codec: "h264"` with BT.2020 VUI).
    pub fn last_colorimetry(&self) -> Option<Colorimetry> {
        match self {
            VideoDecoder::H264(d) => d.last_colorimetry(),
            #[cfg(feature = "hdr-decode")]
            VideoDecoder::Hevc(d) => d.last_colorimetry(),
        }
    }
}

/// Streaming H.264 decoder — the SDR path.
///
/// Wraps openh264 and yields RGBA. openh264 clamps colorimetry to BT.709
/// + limited range (no HDR VUIs), so [`Self::last_colorimetry`] always returns [`Colorimetry::sdr_bt709`] once a frame has decoded.
pub struct H264Decoder {
    inner: Decoder,
    rgba_scratch: Vec<u8>,
    /// Sticky colorimetry once the first frame lands; `None` before then so
    /// peers can distinguish "no frame yet" from "decoded but SDR".
    last_colorimetry: Option<Colorimetry>,
}

impl H264Decoder {
    pub fn new() -> Self {
        let inner =
            Decoder::with_api_config(openh264::OpenH264API::from_source(), DecoderConfig::new())
                .expect("openh264 decoder construction is infallible with the bundled lib");
        Self {
            inner,
            rgba_scratch: Vec::new(),
            last_colorimetry: None,
        }
    }

    /// Push NALU bytes and try to extract a decoded frame. Failures fold into `None` — see the module doc for why.
    pub fn decode(&mut self, nalus: &[u8]) -> Option<DecodedFrame> {
        let maybe = self.inner.decode(nalus).ok().flatten();
        let yuv = maybe?;
        let (width, height) = yuv.dimensions();
        let needed = width * height * 4;
        if self.rgba_scratch.len() != needed {
            self.rgba_scratch.resize(needed, 0);
        }
        yuv.write_rgba8(&mut self.rgba_scratch);
        self.last_colorimetry = Some(Colorimetry::sdr_bt709());
        Some(DecodedFrame {
            width: width as u32,
            height: height as u32,
            pixels: DecodedPixels::Rgba8(self.rgba_scratch.clone()),
            colorimetry: Colorimetry::sdr_bt709(),
        })
    }

    /// Colorimetry of the most recent decoded frame; `None` until the first frame arrives.
    pub fn last_colorimetry(&self) -> Option<Colorimetry> {
        self.last_colorimetry
    }
}

impl Default for H264Decoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Streaming HEVC Main10 decoder — the HDR path.
///
/// Wraps libavcodec's software HEVC decoder and yields P010 planes so the
/// receiver's tone-map shader (or direct `Rgb10a2Unorm` upload) preserves 10-bit precision. Behind the `hdr-decode` cargo feature.
#[cfg(feature = "hdr-decode")]
pub struct HevcDecoder {
    ctx: ffmpeg_next::codec::decoder::Video,
    /// Sticky colorimetry read off the `AVFrame`'s `color_*` fields on the last successful decode.
    last_colorimetry: Option<Colorimetry>,
}

#[cfg(feature = "hdr-decode")]
static FFMPEG_INIT: std::sync::Once = std::sync::Once::new();

#[cfg(feature = "hdr-decode")]
impl HevcDecoder {
    /// Construct a new HEVC decoder. Errors with the libavcodec message when
    /// the linked libavcodec has no HEVC decoder.
    ///
    /// That is the **expected** outcome on a stock qOS image: Fedora's
    /// `libavcodec-free` is configured `--disable-decoder='h264,hevc,vc1,vvc'`
    /// and carries no `hevc` entry at all. Only a host that replaced it
    /// (`libavcodec-freeworld`, or a self-built ffmpeg) can construct this
    /// decoder. Callers must treat the error as routine and fall back to SDR rather than dropping the stream.
    pub fn new() -> Result<Self, String> {
        FFMPEG_INIT.call_once(|| {
            let _ = ffmpeg_next::init();
        });
        let codec = ffmpeg_next::codec::decoder::find(ffmpeg_next::codec::Id::HEVC)
            .ok_or_else(|| "libavcodec has no HEVC decoder".to_string())?;
        let context = ffmpeg_next::codec::Context::new();
        let decoder = context
            .decoder()
            .open_as(codec)
            .map_err(|e| format!("avcodec_open2(HEVC): {e}"))?;
        let video = decoder
            .video()
            .map_err(|e| format!("HEVC decoder not video: {e}"))?;
        Ok(Self {
            ctx: video,
            last_colorimetry: None,
        })
    }

    /// Push NALU bytes; return the decoded frame if one is ready. Same
    /// semantics as [`H264Decoder::decode`] — failures fold into `None`.
    pub fn decode(&mut self, nalus: &[u8]) -> Option<DecodedFrame> {
        let packet = ffmpeg_next::packet::Packet::copy(nalus);
        if self.ctx.send_packet(&packet).is_err() {
            return None;
        }
        let mut frame = ffmpeg_next::util::frame::Video::empty();
        if self.ctx.receive_frame(&mut frame).is_err() {
            return None;
        }
        let width = frame.width();
        let height = frame.height();
        // Read colorimetry off the frame side-data, mapped from libavcodec's enums to our own CICP-aligned enums.
        let colorimetry = colorimetry_from_frame(&frame);
        self.last_colorimetry = Some(colorimetry);

        // Grab P010 planes. Frame format may also be YUV420P10LE if the SW
        // decoder chose that — both are 10-bit-in-16-bit but the layout differs
        // (P010 = Y + interleaved UV; YUV420P10 = Y + U + V separate); handle both by re-packing YUV420P10 into P010-shaped UV.
        let (y, uv, y_stride, uv_stride) = match frame.format() {
            ffmpeg_next::util::format::Pixel::P010LE => {
                let y_stride_bytes = frame.stride(0);
                let uv_stride_bytes = frame.stride(1);
                let y_plane = plane_u16_from_bytes(frame.data(0), y_stride_bytes);
                let uv_plane = plane_u16_from_bytes(frame.data(1), uv_stride_bytes);
                (
                    y_plane,
                    uv_plane,
                    (y_stride_bytes / 2) as u32,
                    (uv_stride_bytes / 2) as u32,
                )
            }
            ffmpeg_next::util::format::Pixel::YUV420P10LE => {
                // Re-pack: interleave U and V into UV. libavcodec returns U in plane 1, V in plane 2, each half-width.
                let y_stride_bytes = frame.stride(0);
                let u_stride_bytes = frame.stride(1);
                let v_stride_bytes = frame.stride(2);
                let y_plane = plane_u16_from_bytes(frame.data(0), y_stride_bytes);
                let u_bytes = frame.data(1);
                let v_bytes = frame.data(2);
                let uv_height = frame.plane_height(1) as usize;
                let uv_width = frame.plane_width(1) as usize;
                let mut uv = Vec::with_capacity(uv_width * 2 * uv_height);
                for row in 0..uv_height {
                    let u_row_off = row * u_stride_bytes;
                    let v_row_off = row * v_stride_bytes;
                    for col in 0..uv_width {
                        let u_off = u_row_off + col * 2;
                        let v_off = v_row_off + col * 2;
                        let u_val = u16::from_le_bytes([u_bytes[u_off], u_bytes[u_off + 1]]);
                        let v_val = u16::from_le_bytes([v_bytes[v_off], v_bytes[v_off + 1]]);
                        // P010 stores samples in the upper 10 bits of the u16; YUV420P10LE
                        // stores them in the lower 10 bits — shift on re-pack so downstream shaders always see the same upper-bit layout.
                        uv.push(u_val << 6);
                        uv.push(v_val << 6);
                    }
                }
                // Same upper-bit shift for the Y plane.
                let mut y_shifted = y_plane;
                for sample in &mut y_shifted {
                    *sample <<= 6;
                }
                let uv_stride_u16 = (uv_width * 2) as u32;
                (y_shifted, uv, (y_stride_bytes / 2) as u32, uv_stride_u16)
            }
            _other => {
                // Unexpected pixel format — drop the frame rather than downcast to 8-bit silently (defeats the whole reason for HDR).
                return None;
            }
        };
        Some(DecodedFrame {
            width,
            height,
            pixels: DecodedPixels::P010 {
                y,
                uv,
                y_stride,
                uv_stride,
            },
            colorimetry,
        })
    }

    /// Colorimetry from the most recent frame's VUI + side-data.
    pub fn last_colorimetry(&self) -> Option<Colorimetry> {
        self.last_colorimetry
    }
}

/// Copy a byte-strided plane into a `u16` Vec (native LE-encoded).
#[cfg(feature = "hdr-decode")]
fn plane_u16_from_bytes(bytes: &[u8], stride_bytes: usize) -> Vec<u16> {
    let mut out = Vec::with_capacity(bytes.len() / 2);
    // bytes.len() is stride_bytes * plane_height; iterate as pairs. The
    // stride includes libavcodec's per-row padding, which the receiver's y_stride/uv_stride preserve but don't read as samples.
    let _ = stride_bytes; // only referenced for clarity — length already accounts for it
    for pair in bytes.chunks_exact(2) {
        out.push(u16::from_le_bytes([pair[0], pair[1]]));
    }
    // Handle odd-length tail (shouldn't happen for a well-formed P010 buffer, but be defensive).
    if bytes.len() % 2 == 1 {
        out.push(u16::from(bytes[bytes.len() - 1]));
    }
    out
}

/// Translate libavcodec's `color_*` enums (BT2020, SMPTE2084, BT2020NCL) into our CICP-aligned [`Colorimetry`].
#[cfg(feature = "hdr-decode")]
fn colorimetry_from_frame(frame: &ffmpeg_next::util::frame::Video) -> Colorimetry {
    use ffmpeg_next::util::color as avc;

    let primaries = match frame.color_primaries() {
        avc::Primaries::BT2020 => ColorPrimaries::Bt2020,
        avc::Primaries::BT709 => ColorPrimaries::Bt709,
        avc::Primaries::SMPTE432 => ColorPrimaries::DisplayP3,
        _ => ColorPrimaries::Unspecified,
    };
    let transfer = match frame.color_transfer_characteristic() {
        avc::TransferCharacteristic::SMPTE2084 => TransferCharacteristics::SmpteSt2084,
        avc::TransferCharacteristic::ARIB_STD_B67 => TransferCharacteristics::AribStdB67,
        avc::TransferCharacteristic::BT709 => TransferCharacteristics::Bt709,
        avc::TransferCharacteristic::IEC61966_2_1 => TransferCharacteristics::Srgb,
        _ => {
            // Absent transfer characteristics on an HEVC stream: assume PQ, since that's what HDR10 content signals when the decoder can't read the VUI.
            TransferCharacteristics::SmpteSt2084
        }
    };
    let matrix = match frame.color_space() {
        avc::Space::BT2020NCL | avc::Space::BT2020CL => MatrixCoefficients::Bt2020NonConstant,
        avc::Space::BT709 => MatrixCoefficients::Bt709,
        avc::Space::RGB => MatrixCoefficients::Identity,
        _ => MatrixCoefficients::Unspecified,
    };
    let range = match frame.color_range() {
        avc::Range::JPEG => ColorRange::Full,
        _ => ColorRange::Limited,
    };
    Colorimetry {
        primaries,
        transfer,
        matrix,
        range,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h264_decoder_returns_none_for_empty_input() {
        let mut dec = H264Decoder::new();
        assert!(dec.decode(&[]).is_none());
    }

    #[test]
    fn h264_decoder_returns_none_for_garbage_input() {
        let mut dec = H264Decoder::new();
        let garbage = [0u8, 0, 0, 1, 0xFF, 0xFF, 0xFF];
        let _ = dec.decode(&garbage);
    }

    #[test]
    fn video_decoder_dispatches_on_wire_label() {
        // "h264" always works — the SDR path is always compiled and is the only
        // codec a default build can encode, so a receiver that can't build it can't view any share at all.
        let d = VideoDecoder::for_wire_codec("h264").expect("h264 available");
        assert!(matches!(d, VideoDecoder::H264(_)));

        // Unknown labels always error, and say which label they choked on.
        let e = VideoDecoder::for_wire_codec("av1").err().expect("unknown");
        match e {
            DecoderError::UnknownCodec(label) => assert_eq!(label, "av1"),
            other => panic!("an unrecognised label must be UnknownCodec, got {other:?}"),
        }
    }

    /// `"hevc-main10"` is a label this build *knows*, so the only legal
    /// answers are the HEVC decoder or a backend failure — never
    /// `UnknownCodec` (arm dropped) and never `FeatureDisabled` (feature
    /// compiled in). Both happen in practice: whether libavcodec carries HEVC
    /// is the distro's choice, and Fedora's `libavcodec-free` — what qOS
    /// builds against — disables it.
    ///
    /// Asserting success here would assert a property of whoever's ffmpeg
    /// happened to be installed, not of qOS; the actual promise is asserted
    /// on the production receive path in `shell::shell::peer_view::tests`, which holds either way.
    #[cfg(feature = "hdr-decode")]
    #[test]
    fn hevc_label_is_known_and_never_misreports_why_it_failed() {
        match VideoDecoder::for_wire_codec("hevc-main10") {
            Ok(d) => assert!(
                matches!(d, VideoDecoder::Hevc(_)),
                "the hevc label must build the HEVC decoder, not some other variant"
            ),
            Err(DecoderError::Backend(msg)) => assert!(
                !msg.is_empty(),
                "a backend failure has to say what libavcodec reported"
            ),
            Err(other) => panic!(
                "with `hdr-decode` compiled, hevc-main10 is a known label \
                 backed by a real attempt; got {other:?}"
            ),
        }
    }

    /// Without `hdr-decode` the label is still recognised but not built —
    /// the receiver must tell a peer using an omitted codec from one talking nonsense, since only the first is a capability gap worth reporting.
    #[cfg(not(feature = "hdr-decode"))]
    #[test]
    fn hevc_label_reports_the_missing_feature_rather_than_an_unknown_codec() {
        let err = VideoDecoder::for_wire_codec("hevc-main10")
            .err()
            .expect("a build without `hdr-decode` cannot produce an HEVC decoder");
        match err {
            DecoderError::FeatureDisabled { codec, reason } => {
                assert_eq!(codec, "hevc-main10");
                assert!(!reason.is_empty(), "the gap has to name itself");
            }
            other => panic!("expected FeatureDisabled without `hdr-decode`, got {other:?}"),
        }
    }

    #[test]
    fn decoded_pixels_variant_predicates() {
        let rgba = DecodedPixels::Rgba8(vec![0u8; 4]);
        assert!(rgba.is_rgba());
        assert!(!rgba.is_p010());

        let p010 = DecodedPixels::P010 {
            y: vec![0u16; 16],
            uv: vec![0u16; 8],
            y_stride: 4,
            uv_stride: 4,
        };
        assert!(!p010.is_rgba());
        assert!(p010.is_p010());
    }
}
