//! Shared thumbnail generation and content-addressed caching for files.
//!
//! Three layers, each usable on its own:
//!
//! 1. **Generation** ([`generate`], [`pdf`], [`video`], [`epub`]) — decode and
//!    downscale previews for images, PDF first pages, EPUB covers, and video
//!    keyframes, or classify other files into a [`FileCategory`] for a type
//!    badge. Rich decoders
//!    are feature-gated (`pdf`, `video`) so the FUSE daemon and hermetic builds
//!    stay light; images and EPUB covers are always-on pure Rust.
//! 2. **Cache** ([`ThumbCache`]) — content-addressed (BLAKE3 → PNG) on disk so
//!    it survives a restart, fronted by a bounded in-memory LRU. Keyed by the
//!    same hash the Tilo filesystem addresses content by, so previews deduplicate
//!    across paths and peers and a changed file receives a new key.
//! 3. **Service** ([`ThumbnailService`]) — an off-thread worker pool: request a
//!    preview, get a placeholder immediately, and receive the image later.
//!
//! The crate is deliberately free of `vello`/`wgpu`/`ui`: it links into the FUSE
//! filesystem, which must never pull a GPU stack into the OS image. Generation
//! yields raw RGBA + PNG bytes; the UI layer wraps them in a `peniko::ImageData`
//! at the edge.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod cache;
pub mod category;
pub mod epub;
pub mod generate;
pub mod service;

#[cfg(test)]
pub(crate) mod testutil;

#[cfg(feature = "pdf")]
pub mod pdf;
#[cfg(feature = "video")]
pub mod video;

pub use cache::{ThumbCache, ThumbKey};
pub use category::{FileCategory, category_for_ext, category_for_path};
pub use generate::{
    DEFAULT_EDGE, RgbaThumb, Source, decode_png, encode_png, epub_cover_thumb, generate,
    image_thumb,
};
pub use service::{ThumbState, ThumbnailService};
