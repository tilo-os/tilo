//! The toolkit's image decoders (the `image` crate: PNG, JPEG, WebP), for a web page's images
//! (`webview::paint`) and the pictures an [`crate::ImageStore`] loads.

use vello::peniko::ImageData;

use crate::image_store::DecodedImage;

/// A page image's encoded bytes to RGBA, at the size it was encoded.
pub fn page(bytes: &[u8]) -> Option<ImageData> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (width, height) = img.dimensions();
    Some(crate::raster::rgba8(img.into_raw(), width, height))
}

/// Encoded bytes to RGBA no wider or taller than `max_dimension`, for an image store.
pub(crate) fn bounded(encoded: &[u8], max_dimension: u32) -> Option<DecodedImage> {
    tilo_image_codec::DecodeImage {
        encoded,
        max_dimension,
    }
    .decode_native()
    .ok()
}

/// Encoded bytes made a PNG no wider or taller than `max_dimension` (a picture a workspace emoji
/// keeps).
pub(crate) fn small_png(encoded: &[u8], max_dimension: u32) -> Option<Vec<u8>> {
    let small = bounded(encoded, max_dimension)?;
    crate::png::encode_rgba8(small.width, small.height, &small.rgba).ok()
}
