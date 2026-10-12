//! Producing a small RGBA preview for a file, by category.
//!
//! Rich decoders (PDF, video) live in sibling modules behind feature flags;
//! this module owns the always-on, pure-Rust paths (raster images and EPUB
//! covers) plus shared encode/decode helpers.

use std::io::{Cursor, Read};
use std::path::Path;

use crate::category::FileCategory;

/// A decoded, downscaled preview: 8-bit RGBA, row-major, `width * height * 4`
/// bytes. Deliberately GPU-toolkit-free — the UI layer wraps it in a
/// `peniko::ImageData` at the edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaThumb {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

impl RgbaThumb {
    /// Whether the buffer length matches the declared dimensions.
    pub fn is_valid(&self) -> bool {
        self.width > 0
            && self.height > 0
            && self.rgba.len()
                == (self.width as usize)
                    .saturating_mul(self.height as usize)
                    .saturating_mul(4)
    }

    /// The longest edge in pixels.
    pub fn max_edge(&self) -> u32 {
        self.width.max(self.height)
    }
}

/// The box a thumbnail must fit inside. The longest-edge form
/// (`ThumbSize::from(256)`) is a square box; a filmstrip cell or a photo tile
/// asks for a 16:9 one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ThumbSize {
    pub width: u32,
    pub height: u32,
}

impl ThumbSize {
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// A square box: the longest edge is at most `edge`.
    pub const fn edge(edge: u32) -> Self {
        Self::new(edge, edge)
    }

    /// The longest edge of the box.
    pub fn max_edge(self) -> u32 {
        self.width.max(self.height)
    }

    /// Never a zero-sized box, whatever a caller asked for.
    fn at_least_one(self) -> Self {
        Self::new(self.width.max(1), self.height.max(1))
    }
}

impl From<u32> for ThumbSize {
    fn from(edge: u32) -> Self {
        Self::edge(edge)
    }
}

/// The bytes to generate a thumbnail from: either already in memory (the FUSE
/// host has the content, or an app decoded it) or a file to read.
pub enum Source<'a> {
    Bytes(&'a [u8]),
    Path(&'a Path),
}

impl Source<'_> {
    /// Materialise the source bytes, reading the file if necessary. A source
    /// past [`MAX_SOURCE_BYTES`] is refused rather than read whole.
    fn read(&self) -> Option<Vec<u8>> {
        match self {
            Source::Bytes(b) => (b.len() <= MAX_SOURCE_BYTES).then(|| b.to_vec()),
            Source::Path(p) => read_bounded(std::fs::File::open(p).ok()?, MAX_SOURCE_BYTES),
        }
    }

    #[cfg(feature = "video")]
    fn as_path(&self) -> Option<&Path> {
        match self {
            Source::Path(p) => Some(p),
            Source::Bytes(_) => None,
        }
    }
}

/// The default longest-edge target for generated thumbnails. Big enough to stay
/// crisp on a 2x grid tile, small enough to fit an xattr and gossip cheaply.
pub const DEFAULT_EDGE: u32 = 256;

/// No source is read whole past this just to make a thumbnail: a pathological
/// file must not stall a worker (or the FUSE thread) or exhaust memory.
pub const MAX_SOURCE_BYTES: usize = 96 * 1024 * 1024;

/// Encoded raster images past this are refused before a codec sees them.
pub const MAX_IMAGE_BYTES: usize = 64 * 1024 * 1024;

/// Raster images whose header declares more pixels than this are refused
/// before any pixel is allocated. 32 Mpx covers phones and most cameras.
pub const MAX_DECODE_PIXELS: u64 = 32 * 1024 * 1024;

/// The decoder's own allocation ceiling: the full-size RGBA of the largest
/// admitted image plus codec working memory.
const MAX_DECODE_ALLOC: u64 = MAX_DECODE_PIXELS * 4 + 64 * 1024 * 1024;

