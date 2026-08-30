//! qOS's one shared thumbnail system: generate a small, good-looking, type-
//! telling preview for a file, cache it content-addressed, and hand it out
//! off-thread.
//!
//! Three layers, each usable on its own:
//!
//! 1. **Generation** ([`generate`], [`pdf`], [`video`], [`epub`]) — decode +
//!    downscale a real preview for the types worth it (images, PDF first page,
//!    EPUB cover, video keyframe), or classify anything else into a
//!    [`FileCategory`] the UI draws an honest **type badge** for. Rich decoders
//!    are feature-gated (`pdf`, `video`) so the FUSE daemon and hermetic builds
//!    stay light; images and EPUB covers are always-on pure Rust.
//! 2. **Cache** ([`ThumbCache`]) — content-addressed (BLAKE3 → PNG) on disk so
//!    it survives a restart, fronted by a bounded in-memory LRU. Keyed by the
//!    same hash the qOS filesystem addresses content by, so previews dedup
//!    across paths and peers and invalidate for free when a file changes.
//! 3. **Service** ([`ThumbnailService`]) — an off-thread worker pool: request a
//!    preview, get a placeholder now and the real image on a later frame; a big
//!    folder never janks because generation is bounded, cached and lazy.
//!
//! The crate is deliberately free of `vello`/`wgpu`/`ui`: it links into the FUSE
//! filesystem, which must never pull a GPU stack into the OS image. Generation
//! yields raw RGBA + PNG bytes; the UI layer wraps them in a `peniko::ImageData`
//! at the edge (see the file manager's adoption).

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "a test that unwraps is stating an invariant"
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
