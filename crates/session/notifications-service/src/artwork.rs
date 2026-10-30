//! A notification's own artwork as pixels: an encoded image an app hands
//! over (PNG, JPEG, WebP or SVG, as bytes, a file or a file descriptor),
//! decoded and bounded into the [`ImageData`] the banners and the centre
//! paint. Only ever pixels: what an image shows never names an app.

use std::io::Read as _;
use std::path::Path;

use crate::ImageData;

/// The most bytes of an encoded image the daemon reads.
pub const MAX_ENCODED_BYTES: u64 = 4 * 1024 * 1024;
/// The longest edge an image is kept at: a banner's artwork is small.
pub const MAX_EDGE: u32 = 256;
/// The longest edge of a raster image the decoder accepts at all.
const MAX_DECODED_EDGE: u32 = 4096;

/// `bytes` decoded, scaled down to [`MAX_EDGE`]; `None` when they are no
/// image the daemon can paint.
#[must_use]
pub fn decode(bytes: &[u8]) -> Option<ImageData> {
    if u64::try_from(bytes.len()).map_or(true, |length| length > MAX_ENCODED_BYTES) {
        return None;
    }
    if looks_like_svg(bytes) {
        return rasterize_svg(bytes);
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_DECODED_EDGE);
    limits.max_image_height = Some(MAX_DECODED_EDGE);
    limits.max_alloc = Some(64 * 1024 * 1024);
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    reader.limits(limits);
    let decoded = reader.decode().ok()?;
    if decoded.width() == 0 || decoded.height() == 0 {
        return None;
    }
    let decoded = if decoded.width().max(decoded.height()) > MAX_EDGE {
        decoded.resize(MAX_EDGE, MAX_EDGE, image::imageops::FilterType::Triangle)
    } else {
        decoded
    };
    let rgba = decoded.into_rgba8();
    let (width, height) = rgba.dimensions();
    Some(ImageData {
        width,
        height,
        rgba: rgba.into_raw(),
    })
}

/// The image `reader` holds, read up to [`MAX_ENCODED_BYTES`]: a file or a
/// file descriptor an app handed over.
#[must_use]
pub fn read(reader: impl std::io::Read) -> Option<ImageData> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_ENCODED_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .ok()?;
    decode(&bytes)
}

/// The image file at `location`: an absolute path or a `file://` URI, as the
/// specification's `image-path` hint and `app_icon` name one. `None` for an
/// icon name, which names no file.
#[must_use]
pub fn from_location(location: &str) -> Option<ImageData> {
    let path = match location.strip_prefix("file://") {
        Some(uri) => url::Url::parse(&format!("file://{uri}"))
            .ok()?
            .to_file_path()
            .ok()?,
        None => Path::new(location).to_path_buf(),
    };
    if !path.is_absolute() {
        return None;
    }
    let file = std::fs::File::open(&path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > MAX_ENCODED_BYTES {
        return None;
    }
    read(file)
}

fn looks_like_svg(bytes: &[u8]) -> bool {
    let head = bytes.get(..bytes.len().min(512)).unwrap_or_default();
    let text = String::from_utf8_lossy(head);
    let text = text.trim_start_matches('\u{feff}').trim_start();
    text.starts_with("<svg") || (text.starts_with("<?xml") && text.contains("<svg"))
}

fn rasterize_svg(bytes: &[u8]) -> Option<ImageData> {
    use resvg::{tiny_skia, usvg};
    let tree = usvg::Tree::from_data(bytes, &usvg::Options::default()).ok()?;
    let size = tree.size();
    let longest = size.width().max(size.height());
    if !longest.is_finite() || longest <= 0.0 {
        return None;
    }
    let edge = f32::from(u16::try_from(MAX_EDGE).ok()?);
    let scale = edge / longest;
    let width = dimension(size.width() * scale)?;
    let height = dimension(size.height() * scale)?;
    let mut pixmap = tiny_skia::Pixmap::new(width, height)?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    let rgba = pixmap
        .pixels()
        .iter()
        .flat_map(|pixel| {
            let color = pixel.demultiply();
            [color.red(), color.green(), color.blue(), color.alpha()]
        })
        .collect();
    Some(ImageData {
        width,
        height,
        rgba,
    })
}

/// A rendered edge in whole pixels, between one and [`MAX_EDGE`].
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a finite edge checked to lie in 1..=MAX_EDGE"
)]
fn dimension(edge: f32) -> Option<u32> {
    let edge = edge.ceil();
    if !edge.is_finite() || edge < 1.0 {
        return Some(1);
    }
    (edge <= f32::from(u16::try_from(MAX_EDGE).ok()?)).then_some(edge as u32)
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "a fixture that fails to build fails the test"
)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = image::RgbaImage::from_pixel(width, height, image::Rgba([255, 0, 0, 255]));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgba8(image)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .expect("encoded");
        bytes
    }

    #[test]
    fn a_png_is_decoded_and_a_large_one_scaled_down() {
        let small = decode(&png(2, 1)).expect("decoded");
        assert_eq!((small.width, small.height), (2, 1));
        assert_eq!(small.rgba, [255, 0, 0, 255, 255, 0, 0, 255]);
        let large = decode(&png(1024, 512)).expect("decoded");
        assert_eq!((large.width, large.height), (MAX_EDGE, MAX_EDGE / 2));
        assert_eq!(large.rgba.len(), (MAX_EDGE * MAX_EDGE / 2 * 4) as usize);
    }

    #[test]
    fn an_svg_is_rendered_at_the_kept_edge() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="8"><rect width="16" height="8" fill="#00ff00"/></svg>"##;
        let image = decode(svg).expect("rendered");
        assert_eq!((image.width, image.height), (MAX_EDGE, MAX_EDGE / 2));
        assert_eq!(image.rgba.get(..4), Some(&[0, 255, 0, 255][..]));
    }

    #[test]
    fn what_is_no_image_is_refused() {
        assert!(decode(b"not an image").is_none());
        assert!(decode(&[]).is_none());
        assert!(
            from_location("dialog-information").is_none(),
            "an icon name"
        );
        assert!(from_location("relative/path.png").is_none());
    }

    #[test]
    fn an_image_file_is_read_by_path_and_by_uri() {
        let dir = tempfile::tempdir().expect("directory");
        let path = dir.path().join("a picture.png");
        std::fs::write(&path, png(3, 3)).expect("written");
        let by_path = from_location(path.to_str().expect("utf-8")).expect("read");
        assert_eq!((by_path.width, by_path.height), (3, 3));
        let uri = url::Url::from_file_path(&path).expect("uri");
        assert!(from_location(uri.as_str()).is_some());
        assert!(
            from_location(dir.path().to_str().expect("utf-8")).is_none(),
            "a directory"
        );
    }
}
