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
//! - [`fetch`] — remote fetch: resolve an app-id through the registry
//!   protocol (`app-registry`), download the tapp over iroh-blobs, verify
//!   it fail-closed, and install it.
//! - [`service`] — Cap'n Proto `AppHost` server implementation.
//! - [`transport`] — Unix-domain socket bind + accept loop (matches
//!   the shape used by `format-host`).
//! - [`client`] — the typed client the Store app uses.
//!
//! The crate exposes both a daemon binary (`tilo-app-host`) and the
//! library surface, so tests can drive the service in-process without
//! spawning the binary.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::path::PathBuf;

pub mod client;
pub mod dev_env;
pub mod fetch;
pub mod install;
pub mod service;
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
    unused_results,
    non_camel_case_types,
    missing_docs,
    dead_code,
    unsafe_code,
    reason = "generated Cap'n Proto bindings do not follow repository lint policy"
)]
pub mod app_host_capnp {
    include!("generated/app_host_capnp.rs");
}

pub use package_store::{Catalog, InstalledPackage, PackageStore, Pinning};
pub use tilo_app_package::PackageId;

/// Default socket path: `$XDG_RUNTIME_DIR/tilo/services/app-host.sock`.
pub fn default_socket_path() -> PathBuf {
    let base = xdg_paths::xdg::runtime_dir().unwrap_or_else(std::env::temp_dir);
    base.join("tilo").join("services").join("app-host.sock")
}
