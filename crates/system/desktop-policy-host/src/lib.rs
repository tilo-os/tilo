//! Desktop policy on the host: the person's defaults read from and written to
//! their settings, the `mimeapps.list` chain of the session and the list Tilo
//! publishes into it, and the storage scanner with its cache cleaner.
//!
//! The vocabulary and the resolver are `desktop-policy`'s, which stays free of
//! host storage so the desktop model can embed it.

pub mod defaults;
pub mod mimeapps;
#[cfg(not(target_arch = "wasm32"))]
pub mod storage;

pub use defaults::{load_personal_defaults, save_personal_defaults};
#[cfg(not(target_arch = "wasm32"))]
pub use storage::{
    CleanupError, CleanupItem, CleanupKind, CleanupPreview, CleanupReport, StorageCategory,
    StorageEntry, StorageRoots, StorageSnapshot, apply_cleanup, preview_cache_cleanup,
    scan_storage,
};
