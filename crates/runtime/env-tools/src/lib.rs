//! `env-tools` — tool images for environments.
//!
//! An environment's `/usr` is a stack of read-only images: one `env-runtime`
//! and any number of `tool` images built for exactly that runtime. This
//! crate turns `pkg install pandoc` into such a tool image:
//!
//! - [`key`]: the cache key, `(runtime id, sorted resolved NEVRAs)`.
//! - [`resolve`]: reading dnf's verdict on a request without installing.
//! - [`sandbox`]: the bwrap build sandbox — the runtime as an overlay lower
//!   with a writable upper, uid 0 in its own user namespace.
//! - [`seal`]: keeping what the packages added and nothing else.
//! - [`index`]: finding an image already built for a key, locally or in the
//!   published catalog.
//! - [`builder`]: all of the above in order.
//! - [`image`]: sealing a built tree into a signed `tool` image.
//! - [`store`]: the device's package store as the builder's store, and
//!   [`store::device_builder`].
//! - [`environment`]: the record's `ToolBuilder`, and rebuilding an
//!   environment's tools for `set-runtime`.
//!
//! The library needs no session: the `tilo-env-tools` binary drives it for
//! CI's catalog and for a relay's compute worker.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod builder;
pub mod delegated;
pub mod environment;
pub mod image;
pub mod index;
pub mod key;
pub mod resolve;
pub mod sandbox;
pub mod seal;
pub mod signing;
pub mod store;

pub use builder::{BuildError, Builder, Built, Origin, SealedTree, Timeouts, ToolStore};
pub use environment::{RebuildError, rebuild_tools};
pub use index::{Catalog, Entry, LocalIndex, NoCatalog, ToolLookup};
pub use key::{CacheKey, KeyDigest};
