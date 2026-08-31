//! Producing a small RGBA preview for a file, by category.
//!
//! Every path converges on [`image`]'s decode + `Triangle` downscale — the same
//! idiom `ui::image_store`, `ebook::epub` and `slides::assets` each grew their
//! own copy of, unified here. Rich decoders (PDF, video) live in sibling modules
//! behind feature flags; this module owns the always-on, pure-Rust paths (raster
//! images and EPUB covers) plus the shared encode/decode helpers.

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
    /// Sanity: the buffer length matches the declared dimensions.
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

/// The bytes to generate a thumbnail from: either already in memory (the FUSE
/// host has the content, or an app decoded it) or a file to read.
pub enum Source<'a> {
    Bytes(&'a [u8]),
    Path(&'a Path),
}

impl Source<'_> {
    /// Materialise the source bytes, reading the file if necessary.
    fn read(&self) -> Option<Vec<u8>> {
        match self {
            Source::Bytes(b) => Some(b.to_vec()),
            Source::Path(p) => std::fs::read(p).ok(),
        }
    }
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

/// Generate a preview for `source`, dispatching on `category`. Returns `None`
/// for a badge-only category, an unreadable source, or a decode failure — the
/// caller then falls back to a type badge.
pub fn generate(source: &Source, category: FileCategory, max_edge: u32) -> Option<RgbaThumb> {
    match category {
        FileCategory::Image => image_thumb(&source.read()?, max_edge),
        FileCategory::Ebook => epub_cover_thumb(&source.read()?, max_edge),
        FileCategory::Pdf => {
            #[cfg(feature = "pdf")]
            {
                crate::pdf::pdf_thumb(&source.read()?, max_edge)
            }
            #[cfg(not(feature = "pdf"))]
            {
                let _ = max_edge;
                None
            }
        }
        FileCategory::Video => {
            #[cfg(feature = "video")]
            {
                crate::video::video_thumb(source.as_path()?, max_edge)
            }
            #[cfg(not(feature = "video"))]
            {
                let _ = source.as_path();
                None
            }
        }
        _ => None,
    }
}

/// Decode raster image bytes and downscale so the longest edge is at most
/// `max_edge`. Handles every format the `image` crate is compiled for
/// (PNG/JPEG/WebP/GIF/BMP/…). `None` on any decode failure.
pub fn image_thumb(bytes: &[u8], max_edge: u32) -> Option<RgbaThumb> {
    let img = image::load_from_memory(bytes).ok()?;
    downscale_to_rgba(img, max_edge)
}

/// Extract and downscale the embedded cover of an EPUB. Reads the ZIP
/// container, resolves the cover href from the OPF manifest (the same
/// resolution order the ebook reader uses), decodes it, and downscales. `None`
/// when the book declares no cover or it fails to decode.
pub fn epub_cover_thumb(bytes: &[u8], max_edge: u32) -> Option<RgbaThumb> {
    let cover = crate::epub::cover_bytes(bytes)?;
    image_thumb(&cover, max_edge)
}

/// Common tail: cap the longest edge (only ever shrinking) and flatten to RGBA.
///
/// The `u32 as f32`/`f32 as u32` round-trip below is exact enough for any
/// real image: dimensions here never come close to f32's 2^24 exact-integer
/// range, and `.max(1)` after the truncating cast keeps the result positive
/// even though the cast itself can't (a ratio of two positive values) go negative.
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "see the function doc above"
)]
fn downscale_to_rgba(img: image::DynamicImage, max_edge: u32) -> Option<RgbaThumb> {
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return None;
    }
    let max_edge = max_edge.max(1);
    let scaled = if w.max(h) > max_edge {
        let scale = max_edge as f32 / w.max(h) as f32;
        let nw = ((w as f32 * scale).round() as u32).max(1);
        let nh = ((h as f32 * scale).round() as u32).max(1);
        img.resize_exact(nw, nh, image::imageops::FilterType::Triangle)
    } else {
        img
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

/// Wrap raw RGBA (e.g. a decoded PDF page or video frame) into an [`RgbaThumb`],
/// downscaling if it exceeds `max_edge`. Used by the rich-decoder modules (so it
/// is dead code when neither `pdf` nor `video` is enabled).
#[cfg_attr(not(any(feature = "pdf", feature = "video")), allow(dead_code))]
pub(crate) fn rgba_to_thumb(rgba: Vec<u8>, w: u32, h: u32, max_edge: u32) -> Option<RgbaThumb> {
    if w == 0 || h == 0 || rgba.len() != (w as usize).saturating_mul(h as usize).saturating_mul(4) {
        return None;
    }
    let buf = image::RgbaImage::from_raw(w, h, rgba)?;
    downscale_to_rgba(image::DynamicImage::ImageRgba8(buf), max_edge)
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

/// Read all bytes from a reader with a hard cap, so a hostile or corrupt input
/// can never make a worker allocate without bound. Returns `None` past `cap`.
pub(crate) fn read_capped<R: Read>(mut r: R, cap: usize) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    let taken = (&mut r)
        .take((cap as u64).saturating_add(1))
        .read_to_end(&mut buf)
        .ok()?;
    if taken > cap { None } else { Some(buf) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::solid_png;

    #[test]
    fn image_thumb_decodes_and_caps_the_long_edge() {
        let png = solid_png(800, 400, [12, 180, 90]);
        let t = image_thumb(&png, 256).expect("decodes");
        assert!(t.is_valid());
        assert_eq!(t.max_edge(), 256, "long edge capped to the target");
        assert_eq!((t.width, t.height), (256, 128), "aspect ratio preserved");
        // A pixel is the source colour (downscale of a solid image is that colour).
        assert_eq!(&t.rgba[0..3], &[12, 180, 90]);
    }

    #[test]
    fn small_images_are_not_upscaled() {
        let png = solid_png(40, 30, [255, 0, 0]);
        let t = image_thumb(&png, 256).expect("decodes");
        assert_eq!(
            (t.width, t.height),
            (40, 30),
            "never enlarged past the source"
        );
    }

    #[test]
    fn png_round_trips_through_encode_decode() {
        let png = solid_png(64, 48, [10, 20, 200]);
        let t = image_thumb(&png, 256).unwrap();
        let encoded = encode_png(&t).expect("encodes");
        let back = decode_png(&encoded).expect("decodes");
        assert_eq!((back.width, back.height), (t.width, t.height));
        assert_eq!(back.rgba, t.rgba, "PNG is lossless: exact round-trip");
    }

    #[test]
    fn corrupt_bytes_decode_to_none() {
        assert!(image_thumb(&[0xDE, 0xAD, 0xBE, 0xEF], 256).is_none());
        assert!(decode_png(b"not a png").is_none());
    }

    #[test]
    fn generate_dispatches_images_and_declines_badges() {
        let png = solid_png(100, 100, [1, 2, 3]);
        assert!(generate(&Source::Bytes(&png), FileCategory::Image, 64).is_some());
        // A badge category never attempts a decode.
        assert!(generate(&Source::Bytes(&png), FileCategory::Archive, 64).is_none());
    }
}
