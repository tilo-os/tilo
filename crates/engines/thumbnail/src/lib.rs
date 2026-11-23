//! Shared thumbnail generation and content-addressed caching for files, in three layers
//! each usable on its own:
//!
//! 1. **Generation** ([`mod@generate`], [`epub`]): decode and downscale previews for
//!    images, EPUB covers, and the PDF first pages and video frames of the decoders an
//!    app hands over, or classify other files into a [`FileCategory`] for a type badge.
//! 2. **The cache** ([`cache`]): previews keyed by the source's content address.
//! 3. **The service** ([`service`]): one priority queue and worker pool in front of it.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod batch;
pub mod cache;
pub mod category;
pub mod epub;
pub mod generate;
pub mod service;

#[cfg(test)]
pub(crate) mod testutil;

pub use cache::{ThumbCache, ThumbKey};
pub use category::{FileCategory, category_for_ext, category_for_path};
pub use generate::{
    DEFAULT_EDGE, DecodeLimits, MAX_DECODE_PIXELS, MAX_IMAGE_BYTES, MAX_SOURCE_BYTES,
    PdfRasteriser, RgbaThumb, Source, ThumbSize, VideoFrames, VideoOpener, decode_image,
    decode_image_with, decode_png, encode_png, epub_cover_thumb, fit_within, generate, image_thumb,
    image_thumb_with, open_video, pdf_previews, read_bounded, rgba_to_thumb, set_pdf_rasteriser,
    set_video_opener, video_thumb,
};
pub use service::{Priority, ThumbRequest, ThumbState, ThumbnailService};
