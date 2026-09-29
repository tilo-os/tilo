//! Decoded pictures in the memory negotiated with the decoder.
//!
//! [`crate::VideoFrame`] is the crate's long-standing surface: tightly packed
//! RGBA8 in system memory, which every existing consumer paints directly. It
//! is also the most expensive shape a hardware decoder can be forced into —
//! asking for RGBA drags a GPU-resident picture back through system memory and
//! a colour conversion before anyone has said they need it.
//!
//! [`Frame`] carries the picture in the form the pipeline negotiated:
//!
//! * [`FramePayload::Dmabuf`] — still in GPU memory, described by fds and a
//!   DRM modifier. Zero copies so far; the GPU importer takes it from here.
//! * [`FramePayload::Planar`] — NV12 / P010 in system memory. What a hardware
//!   decoder without a dmabuf export path yields (NVDEC, and any VA-API
//!   pipeline whose consumer declined dmabuf). Three quarters the bytes of
//!   RGBA and no colour conversion has happened yet.
//! * [`FramePayload::Rgba`] — system-memory RGBA8, either because the software
//!   decoder produced it or because the consumer asked for it.
//!
//! A consumer that can only paint RGBA calls [`Frame::to_rgba`], which
//! converts once and hands back an `Arc` it can keep. A consumer that can
//! upload planes or import a dmabuf reads [`Frame::payload`] and skips the
//! conversion entirely.

use std::sync::Arc;

#[cfg(unix)]
use crate::dmabuf::DmabufImage;
use crate::dmabuf::{ColorRange, Colorimetry, YuvMatrix};
use crate::event::VideoFrame;

/// A system-memory plane: its bytes plus the geometry needed to read them.
/// `stride` may exceed `width * bytes_per_sample` — decoders pad rows.
#[derive(Debug, Clone)]
pub struct PlaneData {
    pub bytes: Arc<Vec<u8>>,
    pub stride: u32,
    pub width: u32,
    pub height: u32,
}

/// The system-memory planar layouts a hardware decoder produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanarFormat {
    /// 8-bit 4:2:0. Plane 0 luma `w*h`, plane 1 interleaved `CbCr` at half
    /// resolution in both axes.
    Nv12,
    /// 10-bit 4:2:0 in 16-bit little-endian containers, samples in the high
    /// bits. Same plane structure as NV12 at two bytes per sample.
    P010,
}

impl PlanarFormat {
    /// Bytes per sample in each plane.
    pub fn bytes_per_sample(self) -> u32 {
        match self {
            PlanarFormat::Nv12 => 1,
            PlanarFormat::P010 => 2,
        }
    }
}

/// A planar YUV picture in system memory.
#[derive(Debug, Clone)]
pub struct PlanarImage {
    pub format: PlanarFormat,
    /// Luma, then interleaved chroma. Exactly two entries for NV12 and P010.
    pub planes: Vec<PlaneData>,
    pub colorimetry: Colorimetry,
}

/// Where a decoded picture's samples live.
#[derive(Clone)]
pub enum FramePayload {
    /// Tightly packed RGBA8, `width * height * 4` bytes.
    Rgba(Arc<Vec<u8>>),
    /// Planar YUV in system memory.
    Planar(Arc<PlanarImage>),
    /// Still in GPU memory. `Arc` because several consumers may hold the same
    /// picture, and the fds must close exactly once — when the last one drops.
    #[cfg(unix)]
    Dmabuf(Arc<DmabufImage>),
}

impl std::fmt::Debug for FramePayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FramePayload::Rgba(b) => write!(f, "Rgba({} bytes)", b.len()),
            FramePayload::Planar(p) => {
                write!(f, "Planar({:?}, {} planes)", p.format, p.planes.len())
            }
            #[cfg(unix)]
            FramePayload::Dmabuf(d) => write!(f, "Dmabuf({})", d.describe()),
        }
    }
}

/// One decoded picture in its native memory, with the presentation timestamp
/// the pipeline stamped it with.
#[derive(Clone, Debug)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// Presentation timestamp, milliseconds from the start of the stream.
    pub pts_ms: u64,
    pub payload: FramePayload,
}

impl Frame {
    /// Whether this picture is still in GPU memory (nothing has been copied
    /// to the CPU yet).
    pub fn is_gpu_resident(&self) -> bool {
        #[cfg(unix)]
        {
            matches!(self.payload, FramePayload::Dmabuf(_))
        }
        #[cfg(not(unix))]
        {
            false
        }
    }