/// Read at most `limit` bytes. `None` when the reader holds more (one sentinel
/// byte tells an exact-limit input from an oversized one) or fails.
pub fn read_bounded(reader: impl Read, limit: usize) -> Option<Vec<u8>> {
    let cap = u64::try_from(limit).ok()?.saturating_add(1);
    let mut bytes = Vec::new();
    reader.take(cap).read_to_end(&mut bytes).ok()?;
    (bytes.len() <= limit).then_some(bytes)
}

/// Generate a preview for `source`, dispatching on `category`. Returns `None`
/// for a badge-only category, an unreadable source, or a decode failure — the
/// caller then falls back to a type badge.
pub fn generate(
    source: &Source,
    category: FileCategory,
    size: impl Into<ThumbSize>,
) -> Option<RgbaThumb> {
    let size = size.into();
    match category {
        FileCategory::Image => image_thumb(&source.read()?, size),
        FileCategory::Ebook => epub_cover_thumb(&source.read()?, size),
        FileCategory::Pdf => {
            #[cfg(feature = "pdf")]
            {
                crate::pdf::pdf_thumb(&source.read()?, size)
            }
            #[cfg(not(feature = "pdf"))]
            {
                None
            }
        }
        FileCategory::Video => {
            #[cfg(feature = "video")]
            {
                crate::video::video_thumb(source.as_path()?, size)
            }
            #[cfg(not(feature = "video"))]
            {
                None
            }
        }
        _ => None,
    }
}

/// Decode raster image bytes into an upright preview that fits `size`.
///
/// See [`decode_image`] for the bounds applied before any pixel is allocated.
/// Handles every format the `image` crate is compiled for (PNG/JPEG/WebP/GIF/
/// BMP/…). `None` on any decode failure.
pub fn image_thumb(bytes: &[u8], size: impl Into<ThumbSize>) -> Option<RgbaThumb> {
    image_thumb_with(bytes, size, DecodeLimits::default())
}

/// [`image_thumb`] under tighter source bounds than the default.
pub fn image_thumb_with(
    bytes: &[u8],
    size: impl Into<ThumbSize>,
    limits: DecodeLimits,
) -> Option<RgbaThumb> {
    downscale_to_rgba(decode_image_with(bytes, limits)?, size.into())
}

/// What a raster source may declare before it is refused, checked against its
/// header before any pixel is allocated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeLimits {
    /// Largest declared width × height.
    pub max_pixels: u64,
    /// Largest declared width or height.
    pub max_dimension: u32,
}

impl Default for DecodeLimits {
    fn default() -> Self {
        Self {
            max_pixels: MAX_DECODE_PIXELS,
            max_dimension: u32::MAX,
        }
    }
}

/// Decode raster image bytes at full size, upright.
///
/// The header is read first, and bytes past [`MAX_IMAGE_BYTES`] or an image
/// declaring more than [`MAX_DECODE_PIXELS`] are refused before any pixel is
/// allocated, so an untrusted file cannot turn a preview into an unbounded
/// allocation. The EXIF orientation is applied, so a portrait phone photo
/// comes out upright.
pub fn decode_image(bytes: &[u8]) -> Option<image::DynamicImage> {
    decode_image_with(bytes, DecodeLimits::default())
}

/// [`decode_image`] under explicit source bounds. The default pixel budget
/// still caps the decoder's own allocation, whatever `limits` allows.
pub fn decode_image_with(bytes: &[u8], limits: DecodeLimits) -> Option<image::DynamicImage> {
    if bytes.is_empty() || bytes.len() > MAX_IMAGE_BYTES {
        return None;
    }
    let (w, h) = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()?;
    let pixels = u64::from(w).saturating_mul(u64::from(h));
    if w == 0
        || h == 0
        || w.max(h) > limits.max_dimension
        || pixels > limits.max_pixels.min(MAX_DECODE_PIXELS)
    {
        return None;
    }
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut alloc = image::Limits::default();
    alloc.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(alloc);
    let mut decoder = reader.into_decoder().ok()?;
    let orientation = image::ImageDecoder::orientation(&mut decoder).ok();
    let mut img = image::DynamicImage::from_decoder(decoder).ok()?;
    if let Some(orientation) = orientation {
        img.apply_orientation(orientation);
    }
    Some(img)
}

