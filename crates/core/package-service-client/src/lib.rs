//! The package service's wire and its typed client: the one way into the device's package store
//! while the device runs. The Store and Settings reach it on the connection the launcher hands
//! them; `app` and `tilo-install` reach it from the person's terminal ([`person`]).
//!
//! The service itself (`package-service`) depends on this crate, so `app`'s library, which the
//! service also runs, can call it without a cycle.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tilo_app_package::{Manifest, PackageId};

pub mod client;
#[cfg(unix)]
pub mod person;
pub mod wire;

/// The name the launcher hands this service to an app under, and its socket
/// file's name without the extension.
pub const SERVICE: &str = "package-service";

/// `$XDG_RUNTIME_DIR/tilo/services/package-service.sock`, or `None` without a runtime directory. Never
/// the shared temp directory, where any local user could bind the name first.
#[must_use]
pub fn default_socket_path() -> Option<PathBuf> {
    tilo_paths::xdg::runtime_dir().map(|base| {
        base.join("tilo")
            .join("services")
            .join("package-service.sock")
    })
}

/// An image's package id, written `tapp:<hex>` or as the bare hex.
///
/// # Errors
///
/// A sentence naming what is not a package id.
pub fn image_id(raw: &str) -> Result<PackageId, String> {
    let text = if raw.starts_with("tapp:") {
        raw.to_owned()
    } else {
        format!("tapp:{raw}")
    };
    text.parse()
        .map_err(|error| format!("parse: image id {raw:?}: {error}"))
}

/// The registry-authenticated identity downloaded bytes must match, supplied by the privileged
/// Store capability: an archive may describe itself, but it may not choose the identity the user
/// approved in the catalogue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallExpectation {
    pub package_root: PackageId,
    pub publisher: [u8; 32],
    pub app_id: String,
    pub version: String,
    pub size_bytes: u64,
}

/// Process-verified identity of handed bytes; grants no trust.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalReview {
    pub root: String,
    pub archive_hash: String,
    pub publisher: String,
    pub attestor: String,
    pub manifest: Manifest,
    pub installed_version: Option<String>,
    pub pinned: bool,
    pub user_managed: bool,
}
