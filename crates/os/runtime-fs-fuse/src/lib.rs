//! The workspace filesystem's capabilities presented through FUSE:
//! [`directory_fuse::DirectoryFilesystem`] over a directory capability and
//! [`selected_fuse::SelectedFilesystem`] over one selected file. Neither
//! mounts anything or grants authority; they present what the capability
//! allows. Only the worker that serves them links `fuser`.

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
