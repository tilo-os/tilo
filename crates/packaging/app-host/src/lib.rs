//! `app-host` — the runtime service for Tilo's native app model.
//!
//! The daemon is the privileged door to the device's [`PackageStore`]:
//! it installs verified packages on behalf of the Store app, pins,
//! removes and launches them, runs size-pressure eviction, and exposes a
//! Cap'n Proto control socket. It keeps no store of its own — packages,
//! their registry and their payload bytes all live in the one package
//! store that the launcher, the shell and `tilo-install` share.
//!
//! ## Crate layout
//!
//! - [`install`] — admit a `.tapp` against the catalogue identity the Store
//!   capability approved, and install it into the package store.
//! - [`service`] — Cap'n Proto `AppHost` server: resolves a Store request
//!   through the same owner-signed catalogue, version index and verified
//!   download the `app` command uses, then installs the exact package the
//!   catalogue named.
//! - [`transport`] — Unix-domain socket bind + accept loop (matches
//!   the shape used by `format-host`).
//! - [`client`] — the typed client the Store app uses.
//!
//! The crate exposes both a daemon binary (`tilo-app-host`) and the
//! library surface, so tests can drive the service in-process without
//! spawning the binary. Only the client side — [`client`], the wire schema,
//! [`install::InstallExpectation`] and archive review in [`local_package`] —
//! builds without the default `service` feature. The Store app depends on
//! that side alone, so an app never compiles the privileged door it talks to
//! and the shared app runtime does not carry it.

use std::path::PathBuf;

pub mod client;
pub mod install;
#[cfg(feature = "service")]
pub mod runtime_request;
pub mod runtime_transaction;
#[cfg(feature = "service")]
pub mod service;
#[cfg(all(unix, feature = "test-support"))]
pub mod testing;
#[cfg(feature = "service")]
pub mod transport;

/// Generated Cap'n Proto bindings for `schema/app-host.capnp`. The
/// generated module is named after the schema file (kebab → snake).
#[allow(
    clippy::all,
    clippy::nursery,
    clippy::pedantic,
    clippy::restriction,
    unused_imports,
    unused_qualifications,
    missing_docs,
    dead_code,
    reason = "generated Cap'n Proto bindings do not follow repository lint policy"
)]
pub mod app_host_capnp {
    include!("generated/app_host_capnp.rs");
}

#[cfg(feature = "service")]
pub use package_store::PackageStore;
pub use package_store::{Catalog, InstalledPackage, Pinning};
pub use tilo_app_package::PackageId;

/// The app-host interface's one version: what the `app-host` service speaks
/// to the apps it is handed to (the Store, the Browser, Settings), on its own
/// rather than the runtime interface's (crates/packaging/app-host/interface.toml).
pub const APP_HOST_INTERFACE_VERSION: u32 = 3;

/// Default socket path: `$XDG_RUNTIME_DIR/tilo/services/app-host.sock`, or
/// `None` without a runtime directory. Never the shared temp directory, where
/// any local user could bind the name first.
pub fn default_socket_path() -> Option<PathBuf> {
    tilo_paths::xdg::runtime_dir()
        .map(|base| base.join("tilo").join("services").join("app-host.sock"))
}

pub mod local_package;
