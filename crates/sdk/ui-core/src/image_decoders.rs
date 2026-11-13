//! The toolkit's image decoders (the `image` crate: PNG, JPEG, WebP), for a web page's images
//! (`webview::paint`) and the pictures an [`crate::ImageStore`] loads. Without `lazy-image-decode` they
//! are linked and always there.

use vello::peniko::ImageData;

use crate::image_store::DecodedImage;

/// The decoders, as the program hands them over.
#[derive(Clone, Copy)]
pub struct ImageDecoders {
    /// A page image's encoded bytes to RGBA, at the size it was encoded.
    pub page: fn(&[u8]) -> Option<ImageData>,
    /// Encoded bytes to RGBA no wider or taller than the given dimension, for an image store.
    pub bounded: fn(&[u8], u32) -> Option<DecodedImage>,
    /// Encoded bytes made a PNG no wider or taller than the given dimension (a picture a workspace
    /// emoji keeps): the encoder rides with the decoders, out of a browser's start module.
    pub small_png: fn(&[u8], u32) -> Option<Vec<u8>>,
}

/// The `image` crate's decoders.
#[must_use]
pub fn builtin_image_decoders() -> ImageDecoders {
    fn page(bytes: &[u8]) -> Option<ImageData> {
        let img = image::load_from_memory(bytes).ok()?.to_rgba8();
        let (width, height) = img.dimensions();
        Some(crate::raster::rgba8(img.into_raw(), width, height))
    }
    fn bounded(encoded: &[u8], max_dimension: u32) -> Option<DecodedImage> {
        tilo_image_codec::DecodeImage {
            encoded,
            max_dimension,
        }
        .decode_native()
        .ok()
    }
    fn small_png(encoded: &[u8], max_dimension: u32) -> Option<Vec<u8>> {
        let small = bounded(encoded, max_dimension)?;
        crate::png::encode_rgba8(small.width, small.height, &small.rgba).ok()
    }
    ImageDecoders {
        page,
        bounded,
        small_png,
    }
}

crate::lazy_code! {
    /// The decoders ([`builtin_image_decoders`]).
    pub static IMAGE_DECODERS: ImageDecoders = builtin_image_decoders, feature = "lazy-image-decode";
}

/// The decoders, or `None` while they are still to come (and then fetched).
pub(crate) fn decoders() -> Option<ImageDecoders> {
    IMAGE_DECODERS.get()
}

/// Wake `window` once the decoders are in (or their fetch failed): work it does once a frame that found
/// them missing then runs again.
pub(crate) fn wake_when_in(window: crate::wake::WakeHandle) {
    struct Window(crate::wake::WakeHandle);
    impl std::task::Wake for Window {
        fn wake(self: std::sync::Arc<Self>) {
            self.0.wake();
        }
    }
    IMAGE_DECODERS.wake_on_arrival(std::task::Waker::from(std::sync::Arc::new(Window(window))));
}
