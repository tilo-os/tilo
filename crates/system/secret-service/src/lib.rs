//! Tilo's own Secret Service: `org.freedesktop.secrets` for the person's
//! session, backed by the device identity.

pub mod app;
pub mod migrate;
pub mod service;
pub mod store;
pub mod vault;

use std::path::PathBuf;

/// The directory the person's store lives in: `$XDG_DATA_HOME/tilo/secrets`.
///
/// `None` without an absolute data home or a home directory.
#[must_use]
pub fn data_dir() -> Option<PathBuf> {
    Some(tilo_paths::xdg::data_home()?.join("tilo/secrets"))
}
