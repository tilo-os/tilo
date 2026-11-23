//! `env-tools` — tool images for environments.
//!
//! An environment's `/usr` is a stack of read-only images: one `env-runtime`
//! and any number of `tool` images built for exactly that runtime. This crate
//! turns `pkg install pandoc` into such a tool image, without a session or a
//! display: resolve the request with dnf, look the key up in the index and the
//! catalog, else build it in a sandbox and seal what the packages added.

pub mod builder;
pub mod delegated;
pub mod environment;
pub mod image;
pub mod index;
pub mod key;
pub mod network;
pub mod resolve;
pub mod sandbox;
pub mod seal;
pub mod signing;
pub mod store;

pub use builder::{BuildError, Builder, Built, Origin, SealedTree, Timeouts, ToolStore};
pub use environment::{RebuildError, rebuild_tools};
pub use index::{Catalog, Entry, LocalIndex, NoCatalog, ToolLookup};
pub use key::{CacheKey, KeyDigest};
