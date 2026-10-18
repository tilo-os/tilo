//! Shared thumbnail generation and content-addressed caching for files.
//!
//! Three layers, each usable on its own:
//!
//! 1. **Generation** ([`mod@generate`], `video`, [`epub`]) — decode and
//!    downscale previews for images, PDF first pages, EPUB covers, and video
//!    keyframes, or classify other files into a [`FileCategory`] for a type
//!    badge. Video is feature-gated (`video`) so the FUSE daemon and hermetic
//!    builds stay light; images and EPUB covers are always-on pure Rust. PDF
//!    pages come from Tilo's PDF engine, which this crate does not link: an
//!    app that does hands its rasteriser over once ([`set_pdf_rasteriser`]).
//!    The engine is a library of its own beside the shared runtime, mapped
//!    only by the apps that read PDFs, so this crate, which every app's
//!    runtime carries, stays free of it.
//! 2. **Cache** ([`ThumbCache`]) — keyed by the BLAKE3 hash of the source (the
//!    same hash the Tilo filesystem addresses content by, so previews
//!    deduplicate across paths and peers and a changed file receives a new
//!    key), with the PNGs kept in a `tilo-content` content store so they
//!    survive a restart, fronted by a bounded in-memory LRU.
//! 3. **Service** ([`ThumbnailService`]) — one priority queue feeding one
//!    off-thread worker pool: request a preview, get a placeholder immediately,
//!    and receive the image later. Every app's previews — file tiles, photo
//!    grids, camera peeks, video filmstrips — go through it.
//!
//! The crate is deliberately free of `vello`/`wgpu`/`ui`: it links into the FUSE
//! filesystem, which must never pull a GPU stack into the OS image. Generation
//! yields raw RGBA + PNG bytes; the UI layer wraps them in a `peniko::ImageData`
//! at the edge.

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

#[cfg(feature = "video")]
pub mod video;

pub use cache::{ThumbCache, ThumbKey};
pub use category::{FileCategory, category_for_ext, category_for_path};
pub use generate::{
    DEFAULT_EDGE, DecodeLimits, MAX_DECODE_PIXELS, MAX_IMAGE_BYTES, MAX_SOURCE_BYTES, RgbaThumb,
    PdfRasteriser, Source, ThumbSize, decode_image, decode_image_with, decode_png, encode_png,
    epub_cover_thumb, fit_within, generate, image_thumb, image_thumb_with, pdf_previews,
    read_bounded, rgba_to_thumb, set_pdf_rasteriser,
};
pub use service::{Priority, ThumbRequest, ThumbState, ThumbnailService};
