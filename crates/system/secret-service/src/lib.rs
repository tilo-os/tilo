//! Tilo's own Secret Service: `org.freedesktop.secrets` for the person's
//! session, backed by the device identity.
//!
//! libsecret clients (Chromium, 1Password, `secret-tool`, anything built on
//! GNOME or KDE libraries) find the service where they always did. What
//! differs is how it opens: the store is sealed under a key the machine's
//! device identity releases, so it is available after any kind of login and
//! never asks the person for a password of its own. See [`store`] for the
//! locked and open states, [`app`] for how callers are attributed, and
//! [`migrate`] for taking over the items GNOME Keyring held before.

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
