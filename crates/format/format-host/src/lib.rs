//! qOS format-router daemon — library surface. This commit is the skeleton:
//! the [`registry`] module that parses the on-disk `format-providers.toml`
//! file. Subsequent commits add the wasmtime component runner, the Cap'n
//! Proto wire, and the inotify hot-reload + main entry point.

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        reason = "a test that unwraps is stating an invariant"
    )
)]

/// Generated Cap'n Proto bindings for `format-router.capnp`. Lives at the
/// crate root because the capnpc output references `crate::format_router_capnp::*`.
pub mod format_router_capnp {
    include!(concat!(env!("OUT_DIR"), "/format_router_capnp.rs"));
}
pub mod registry;
pub mod router;
pub mod runner;
pub mod service;
pub mod transport;
pub mod watch;

pub use registry::{ProviderEntry, Registry};
pub use router::{ActiveProvider, Router};
pub use runner::{LoadedComponent, MAX_MEMORY_BYTES, MAX_WALLCLOCK};

use std::path::PathBuf;

/// Default on-disk registry path: `~/.config/qos/extensions/format-providers.toml`.
pub fn default_registry_path() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("qos")
        .join("extensions")
        .join("format-providers.toml")
}
