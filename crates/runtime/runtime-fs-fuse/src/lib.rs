//! `runtime-fs-fuse`: the workspace filesystem's capabilities presented
//! through FUSE.
//!
//! A fixed native projection worker (Imager's retained-source target
//! projection) serves these over a FUSE channel the launcher already
//! admitted: [`directory_fuse::DirectoryFilesystem`] over a directory
//! capability ([`runtime_fs::directory_connection::DirectoryConnection`]),
//! [`selected_fuse::SelectedFilesystem`] over one selected file
//! ([`runtime_fs::selected_client::SelectedFileConnection`]). Neither mounts
//! anything or grants authority; they present what the capability allows.
//!
//! They used to be runtime-fs's `selected-fuse` feature. The shared app
//! runtime carries runtime-fs, so the feature put `fuser` and this code into
//! every app process; here only the worker that serves them links them.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

#[cfg(target_os = "linux")]
pub mod directory_fuse;
#[cfg(target_os = "linux")]
pub mod selected_fuse;
