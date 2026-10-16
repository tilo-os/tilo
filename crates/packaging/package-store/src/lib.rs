//! `package-store` — the device's installed packages.
//!
//! One registry and one [`InstalledPackage`] type describe every installed
//! and preinstalled package; the launcher, the shell, settings, the relay,
//! the app host and `tilo-install` all read and write them through this
//! crate, so there is exactly one answer to "what is installed and where".
//!
//! - [`registry`] — the `registry.toml` rows.
//! - [`catalog`] — the user's store merged over the read-only system store:
//!   what a session can run.
//! - [`layout`] — where a store keeps its files, and where the stores are.
//! - [`mounts`] — which installed trees running apps have mounted.
//! - `lease` (feature `lease`) — the hold a launch keeps on its payload
//!   tree from choosing it until its sandbox exits.
//! - [`integrity`] — replay an installed package's signature chain against
//!   its files.
//! - `PackageStore` (feature `store`) — install, remove, pin and collect.
//!   Payload bytes live in the fs crate's content store; each payload tree
//!   is materialised once from it by reflink, and a package's `.tapp` may be
//!   thin because only blobs the store lacks are fetched.

pub mod catalog;
pub mod dev_env;
pub mod integrity;
pub mod interfaces;
pub mod layout;
#[cfg(feature = "lease")]
pub mod lease;
pub mod mounts;
pub mod registry;

#[cfg(feature = "store")]
mod content;
#[cfg(feature = "store")]
mod image;
#[cfg(feature = "store")]
mod store;

pub use catalog::{
    Catalog, CatalogError, Layers, SystemStore, display_name_for_package_id, find_installed,
    list_installed, runs_over_image,
};
#[cfg(feature = "store")]
pub use image::{
    FleetSource, HOST_SOURCES_FILE, ImageShelf, ImageTrust, MaterializeError,
    SYSTEM_IMAGE_PUBLISHERS_FILE, Store, check_tool_runtime, local_materialized_tree,
    materialized_tree, tool_runtime,
};
pub use layout::Layout;
pub use registry::{InstalledPackage, Pinning, Registry, RegistryError};
#[cfg(feature = "store")]
pub use store::{
    Admission, DEFAULT_MAX_STORE_BYTES, DiskUsage, InstallSelection, Installed, Orphans,
    PackageStore, Removed,
};
pub use tilo_app_package::BlobFetcher;
#[cfg(feature = "store")]
pub use tilo_user_identity::{CertifiedFleet, DeviceStanding};

/// Why a store operation failed.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("HOME / XDG_DATA_HOME not set — cannot locate the app store")]
    NoHome,
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Registry(#[from] RegistryError),
    #[error("package refused: {0}")]
    Refused(String),
    #[error("image app-store build requires a valid SOURCE_DATE_EPOCH")]
    InvalidImageBuildEpoch,
    #[error("package archive: {0}")]
    Tapp(String),
    #[error("{} payload blob(s) are neither in the archive, in the store, nor fetchable", .0.len())]
    MissingBlobs(Vec<tilo_app_package::BlobHash>),
    #[error("payload: {0}")]
    Payload(String),
    #[error("content store: {0}")]
    Content(String),
    /// Installation selected the package, but the final content-store close
    /// failed. The selected row remains committed; this is not a rollback.
    #[error("selected {id} {version} ({root}), but closing the content store failed: {source}")]
    CommittedSelection {
        id: String,
        version: String,
        root: String,
        source: Box<StoreError>,
    },
    #[error("{0} is not installed")]
    NotInstalled(String),
    #[error("refusing to remove pinned package {0}")]
    Pinned(String),
    #[error("installed package {0} has an unreadable record: {1}")]
    CorruptPackage(String, String),
}

/// Recursive byte sum of every regular file under `path`; 0 when absent.
pub fn tree_size(path: &std::path::Path) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else if meta.is_file() {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}