    /// The dmabuf behind a GPU-resident picture, for the importer.
    #[cfg(unix)]
    pub fn dmabuf(&self) -> Option<&Arc<DmabufImage>> {
        match &self.payload {
            FramePayload::Dmabuf(d) => Some(d),
            _ => None,
        }
    }

    /// The system-memory planes behind a planar picture, for a direct upload.
    pub fn planar(&self) -> Option<&Arc<PlanarImage>> {
        match &self.payload {
            FramePayload::Planar(p) => Some(p),
            _ => None,
        }
    }

    /// Tightly packed RGBA8 for this picture, converting on the CPU if the
    /// payload is planar.
    ///
    /// Returns `None` for a [`FramePayload::Dmabuf`]: the samples are in GPU
    /// memory, and quietly mapping and converting them here would be exactly
    /// the stall this whole path exists to avoid. A consumer that cannot
    /// import a dmabuf must ask the engine for [`crate::FrameAcceptance::Rgba`]
    /// output instead, which negotiates a pipeline that lands in system
    /// memory.
    pub fn to_rgba(&self) -> Option<Arc<Vec<u8>>> {
        match &self.payload {
            FramePayload::Rgba(b) => Some(b.clone()),
            FramePayload::Planar(p) => Some(Arc::new(planar_to_rgba(p, self.width, self.height))),
            #[cfg(unix)]
            FramePayload::Dmabuf(_) => None,
        }
    }

    /// This picture as the legacy [`VideoFrame`] surface, or `None` when the
    /// payload is GPU-resident (see [`Self::to_rgba`]).
    pub fn to_video_frame(&self) -> Option<VideoFrame> {
        let rgba = self.to_rgba()?;
        Some(VideoFrame {
            width: self.width,
            height: self.height,
            rgba,
            pts_ms: self.pts_ms,
        })
    }
}

/// How much each chroma channel contributes to each output channel, the
/// inverse of the RGB→YCbCr matrix defined by the luma coefficients.
///
/// Cb is the blue-difference chroma channel and Cr the red-difference one.
#[derive(Clone, Copy, Debug)]
struct ChromaMatrix {
    red_from_cr: f32,
    green_from_cb: f32,
    green_from_cr: f32,
    blue_from_cb: f32,
}

impl ChromaMatrix {
    /// Invert the forward matrix `matrix`'s luma coefficients define.
    fn inverse_of(matrix: YuvMatrix) -> Self {
        let (kr, kg, kb) = matrix.luma_coefficients();
        Self {
            red_from_cr: 2.0 * (1.0 - kr),
            green_from_cb: -2.0 * kb * (1.0 - kb) / kg,
            green_from_cr: -2.0 * kr * (1.0 - kr) / kg,
            blue_from_cb: 2.0 * (1.0 - kb),
        }
    }
}

