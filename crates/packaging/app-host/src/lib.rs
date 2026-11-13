//! `app-host`: the runtime service for Tilo's native app model. The daemon is the privileged
//! door to the device's [`PackageStore`], installing verified packages for the Store app, pinning,
//! removing and launching them, running size-pressure eviction, and serving a control socket. It
//! keeps no store of its own: packages, their registry and their payload bytes live in the one
//! package store the launcher, the shell and `tilo-install` share.
//!
//! Only the client side ([`client`], [`wire`], [`install::InstallExpectation`] and archive review
//! in [`local_package`]) builds without the default `service` feature, so the Store app never
//! compiles the privileged door it talks to.

use std::path::PathBuf;

pub mod client;
pub mod install;
#[cfg(feature = "service")]
pub mod runtime_request;
#[cfg(feature = "service")]
pub mod service;
#[cfg(all(unix, feature = "test-support"))]
pub mod testing;
#[cfg(feature = "service")]
pub mod transport;

pub mod wire;

#[cfg(feature = "service")]
pub use package_store::PackageStore;
pub use package_store::{Catalog, InstalledPackage, Pinning};
pub use tilo_app_package::PackageId;

/// The `app-host` interface's one version (crates/packaging/app-host/interface.toml), which the
/// service speaks to the apps it is handed to, on its own rather than the runtime interface's.
pub const APP_HOST_INTERFACE_VERSION: u32 = 5;

/// `$XDG_RUNTIME_DIR/tilo/services/app-host.sock`, or `None` without a runtime directory. Never
/// the shared temp directory, where any local user could bind the name first.
pub fn default_socket_path() -> Option<PathBuf> {
    tilo_paths::xdg::runtime_dir()
        .map(|base| base.join("tilo").join("services").join("app-host.sock"))
}

pub mod local_package;
