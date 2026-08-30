//! app-fs — read-only FUSE3 driver that mounts a qOS
//! package payload from a content-addressed [`BlobStore`].
//!
//! ## Mental model
//!
//! Every `.qpkg` carries a payload "Collection" — a small,
//! deterministic table of `(payload-relative-path -> blob-hash)`
//! pairs (see [`Collection`]). The on-disk file tree is a view
//! of that table; each file's bytes live as a single
//! BLAKE3-addressed blob in a [`BlobStore`].
//!
//! The launcher mounts a package at
//! `/var/qos-apps/<package_id>/`. The mount is intentionally
//! **lazy**:
//!
//! - `lookup` / `getattr` resolve from the Collection's path
//!   table without fetching any blob bytes.
//! - `read(ino, offset, size)` calls `BlobStore::get_range` for
//!   exactly the slice the kernel asked for.
//! - A [`PageCache`] sits between the FUSE worker and the
//!   store so an mmap'd binary doesn't re-fetch the same page
//!   on every fault.
//!
//! Result: a 500 MB app bundle's cold-start cost is bounded by
//! the bytes the app actually reads, not by the bundle size.
//!
//! ## Module map
//!
//! - [`store`] — `BlobStore` trait + `MemoryBlobStore` /
//!   `DiskBlobStore` / `CountingStore` impls.
//! - [`cache`] — LRU page cache that wraps a store.
//! - [`fs`] — `fuser::Filesystem` impl.
//! - [`mount`] — mount / unmount lifecycle handles.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "a test that unwraps is stating an invariant"
    )
)]

pub mod cache;
pub mod collection;
pub mod fs;
pub mod mount;
pub mod store;

pub use cache::{PAGE_SIZE, PageCache, PageCacheStats};
pub use collection::{Collection, CollectionDecodeError};
pub use fs::QosAppFs;
pub use mount::{Mount, MountError, MountOptions};
pub use store::{
    BlobError, BlobStore, CounterSnapshot, CountingStore, DiskBlobStore, MemoryBlobStore,
};