/// Convert a planar YUV picture to tightly packed RGBA8.
///
/// This is the CPU fallback, used when a consumer that can only paint RGBA is
/// fed a planar frame. The GPU path in `ui` does the same maths in a shader
/// and never runs this.
pub fn planar_to_rgba(img: &PlanarImage, width: u32, height: u32) -> Vec<u8> {
    let pic_width = width as usize;
    let pic_height = height as usize;
    // Bytes in one RGBA row, and in the whole picture. A picture whose byte
    // count does not fit in `usize` could never be allocated, so it reads as
    // empty rather than overflowing the size arithmetic. A zero-width picture
    // is empty by the same route, which is also what keeps the row iteration
    // below off a zero-length chunk.
    let Some(row_bytes) = pic_width.checked_mul(4).filter(|n| *n > 0) else {
        return Vec::new();
    };
    let Some(byte_len) = row_bytes.checked_mul(pic_height) else {
        return Vec::new();
    };
    let mut out = vec![0u8; byte_len];

    // NV12 and P010 are both two-plane formats. Anything shorter is not a
    // picture this function can read, so it stays black.
    let [luma, chroma, ..] = img.planes.as_slice() else {
        return out;
    };

    let chroma_matrix = ChromaMatrix::inverse_of(img.colorimetry.matrix);

    let (y_scale, y_offset, c_scale) = match img.colorimetry.range {
        // 16..235 luma / 16..240 chroma expanded to 0..1.
        ColorRange::Limited => (255.0 / 219.0, 16.0 / 255.0, 255.0 / 224.0),
        ColorRange::Full => (1.0, 0.0, 1.0),
    };

    let bps = img.format.bytes_per_sample() as usize;
    // 10-bit samples sit in the high bits of a 16-bit container.
    //
    // Every term of the byte offset is checked. `stride` and the plane
    // dimensions come from the decoder, for a format this crate did not
    // choose; a stride that does not match the buffer must read as black
    // rather than overflow an offset or index past the end.
    let sample = |plane: &PlaneData, x: usize, y: usize, comp: usize, comps: usize| -> f32 {
        let Some(idx) = x
            .checked_mul(comps)
            .and_then(|col| col.checked_add(comp))
            .and_then(|col| col.checked_mul(bps))
            .and_then(|off| y.checked_mul(plane.stride as usize)?.checked_add(off))
        else {
            return 0.0;
        };
        let Some(end) = idx.checked_add(bps) else {
            return 0.0;
        };
        // The slice pattern is the sample width: one byte for NV12, two for
        // P010. No index is computed, so a short plane falls through to black.
        match plane.bytes.get(idx..end) {
            Some(&[lo]) => f32::from(lo) / 255.0,
            Some(&[lo, hi]) => f32::from(u16::from_le_bytes([lo, hi]) >> 6) / 1023.0,
            _ => 0.0,
        }
    };

    let chroma_max_x = chroma.width.saturating_sub(1) as usize;
    let chroma_max_y = chroma.height.saturating_sub(1) as usize;
    for (y, row) in out.chunks_exact_mut(row_bytes).enumerate() {
        for (x, px) in row.chunks_exact_mut(4).enumerate() {
            let luma_sample = (sample(luma, x, y, 0, 1) - y_offset) * y_scale;
            // 4:2:0: one chroma sample per 2x2 luma block, clamped to the
            // plane in case the picture is odd-sized or the plane is short.
            let chroma_x = (x / 2).min(chroma_max_x);
            let chroma_y = (y / 2).min(chroma_max_y);
            let cb = (sample(chroma, chroma_x, chroma_y, 0, 2) - 0.5) * c_scale;
            let cr = (sample(chroma, chroma_x, chroma_y, 1, 2) - 0.5) * c_scale;

            let red = luma_sample + chroma_matrix.red_from_cr * cr;
            let green =
                luma_sample + chroma_matrix.green_from_cb * cb + chroma_matrix.green_from_cr * cr;
            let blue = luma_sample + chroma_matrix.blue_from_cb * cb;

            // `chunks_exact_mut(4)` yields exactly four bytes, which is what
            // makes writing the pixel a copy rather than four indexed stores.
            px.copy_from_slice(&[to_u8(red), to_u8(green), to_u8(blue), 255]);
        }
    }
    out
}

/// One colour channel, clamped to the display range and quantised to 8 bits.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the clamp on the same line bounds the product to 0.5..=255.5 — \
              non-negative, and inside u8 once truncated. A NaN channel (a \
              degenerate matrix) clamps to NaN, which casts to 0."
)]
fn to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// What memory shapes a consumer is willing to receive. Declared when the
/// engine is built, because it decides how the pipeline negotiates: a
/// consumer that can only paint RGBA gets a pipeline that ends in system
/// memory, and one that can import GPU buffers gets a pipeline that never
/// leaves the GPU when the hardware allows it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FrameAcceptance {
    /// System-memory RGBA8 only. The compatibility default: what every
    /// existing consumer of [`VideoFrame`] expects.
    #[default]
    Rgba,
    /// Any of dmabuf, planar, or RGBA — best first. The video editor and any
    /// other GPU-side consumer.
    Native,
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    reason = "fixture arithmetic on literal picture sizes, and channel \
              differences taken after widening to i32 — a wrong number here \
              is a broken test, not a hazard reaching a user's footage"
)]
mod tests {
    use super::*;

    fn nv12(width: u32, height: u32, y: u8, cb: u8, cr: u8, range: ColorRange) -> PlanarImage {
        let w = width as usize;
        let h = height as usize;
        PlanarImage {
            format: PlanarFormat::Nv12,
            planes: vec![
                PlaneData {
                    bytes: Arc::new(vec![y; w * h]),
                    stride: width,
                    width,
                    height,
                },
                PlaneData {
                    bytes: Arc::new(
                        (0..(w / 2) * (h / 2))
                            .flat_map(|_| [cb, cr])
                            .collect::<Vec<u8>>(),
                    ),
                    stride: width,
                    width: width / 2,
                    height: height / 2,
                },
            ],
            colorimetry: Colorimetry {
                matrix: YuvMatrix::Bt709,
                range,
            },
        }
    }

