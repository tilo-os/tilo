//! `app-host` — the runtime backbone for qOS's native app model.
//!
//! The daemon owns the local content-addressed package store at
//! `~/.local/share/qos-apps/store/`, maintains a `Pinned` vs `LRU
//! (Tried)` registry, runs size-pressure GC, and exposes a Cap'n Proto
//! control socket the launcher (and a future App Tray UI) calls.
//!
//! ## Crate layout
//!
//! - [`store`] — on-disk content-addressed blob store + per-package
//!   eagerly-extracted payload tree. Wraps [`pkg::BlobStore`].
//! - [`registry`] — `Pinned` / `Tried` index, persisted as TOML.
//! - [`gc`] — LRU + size-pressure eviction.
//! - [`fetch`] — remote fetch: resolve an app-id through the registry
//!   protocol (`app-registry`), download the qpkg over iroh-blobs,
//!   verify it fail-closed, and install it.
//! - [`install`] — parse a .qpkg-shaped byte blob and ingest into the
//!   Store + Registry.
//! - [`service`] — Cap'n Proto `AppHost` server implementation.
//! - [`transport`] — Unix-domain socket bind + accept loop (matches
//!   the shape used by `format-host`).
//!
//! The crate exposes both a daemon binary (`qos-app-host`) and the
//! library surface, so tests can drive the service in-process without
//! spawning the binary.

use std::path::PathBuf;

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
    clippy::pedantic,
    unused_imports,
    unused_qualifications,
    unused_results,
    non_camel_case_types,
    missing_docs,
    dead_code,
    unsafe_code
)]
pub mod app_host_capnp {
    include!(concat!(env!("OUT_DIR"), "/app_host_capnp.rs"));
}

pub use gc::GcReport;
pub use registry::{Pinning, RegistryEntry};
pub use store::{Store, StoreError};

/// Default root for the on-disk store. Layout:
///
/// ```text
/// $root/
///   store/<hex>         # one file per content-addressed blob
///   payloads/<package-id-hex>/<payload-tree>
///   registry.toml       # the Pinning + lastUsedUnix index
/// ```
pub fn default_store_root() -> PathBuf {
    let base = dirs::data_dir().unwrap_or_else(|| {
        // dirs gives `~/.local/share` on Linux; if that fails we
        // fall back to a per-user temp dir.
        std::env::temp_dir()
    });
    base.join("qos-apps")
}

/// Default socket path: `$XDG_RUNTIME_DIR/qos/services/app-host.sock`.
pub fn default_socket_path() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
    base.join("qos").join("services").join("app-host.sock")
}
