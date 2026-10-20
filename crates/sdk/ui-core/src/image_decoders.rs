//! The toolkit's image decoders (the `image` crate: PNG, JPEG, WebP), for a
//! web page's images (`webview::paint`) and the pictures an
//! [`crate::ImageStore`] loads.
//!
//! Without `lazy-image-decode` they are linked and always there. With it,
//! nothing in the toolkit names them: the program hands over a loader
//! ([`defer_image_decoders`]) that fetches their code the first time
//! something decodes and passes back [`builtin_image_decoders`]
//! ([`install_image_decoders`]). Until then page images are left out and an
//! image store holds the pictures it fetched as undecoded, loading them again
//! once the decoders are in. The browser desktop uses this to keep the
//! decoders out of its start module.

use vello::peniko::ImageData;

use crate::image_store::DecodedImage;

/// The decoders, as the program hands them over.
#[derive(Clone, Copy)]
pub struct ImageDecoders {
    /// A page image's encoded bytes to RGBA, at the size it was encoded.
    pub page: fn(&[u8]) -> Option<ImageData>,
    /// Encoded bytes to RGBA no wider or taller than the given dimension,
    /// for an image store.
    pub bounded: fn(&[u8], u32) -> Option<DecodedImage>,
}

/// Hands over the decoders, for [`install_image_decoders`].
pub type MakeImageDecoders = fn() -> ImageDecoders;

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
    ImageDecoders { page, bounded }
}

#[cfg(not(feature = "lazy-image-decode"))]
mod linked {
    use super::ImageDecoders;

    #[expect(
        clippy::unnecessary_wraps,
        reason = "the lazy-image-decode build has none until they are fetched"
    )]
    pub(crate) fn decoders() -> Option<ImageDecoders> {
        Some(super::builtin_image_decoders())
    }

    pub(crate) fn decoders_in() -> bool {
        true
    }
}

#[cfg(not(feature = "lazy-image-decode"))]
pub(crate) use linked::{decoders, decoders_in};

#[cfg(feature = "lazy-image-decode")]
mod fetched {
    use super::{ImageDecoders, MakeImageDecoders};
    use crate::deferred::Deferred;

    static DECODERS: Deferred<ImageDecoders> = Deferred::new();

    /// The decoders, or `None` while they are still to come (and then
    /// fetched).
    pub(crate) fn decoders() -> Option<ImageDecoders> {
        DECODERS.get()
    }

    /// Whether the decoders are in, without fetching them.
    pub(crate) fn decoders_in() -> bool {
        DECODERS.generation() > 0
    }

    /// How the program fetches the decoders' code: `load` starts the fetch
    /// and, once the code is in, calls [`install_image_decoders`] (or
    /// [`image_decoders_load_failed`]).
    pub fn defer_image_decoders(load: fn()) {
        DECODERS.defer(load);
    }

    /// The decoders' code is in: images decode from now on.
    pub fn install_image_decoders(make: MakeImageDecoders) {
        DECODERS.install(make());
    }

    /// The fetch failed: the next decode fetches again.
    pub fn image_decoders_load_failed() {
        DECODERS.load_failed();
    }
}

#[cfg(feature = "lazy-image-decode")]
pub(crate) use fetched::{decoders, decoders_in};
#[cfg(feature = "lazy-image-decode")]
pub use fetched::{defer_image_decoders, image_decoders_load_failed, install_image_decoders};
