//! `app-host` — the runtime service for Tilo's native app model.
//!
//! The daemon owns the local content-addressed package store at
//! `~/.local/share/tilo-apps/store/`, maintains a `Pinned` vs `LRU
//! (Tried)` registry, runs size-pressure GC, and exposes a Cap'n Proto
//! control socket the launcher (and a future App Tray UI) calls.
//!
//! ## Crate layout
//!
//! - [`store`] — on-disk content-addressed blob store + per-package
//!   eagerly-extracted payload tree. Wraps [`tilo_app_package::BlobStore`].
//! - [`registry`] — `Pinned` / `Tried` index, persisted as TOML.
//! - [`gc`] — LRU + size-pressure eviction.
//! - [`fetch`] — remote fetch: resolve an app-id through the registry
//!   protocol (`app-registry`), download the tapp over iroh-blobs,
//!   verify it fail-closed, and install it.
//! - [`install`] — parse a .tapp-shaped byte blob and ingest into the
//!   Store + Registry.
//! - [`service`] — Cap'n Proto `AppHost` server implementation.
//! - [`transport`] — Unix-domain socket bind + accept loop (matches
//!   the shape used by `format-host`).
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
pub mod gc;
pub mod install;
pub mod registry;
pub mod service;
pub mod store;
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
    include!(concat!(env!("OUT_DIR"), "/app_host_capnp.rs"));
}

pub use gc::GcReport;
pub use registry::{Pinning, RegistryEntry};
pub use store::{Store, StoreError};
pub use tilo_app_package::PackageId;

/// Default root for the on-disk store. Layout:
///
/// ```text
/// $root/
///   store/<hex>         # one file per content-addressed blob
///   payloads/<package-id-hex>/<payload-tree>
///   registry.toml       # the Pinning + lastUsedUnix index
/// ```
pub fn default_store_root() -> PathBuf {
    let base = xdg_paths::data_dir().unwrap_or_else(|| {
        // dirs gives `~/.local/share` on Linux; if that fails we
        // fall back to a per-user temp dir.
        std::env::temp_dir()
    });
    // Deliberately distinct from tilo-install's legacy `tilo-apps/registry.toml`:
    // the two registries have different schemas and sharing the path makes the
    // daemon fail at boot on any machine that used the old CLI.
    base.join("tilo-app-host")
}

/// Default socket path: `$XDG_RUNTIME_DIR/tilo/services/app-host.sock`.
pub fn default_socket_path() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
    base.join("tilo").join("services").join("app-host.sock")
}