/// Extract and downscale the embedded cover of an EPUB. Reads the ZIP
/// container, resolves the cover href from the OPF manifest (the same
/// resolution order the ebook reader uses), decodes it, and downscales. `None`
/// when the book declares no cover or it fails to decode.
pub fn epub_cover_thumb(bytes: &[u8], size: impl Into<ThumbSize>) -> Option<RgbaThumb> {
    let cover = crate::epub::cover_bytes(bytes)?;
    image_thumb(&cover, size)
}

/// The dimensions a `w`×`h` image takes to fit inside `size`, preserving the
/// aspect ratio and never enlarging.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the scaled dimensions are at most the source dimensions, which fit u32"
)]
pub fn fit_within(w: u32, h: u32, size: impl Into<ThumbSize>) -> (u32, u32) {
    let size = size.into().at_least_one();
    if w <= size.width && h <= size.height {
        return (w, h);
    }
    let scale = (f64::from(size.width) / f64::from(w)).min(f64::from(size.height) / f64::from(h));
    let nw = (f64::from(w) * scale).round().max(1.0) as u32;
    let nh = (f64::from(h) * scale).round().max(1.0) as u32;
    (nw.min(w), nh.min(h))
}

/// Common tail: fit the box (only ever shrinking) and flatten to RGBA.
fn downscale_to_rgba(img: image::DynamicImage, size: ThumbSize) -> Option<RgbaThumb> {
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return None;
    }
    let (nw, nh) = fit_within(w, h, size);
    let scaled = if (nw, nh) == (w, h) {
        img
    } else {
        img.resize_exact(nw, nh, image::imageops::FilterType::Triangle)
    };
    let rgba = scaled.to_rgba8();
    let (width, height) = (rgba.width(), rgba.height());
    let out = RgbaThumb {
        rgba: rgba.into_raw(),
        width,
        height,
    };
    out.is_valid().then_some(out)
}

/// Wrap raw RGBA (a decoded PDF page, a video frame, a camera capture) into an
/// [`RgbaThumb`] that fits `size`.
pub fn rgba_to_thumb(
    rgba: Vec<u8>,
    w: u32,
    h: u32,
    size: impl Into<ThumbSize>,
) -> Option<RgbaThumb> {
    if w == 0 || h == 0 || rgba.len() != (w as usize).saturating_mul(h as usize).saturating_mul(4) {
        return None;
    }
    let buf = image::RgbaImage::from_raw(w, h, rgba)?;
    downscale_to_rgba(image::DynamicImage::ImageRgba8(buf), size.into())
}

/// Encode a thumbnail as PNG — the on-disk cache, xattr and gossip form. `None`
/// on an encode failure (never happens for a valid buffer).
pub fn encode_png(thumb: &RgbaThumb) -> Option<Vec<u8>> {
    if !thumb.is_valid() {
        return None;
    }
    let buf = image::RgbaImage::from_raw(thumb.width, thumb.height, thumb.rgba.clone())?;
    let mut out = Vec::new();
    image::DynamicImage::ImageRgba8(buf)
        .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
        .ok()?;
    Some(out)
}

