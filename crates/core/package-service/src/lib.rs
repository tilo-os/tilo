//! `package-service`: the runtime service for Tilo's native app model. The daemon is the privileged
//! service over the device's [`PackageStore`], and its actions are the one way the package store
//! changes while the device runs: installs, updates and removals, pins, images, runtime bundles,
//! trust, collection and launches, for the Store, Settings and the person's terminal (`app`,
//! `tilo-install`). It keeps no store of its own: packages, their registry and their payload bytes
//! live in the one package store the launcher and the shell read.
//!
//! Only the client side ([`client`], [`wire`], [`install::InstallExpectation`] and archive review
//! in [`local_package`]) builds without the default `service` feature, so the Store app never
//! compiles the privileged service it talks to.

#[cfg(feature = "service")]
pub mod change;
pub mod install;
#[cfg(feature = "service")]
pub mod language_pack;
#[cfg(feature = "service")]
pub mod service;
#[cfg(all(unix, feature = "test-support"))]
pub mod testing;
#[cfg(feature = "service")]
pub mod transport;

pub use package_service_client::{SERVICE, client, default_socket_path, wire};

#[cfg(feature = "service")]
pub use package_store::PackageStore;
pub use package_store::{Catalog, InstalledPackage, Pinning};
pub use tilo_app_package::PackageId;

/// The `package-service` interface's one version (crates/core/package-service/interface.toml), which the
/// service speaks to the apps it is handed to, on its own rather than the runtime interface's.
pub const PACKAGE_SERVICE_INTERFACE_VERSION: u32 = 7;

pub mod local_package;
