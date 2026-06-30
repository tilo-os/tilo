//! qOS format-router daemon — library surface. This commit is the skeleton:
//! the [`registry`] module that parses the on-disk `format-providers.toml`
//! file. Subsequent commits add the wasmtime component runner, the Cap'n
//! Proto wire, and the inotify hot-reload + main entry point.

pub mod registry;

pub use registry::{ProviderEntry, Registry};

use std::path::PathBuf;

/// Default on-disk registry path: `~/.config/qos/extensions/format-providers.toml`.
pub fn default_registry_path() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("qos").join("extensions").join("format-providers.toml")
}
