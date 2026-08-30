//! Decoded pictures in whatever memory the decoder actually produced them in.
//!
//! [`crate::VideoFrame`] is the crate's long-standing surface: tightly packed
//! RGBA8 in system memory, which every existing consumer paints directly. It
//! is also the most expensive shape a hardware decoder can be forced into —
//! asking for RGBA drags a GPU-resident picture back through system memory and
//! a colour conversion before anyone has said they need it.
//!
//! [`Frame`] is the honest one. It carries the picture in the form the
//! pipeline negotiated:
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

use crate::dmabuf::{ColorRange, Colorimetry, DmabufImage};
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
    /// 8-bit 4:2:0. Plane 0 luma `w*h`, plane 1 interleaved CbCr at half
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
    Dmabuf(Arc<DmabufImage>),
}

impl std::fmt::Debug for FramePayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FramePayload::Rgba(b) => write!(f, "Rgba({} bytes)", b.len()),
            FramePayload::Planar(p) => {
                write!(f, "Planar({:?}, {} planes)", p.format, p.planes.len())
            }
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
        matches!(self.payload, FramePayload::Dmabuf(_))
    }

    /// The dmabuf behind a GPU-resident picture, for the importer.
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

/// Convert a planar YUV picture to tightly packed RGBA8.
///
/// This is the CPU fallback, used when a consumer that can only paint RGBA is
/// fed a planar frame. The GPU path in `ui` does the same maths in a shader
/// and never runs this.
pub fn planar_to_rgba(img: &PlanarImage, width: u32, height: u32) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let mut out = vec![0u8; w * h * 4];
    if img.planes.len() < 2 {
        return out;
    }
    let (kr, kg, kb) = img.colorimetry.matrix.luma_coefficients();
    // Inverse of the RGB→YCbCr matrix defined by the luma coefficients.
    let r_cr = 2.0 * (1.0 - kr);
    let b_cb = 2.0 * (1.0 - kb);
    let g_cb = -2.0 * kb * (1.0 - kb) / kg;
    let g_cr = -2.0 * kr * (1.0 - kr) / kg;

    let (y_scale, y_offset, c_scale) = match img.colorimetry.range {
        // 16..235 luma / 16..240 chroma expanded to 0..1.
        ColorRange::Limited => (255.0 / 219.0, 16.0 / 255.0, 255.0 / 224.0),
        ColorRange::Full => (1.0, 0.0, 1.0),
    };

    let luma = &img.planes[0];
    let chroma = &img.planes[1];
    let bps = img.format.bytes_per_sample() as usize;
    // 10-bit samples sit in the high bits of a 16-bit container.
    let sample = |plane: &PlaneData, x: usize, y: usize, comp: usize, comps: usize| -> f32 {
        let idx = y * plane.stride as usize + (x * comps + comp) * bps;
        match plane.bytes.get(idx..idx + bps) {
            Some(s) if bps == 1 => s[0] as f32 / 255.0,
            Some(s) => (u16::from_le_bytes([s[0], s[1]]) >> 6) as f32 / 1023.0,
            None => 0.0,
        }
    };

    for y in 0..h {
        for x in 0..w {
            let yy = (sample(luma, x, y, 0, 1) - y_offset) * y_scale;
            let cx = (x / 2).min(chroma.width.saturating_sub(1) as usize);
            let cy = (y / 2).min(chroma.height.saturating_sub(1) as usize);
            let cb = (sample(chroma, cx, cy, 0, 2) - 0.5) * c_scale;
            let cr = (sample(chroma, cx, cy, 1, 2) - 0.5) * c_scale;

            let r = yy + r_cr * cr;
            let g = yy + g_cb * cb + g_cr * cr;
            let b = yy + b_cb * cb;

            let i = (y * w + x) * 4;
            out[i] = to_u8(r);
            out[i + 1] = to_u8(g);
            out[i + 2] = to_u8(b);
            out[i + 3] = 255;
        }
    }
    out
}

fn to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// What memory shapes a consumer is willing to receive. Declared when the
/// engine is built, because it decides how the pipeline negotiates: a
/// consumer that can only paint RGBA gets a pipeline that ends in system
/// memory, and one that can import GPU buffers gets a pipeline that never
/// leaves the GPU when the hardware allows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameAcceptance {
    /// System-memory RGBA8 only. The compatibility default: what every
    /// existing consumer of [`VideoFrame`] expects.
    Rgba,
    /// Any of dmabuf, planar, or RGBA — best first. The video editor and any
    /// other GPU-side consumer.
    Native,
}

impl Default for FrameAcceptance {
    fn default() -> Self {
        FrameAcceptance::Rgba
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dmabuf::YuvMatrix;

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
        let delta = actual as i32 - expected as i32;
        assert!(
            delta.abs() <= tol,
            "{what}: expected {expected} ±{tol}, got {actual} (off by {delta})"
        );
    }

    #[test]
    fn nv12_neutral_chroma_converts_to_grey_not_a_colour_cast() {
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

    #[test]
    fn limited_range_black_and_white_expand_to_the_full_scale() {
        // Y=16 limited is video black; Y=235 is video white. Failing to expand
        // the range yields washed-out 16/235 instead of 0/255.
        let black = planar_to_rgba(&nv12(2, 2, 16, 128, 128, ColorRange::Limited), 2, 2);
        assert_near(black[0], 0, 2, "limited-range 16 is full black");
        let white = planar_to_rgba(&nv12(2, 2, 235, 128, 128, ColorRange::Limited), 2, 2);
        assert_near(white[0], 255, 2, "limited-range 235 is full white");

        // Without the range expansion these would come back as 16 and 235 —
        // a washed-out picture. Prove the gap really is the full scale.
        assert!(
            white[0] as i32 - black[0] as i32 >= 250,
            "limited range must expand to the full scale, got {} .. {}",
            black[0],
            white[0]
        );
    }

    #[test]
    fn chroma_offset_drives_the_expected_channel() {
        // Cr above neutral must push red up and leave blue below it; Cb above
        // neutral must do the reverse. This catches a Cb/Cr swap, which a
        // grey-only test cannot see.
        let reddish = planar_to_rgba(&nv12(2, 2, 128, 128, 200, ColorRange::Full), 2, 2);
        assert!(reddish[0] > reddish[2], "high Cr → red exceeds blue");
        let blueish = planar_to_rgba(&nv12(2, 2, 128, 200, 128, ColorRange::Full), 2, 2);
        assert!(blueish[2] > blueish[0], "high Cb → blue exceeds red");
    }

    #[test]
    fn stride_padding_is_honoured_rather_than_read_as_pixels() {
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

    #[test]
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

    #[test]
    fn an_rgba_frame_hands_back_the_same_buffer_without_re_copying() {
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