    /// Assert `actual` is within `tol` codes of `expected`.
    ///
    /// 8-bit chroma has no exactly-neutral value — true neutral is 127.5, and
    /// the nearest code, 128, sits half an LSB above it — so a *correct*
    /// conversion of "grey" lands one code off. A tolerance of 2 absorbs that
    /// while staying far tighter than any real defect: a swapped Cb/Cr, a
    /// wrong luma matrix, or a missed range expansion all move a channel by
    /// tens of codes.
    fn assert_near(actual: u8, expected: u8, tol: i32, what: &str) {
        let delta = i32::from(actual) - i32::from(expected);
        assert!(
            delta.abs() <= tol,
            "{what}: expected {expected} ±{tol}, got {actual} (off by {delta})"
        );
    }

    #[test]
    fn yuv_conversion_is_colour_correct_and_honours_stride() {
        // Phase `nv12_neutral_chroma_converts_to_grey_not_a_colour_cast`.
        {
            // Y=128, Cb=Cr=128 is mid grey. A wrong matrix or a missed -0.5
            // chroma bias shows up immediately as a colour cast here.
            let img = nv12(4, 4, 128, 128, 128, ColorRange::Full);
            let rgba = planar_to_rgba(&img, 4, 4);
            assert_eq!(rgba.len(), 4 * 4 * 4);
            for px in rgba.chunks_exact(4) {
                assert_near(px[0], px[1], 2, "r vs g on neutral chroma");
                assert_near(px[1], px[2], 2, "g vs b on neutral chroma");
                assert_eq!(px[3], 255, "opaque");
                assert_near(px[0], 128, 2, "mid grey");
            }
        }
        // Phase `limited_range_black_and_white_expand_to_the_full_scale`.
        {
            // Y=16 limited is video black; Y=235 is video white. Failing to expand
            // the range yields washed-out 16/235 instead of 0/255.
            let black = planar_to_rgba(&nv12(2, 2, 16, 128, 128, ColorRange::Limited), 2, 2);
            assert_near(black[0], 0, 2, "limited-range 16 is full black");
            let white = planar_to_rgba(&nv12(2, 2, 235, 128, 128, ColorRange::Limited), 2, 2);
            assert_near(white[0], 255, 2, "limited-range 235 is full white");

            // Without the range expansion these would come back as 16 and 235 —
            // a washed-out picture. Prove the gap really is the full scale.
            assert!(
                i32::from(white[0]) - i32::from(black[0]) >= 250,
                "limited range must expand to the full scale, got {} .. {}",
                black[0],
                white[0]
            );
        }
        // Phase `chroma_offset_drives_the_expected_channel`.
        {
            // Cr above neutral must push red up and leave blue below it; Cb above
            // neutral must do the reverse. This catches a Cb/Cr swap, which a
            // grey-only test cannot see.
            let reddish = planar_to_rgba(&nv12(2, 2, 128, 128, 200, ColorRange::Full), 2, 2);
            assert!(reddish[0] > reddish[2], "high Cr → red exceeds blue");
            let blueish = planar_to_rgba(&nv12(2, 2, 128, 200, 128, ColorRange::Full), 2, 2);
            assert!(blueish[2] > blueish[0], "high Cb → blue exceeds red");
        }
        // Phase `stride_padding_is_honoured_rather_than_read_as_pixels`.
        {
            // A decoder pads rows; reading stride as width shears the picture.
            let width = 4u32;
            let height = 2u32;
            let stride = 8u32; // 4 bytes of padding per row
            let mut luma = vec![0u8; (stride * height) as usize];
            // Row 0 real pixels bright, its padding black; row 1 real pixels dark.
            for x in 0..width as usize {
                luma[x] = 235;
                luma[stride as usize + x] = 16;
            }
            let img = PlanarImage {
                format: PlanarFormat::Nv12,
                planes: vec![
                    PlaneData {
                        bytes: Arc::new(luma),
                        stride,
                        width,
                        height,
                    },
                    PlaneData {
                        bytes: Arc::new(vec![128u8; (stride * height / 2) as usize]),
                        stride,
                        width: width / 2,
                        height: height / 2,
                    },
                ],
                colorimetry: Colorimetry {
                    matrix: YuvMatrix::Bt709,
                    range: ColorRange::Limited,
                },
            };
            let rgba = planar_to_rgba(&img, width, height);
            // Every pixel of row 0 is white, every pixel of row 1 is black. If the
            // stride were ignored, row 0's tail would pick up the padding zeros.
            for x in 0..width as usize {
                assert_near(rgba[x * 4], 255, 2, &format!("row 0 col {x} is white"));
                assert_near(
                    rgba[(width as usize + x) * 4],
                    0,
                    2,
                    &format!("row 1 col {x} is black"),
                );
            }
        }
    }