/// Decode a cached/transported PNG thumbnail back into RGBA.
pub fn decode_png(bytes: &[u8]) -> Option<RgbaThumb> {
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png).ok()?;
    let rgba = img.to_rgba8();
    let (width, height) = (rgba.width(), rgba.height());
    let out = RgbaThumb {
        rgba: rgba.into_raw(),
        width,
        height,
    };
    out.is_valid().then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::solid_png;

    #[test]
    fn a_box_fits_both_axes() {
        let png = solid_png(400, 400, [1, 2, 3]);
        let t = image_thumb(&png, ThumbSize::new(160, 90)).expect("decodes");
        assert_eq!(
            (t.width, t.height),
            (90, 90),
            "a square fits the short axis"
        );
        let wide = image_thumb(&solid_png(1920, 1080, [1, 2, 3]), ThumbSize::new(160, 90))
            .expect("decodes");
        assert_eq!((wide.width, wide.height), (160, 90));
    }

    /// A header declaring more pixels than the budget is refused before the
    /// decoder allocates them, even though the file itself is tiny.
    #[test]
    fn an_oversized_header_is_refused_before_decoding() {
        let mut png = solid_png(1, 1, [0, 0, 0]);
        // IHDR width/height live at bytes 16..24; declare 65535 × 65535.
        png[16..24].copy_from_slice(&[0, 0, 0xFF, 0xFF, 0, 0, 0xFF, 0xFF]);
        assert!(decode_image(&png).is_none());
    }

    /// A portrait phone JPEG is stored landscape with an EXIF rotation; its
    /// preview must come out upright.
    #[test]
    fn exif_orientation_is_applied() {
        let mut jpeg = Vec::new();
        image::DynamicImage::ImageRgb8(image::RgbImage::new(40, 20))
            .write_to(&mut Cursor::new(&mut jpeg), image::ImageFormat::Jpeg)
            .unwrap();
        // An APP1 EXIF segment right after SOI: big-endian TIFF, one IFD entry,
        // Orientation (0x0112) = 6 ("rotate 90° clockwise").
        let tiff: &[u8] = &[
            b'M', b'M', 0, 42, 0, 0, 0, 8, // header, IFD at offset 8
            0, 1, // one entry
            0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0, // Orientation SHORT 6
            0, 0, 0, 0, // no next IFD
        ];
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend_from_slice(tiff);
        let len = u16::try_from(app1.len() + 2).unwrap().to_be_bytes();
        let mut tagged = jpeg[..2].to_vec();
        tagged.extend_from_slice(&[0xFF, 0xE1, len[0], len[1]]);
        tagged.extend_from_slice(&app1);
        tagged.extend_from_slice(&jpeg[2..]);
        let t = image_thumb(&tagged, 256).expect("decodes");
        assert_eq!((t.width, t.height), (20, 40), "rotated upright");
    }

    #[test]
    fn read_bounded_refuses_one_byte_past_the_limit() {
        assert_eq!(read_bounded(&b"abcd"[..], 4).as_deref(), Some(&b"abcd"[..]));
        assert!(read_bounded(&b"abcde"[..], 4).is_none());
    }

    #[test]
    fn image_thumbnails_decode_cap_and_encode_or_decline() {
        // Phase `image_thumb_decodes_and_caps_the_long_edge`.
        {
            let png = solid_png(800, 400, [12, 180, 90]);
            let t = image_thumb(&png, 256).expect("decodes");
            assert!(t.is_valid());
            assert_eq!(t.max_edge(), 256, "long edge capped to the target");
            assert_eq!((t.width, t.height), (256, 128), "aspect ratio preserved");
            // A pixel is the source colour (downscale of a solid image is that colour).
            assert_eq!(&t.rgba[0..3], &[12, 180, 90]);
        }
        // Phase `small_images_are_not_upscaled`.
        {
            let png = solid_png(40, 30, [255, 0, 0]);
            let t = image_thumb(&png, 256).expect("decodes");
            assert_eq!(
                (t.width, t.height),
                (40, 30),
                "never enlarged past the source"
            );
        }
        // Phase `png_round_trips_through_encode_decode`.
        {
            let png = solid_png(64, 48, [10, 20, 200]);
            let t = image_thumb(&png, 256).unwrap();
            let encoded = encode_png(&t).expect("encodes");
            let back = decode_png(&encoded).expect("decodes");
            assert_eq!((back.width, back.height), (t.width, t.height));
            assert_eq!(back.rgba, t.rgba, "PNG is lossless: exact round-trip");
        }
        // Phase `corrupt_bytes_decode_to_none`.
        {
            assert!(image_thumb(&[0xDE, 0xAD, 0xBE, 0xEF], 256).is_none());
            assert!(decode_png(b"not a png").is_none());
        }
        // Phase `generate_dispatches_images_and_declines_badges`.
        {
            let png = solid_png(100, 100, [1, 2, 3]);
            assert!(generate(&Source::Bytes(&png), FileCategory::Image, 64).is_some());
            // A badge category never attempts a decode.
            assert!(generate(&Source::Bytes(&png), FileCategory::Archive, 64).is_none());
        }
    }
}
