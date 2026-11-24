//! Images crossing the clipboard as PNG bytes and RGBA8 pixels.
//!
//! The service carries an image as its file's bytes (PNG, JPEG or WebP), while
//! an OS clipboard takes pixels; these are the two conversions the service's
//! sides need, in one place.

use crate::MAX_CLIPBOARD_IMAGE_INPUT_BYTES;

/// An RGBA8 image as PNG, when it is well formed and the image clipboard takes it.
pub(crate) fn encode_png(width: usize, height: usize, rgba: &[u8]) -> Option<Vec<u8>> {
    if width == 0 || height == 0 || rgba.len() > MAX_CLIPBOARD_IMAGE_INPUT_BYTES {
        return None;
    }
    let width_u32 = u32::try_from(width).ok()?;
    let height_u32 = u32::try_from(height).ok()?;
    let expected_len = width.checked_mul(height)?.checked_mul(4)?;
    if rgba.len() != expected_len {
        return None;
    }
    let mut bytes = Vec::new();
    {
        use image::ImageEncoder as _;
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(rgba, width_u32, height_u32, image::ExtendedColorType::Rgba8)
            .ok()?;
    }
    (bytes.len() <= crate::MAX_CLIPBOARD_IMAGE_BYTES).then_some(bytes)
}

/// An encoded image as RGBA8 and its size, for an OS clipboard that takes pixels. Only the
/// service's serving side carries encoded images.
#[cfg(any(unix, windows))]
pub(crate) fn decode_image(bytes: &[u8]) -> Option<(usize, usize, Vec<u8>)> {
    let image = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (width, height) = image.dimensions();
    let rgba = image.into_raw();
    if rgba.len() > MAX_CLIPBOARD_IMAGE_INPUT_BYTES {
        return None;
    }
    Some((
        usize::try_from(width).ok()?,
        usize::try_from(height).ok()?,
        rgba,
    ))
}
