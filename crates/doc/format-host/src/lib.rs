//! Tilo format-router daemon: provider registry, sandboxed provider runner,
//! Cap'n Proto service, and registry hot reload.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

/// Generated Cap'n Proto bindings for `format-router.capnp`. Lives at the
/// crate root because the capnpc output references `crate::format_router_capnp::*`.
#[allow(
    clippy::all,
    clippy::nursery,
    clippy::pedantic,
    clippy::restriction,
    dead_code,
    unused,
    reason = "generated Cap'n Proto bindings do not follow repository lint policy"
)]
pub mod format_router_capnp {
    include!("generated/format_router_capnp.rs");
}
pub mod registry;
pub mod router;
pub mod runner;
pub mod service;
pub mod transport;
pub mod watch;

pub use registry::{ProviderEntry, Registry};
pub use router::{ActiveProvider, Router};
pub use runner::{DEFAULT_FUEL, Limits, LoadedProvider, MAX_MEMORY_BYTES};

use std::path::PathBuf;

/// Default on-disk registry path: `~/.config/tilo/extensions/format-providers.toml`,
/// or `None` without a config directory. Never a shared directory: whoever can
/// write the registry chooses which providers parse this user's files.
pub fn default_registry_path() -> Option<PathBuf> {
    tilo_paths::config_dir().map(|base| {
        base.join("tilo")
            .join("extensions")
            .join("format-providers.toml")
    })
}