    #[test]
    fn malformed_or_non_nv12_frames_convert_safely_without_copies() {
        // Phase `degenerate_geometry_converts_to_nothing_rather_than_panicking`.
        {
            // A decoder renegotiating mid-stream, or a container whose header
            // disagrees with its frames, hands over dimensions that describe no
            // picture. The converter has to answer "no pixels", because every
            // alternative here is a panic on somebody's footage.
            let img = nv12(4, 4, 128, 128, 128, ColorRange::Full);
            assert!(planar_to_rgba(&img, 0, 4).is_empty(), "zero width");
            assert!(planar_to_rgba(&img, 4, 0).is_empty(), "zero height");
            assert!(planar_to_rgba(&img, 0, 0).is_empty(), "zero both");
            // A byte count that cannot fit in `usize`: 2^31 x 2^31 pixels is 2^64
            // bytes of RGBA.
            assert!(
                planar_to_rgba(&img, 1 << 31, 1 << 31).is_empty(),
                "a picture too large to describe is not a picture"
            );
        }
        // Phase `a_plane_shorter_than_its_geometry_claims_reads_as_black_not_a_panic`.
        {
            // The stride says 64 bytes a row; the buffer holds one row. Every
            // sample past it must fall through to black rather than index out.
            let img = PlanarImage {
                format: PlanarFormat::Nv12,
                planes: vec![
                    PlaneData {
                        bytes: Arc::new(vec![235u8; 64]),
                        stride: 64,
                        width: 64,
                        height: 8,
                    },
                    PlaneData {
                        bytes: Arc::new(vec![128u8; 8]),
                        stride: 64,
                        width: 32,
                        height: 4,
                    },
                ],
                colorimetry: Colorimetry {
                    matrix: YuvMatrix::Bt709,
                    range: ColorRange::Limited,
                },
            };
            let rgba = planar_to_rgba(&img, 64, 8);
            assert_eq!(rgba.len(), 64 * 8 * 4, "full picture, short plane");
            assert_near(rgba[0], 255, 2, "the one row that exists is white");
            // Row 1 onwards is past the end of the buffer.
            assert_near(rgba[64 * 4], 0, 2, "a missing row is black");
        }
        // Phase `a_single_plane_image_is_not_read_as_nv12`.
        {
            // NV12 and P010 are two-plane. One plane is a different format the
            // caller has mislabelled, and guessing at the chroma would invent
            // colour that is not in the stream.
            let mut img = nv12(4, 4, 128, 128, 128, ColorRange::Full);
            img.planes.truncate(1);
            let rgba = planar_to_rgba(&img, 4, 4);
            assert_eq!(rgba.len(), 4 * 4 * 4, "still a full-size buffer");
            assert!(rgba.iter().all(|b| *b == 0), "and it is blank");
        }
        // Phase `an_rgba_frame_hands_back_the_same_buffer_without_re_copying`.
        {
            let bytes = Arc::new(vec![7u8; 8 * 8 * 4]);
            let frame = Frame {
                width: 8,
                height: 8,
                pts_ms: 42,
                payload: FramePayload::Rgba(bytes.clone()),
            };
            let got = frame.to_rgba().unwrap();
            assert!(Arc::ptr_eq(&got, &bytes), "same allocation, not a copy");
            let vf = frame.to_video_frame().unwrap();
            assert_eq!(vf.pts_ms, 42);
            assert_eq!(vf.width, 8);
        }
    }

    #[test]
    #[cfg(unix)]
    fn a_gpu_resident_frame_refuses_to_silently_copy_itself_to_the_cpu() {
        let frame = Frame {
            width: 64,
            height: 64,
            pts_ms: 0,
            payload: FramePayload::Dmabuf(Arc::new(DmabufImage {
                planes: vec![],
                fourcc: crate::dmabuf::DRM_FORMAT_NV12,
                modifier: 0,
                width: 64,
                height: 64,
                colorimetry: Colorimetry::default(),
            })),
        };
        assert!(frame.is_gpu_resident());
        assert!(frame.to_rgba().is_none(), "no hidden GPU→CPU readback");
        assert!(frame.to_video_frame().is_none());
        assert!(frame.dmabuf().is_some(), "the importer can still reach it");
    }
}
