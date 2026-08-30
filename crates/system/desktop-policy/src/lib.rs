//! Canonical desktop policy shared by the launcher, Settings, and workspace chrome.
//!
//! A personal default is a preference. A workspace rule is shared policy. A
//! per-person workspace override is the person's explicit escape hatch. The
//! resolver composes those three layers; callers must not reimplement the
//! precedence rules in UI code.

pub mod defaults;
pub mod storage;

pub use defaults::{
    AppCandidate, DefaultAppError, DefaultAppResolver, DefaultAppSelection, PersonalDefaults,
    ResourceKind, UserWorkspaceChoice, WorkspaceDefaultPolicy, WorkspaceDefaults,
};
pub use storage::{
    CleanupError, CleanupItem, CleanupKind, CleanupPreview, CleanupReport, StorageCategory,
    StorageEntry, StorageRoots, StorageSnapshot, apply_cleanup, preview_cache_cleanup,
    scan_storage,
};
