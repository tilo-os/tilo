//! Canonical desktop policy shared by the launcher, Settings, and workspace chrome.
//!
//! A personal default is a preference. A workspace rule is shared policy. A
//! per-person workspace override is the person's explicit escape hatch. The
//! resolver composes those three layers; callers must not reimplement the
//! precedence rules in UI code.

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

pub mod defaults;
pub mod mimeapps;
#[cfg(all(feature = "host-storage", not(target_arch = "wasm32")))]
pub mod storage;

pub use defaults::{
    AppCandidate, DefaultAppError, DefaultAppResolver, DefaultAppSelection, PersonalDefaults,
    ResourceKind, UserWorkspaceChoice, WorkspaceDefaultPolicy, WorkspaceDefaults,
};
pub use mimeapps::{MimeAppsChain, MimeAppsList, host_app_id, host_desktop_id};
#[cfg(all(feature = "host-storage", not(target_arch = "wasm32")))]
pub use storage::{
    CleanupError, CleanupItem, CleanupKind, CleanupPreview, CleanupReport, StorageCategory,
    StorageEntry, StorageRoots, StorageSnapshot, apply_cleanup, preview_cache_cleanup,
    scan_storage,
};
