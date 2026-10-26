//! Canonical desktop policy shared by the launcher, Settings, and workspace chrome.
//!
//! A personal default is a preference. A workspace rule is shared policy. A
//! per-person workspace override is the person's explicit escape hatch. The
//! resolver composes those three layers; callers must not reimplement the
//! precedence rules in UI code.
//!
//! This crate is the vocabulary and the resolver only, which the desktop model
//! embeds; reading and writing them on the host, and the storage scanner, are
//! `desktop-policy-host`'s.

pub mod defaults;
pub mod mimeapps;

pub use defaults::{
    AppCandidate, DefaultAppError, DefaultAppResolver, DefaultAppSelection,
    PERSONAL_DEFAULTS_SECTION, PersonalDefaults, ResourceKind, UserWorkspaceChoice,
    WorkspaceDefaultPolicy, WorkspaceDefaults,
};
pub use mimeapps::{MimeAppsChain, MimeAppsList, host_app_id, host_desktop_id};
