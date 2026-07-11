//! `$XDG_CONFIG_HOME/qos-store/` on-disk state.
//!
//! Two files:
//!
//!   * `registries.toml` — the list of qOS registries the user pulls
//!     entries from. Ships one default entry pointing at the hosted
//!     "qOS main" registry, with a placeholder owner pubkey the user
//!     replaces on bootstrap (a signed registry index — the first
//!     entry.updated_at_unix > 0 index sig — pins the owner key).
//!   * `config.toml` — persistent UI settings (currently: which filter
//!     chip was active last, and the last search string, so the store
//!     opens in the state the user left it).
//!
//! Everything here is best-effort I/O: a missing / malformed file is
//! never fatal — the UI falls back to defaults and keeps rendering.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::model::SourceFilter;

/// Default hosted registry base URL. Overridable per-entry in the
/// config file; this is only used to seed the default `registries.toml`.
pub const DEFAULT_REGISTRY_BASE_URL: &str = "https://registry.qos.dev";

/// A single configured registry the store pulls entries from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryConfig {
    /// Human-readable short name shown on cards and in the detail
    /// sheet.
    pub name: String,
    /// Base URL — `GET {base_url}/entries` returns the catalogue.
    pub base_url: String,
    /// Registry owner's ed25519 identity, hex-encoded. Optional so a
    /// fresh install can boot without pinning a key first; when
    /// unset the registry's signed entries are still visible but
    /// the store surfaces a "trust-on-first-use" state on the
    /// detail sheet.
    #[serde(default)]
    pub owner_pubkey_hex: String,
    /// Users can silence a registry without removing it.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

/// Top-level shape of `registries.toml`. TOML's array-of-tables
/// serde form is `[[registry]] { … }` per entry.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistriesFile {
    #[serde(default, rename = "registry")]
    pub registries: Vec<RegistryConfig>,
}

impl RegistriesFile {
    /// The default file: one hosted-registry entry with a placeholder
    /// owner pubkey the user replaces via `qos-store registry pin
    /// <hex>` (a follow-up CLI, not shipped here). The placeholder
    /// is 64 zeros so the "not yet bootstrapped" state is
    /// unambiguous.
    pub fn seeded() -> Self {
        Self {
            registries: vec![RegistryConfig {
                name: "qOS main".into(),
                base_url: DEFAULT_REGISTRY_BASE_URL.into(),
                owner_pubkey_hex: "00".repeat(32),
                enabled: true,
            }],
        }
    }
}

/// Persistent UI state — the filter and search bar are the two things
/// worth restoring across restarts. Everything else lives in-memory.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StoreConfig {
    #[serde(default)]
    pub last_filter: Option<SourceFilter>,
    #[serde(default)]
    pub last_query: Option<String>,
}

/// Root config dir. `QOS_STORE_CONFIG_DIR` overrides for tests.
pub fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("QOS_STORE_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }
    dirs::config_dir().map(|d| d.join("qos-store"))
}

/// Cache dir (`$XDG_CACHE_HOME/qos-store`). Registry responses are
/// serialized here for offline use.
pub fn cache_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("QOS_STORE_CACHE_DIR") {
        return Some(PathBuf::from(dir));
    }
    dirs::cache_dir().map(|d| d.join("qos-store"))
}

/// Read `registries.toml`, seeding a default file if none exists.
///
/// The seed is written to disk on first read so users can hand-edit
/// it. If the write fails (e.g. read-only home in a sandbox mode)
/// we still return the seeded shape from memory so the store
/// remains usable.
pub fn load_registries() -> RegistriesFile {
    let Some(dir) = config_dir() else {
        return RegistriesFile::seeded();
    };
    let path = dir.join("registries.toml");
    if let Ok(text) = fs::read_to_string(&path) {
        if let Ok(parsed) = toml::from_str::<RegistriesFile>(&text) {
            return parsed;
        }
    }
    let seeded = RegistriesFile::seeded();
    if let Ok(text) = toml::to_string_pretty(&seeded) {
        let _ = fs::create_dir_all(&dir);
        let _ = fs::write(&path, text);
    }
    seeded
}

/// Read `config.toml` — never fails, missing / malformed → default.
pub fn load_config() -> StoreConfig {
    config_dir()
        .and_then(|d| fs::read_to_string(d.join("config.toml")).ok())
        .and_then(|t| toml::from_str::<StoreConfig>(&t).ok())
        .unwrap_or_default()
}

/// Write `config.toml`. Best-effort — silently returns on failure.
pub fn save_config(cfg: &StoreConfig) {
    let Some(dir) = config_dir() else { return };
    let _ = fs::create_dir_all(&dir);
    if let Ok(text) = toml::to_string_pretty(cfg) {
        let _ = fs::write(dir.join("config.toml"), text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use tempfile::TempDir;

    /// Shared lock used to serialise the env-var-touching tests in
    /// this module. Without it, cargo's default parallel test
    /// runner races on `QOS_STORE_CONFIG_DIR` and the assertions
    /// see a value another test set.
    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            // A poisoned lock only means a previous test panicked; we
            // still want to run — clear the poison and continue.
            .unwrap_or_else(|e| e.into_inner())
    }

    struct Guard<'a> {
        _lock: MutexGuard<'a, ()>,
        prev: Option<std::ffi::OsString>,
    }
    impl<'a> Drop for Guard<'a> {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => unsafe { std::env::set_var("QOS_STORE_CONFIG_DIR", v) },
                None => unsafe { std::env::remove_var("QOS_STORE_CONFIG_DIR") },
            }
        }
    }

    fn scoped_dir(td: &TempDir) -> Guard<'static> {
        let lock = env_lock();
        let prev = std::env::var_os("QOS_STORE_CONFIG_DIR");
        unsafe { std::env::set_var("QOS_STORE_CONFIG_DIR", td.path()) };
        Guard { _lock: lock, prev }
    }

    #[test]
    fn seed_written_when_missing() {
        let td = TempDir::new().unwrap();
        let _g = scoped_dir(&td);
        let regs = load_registries();
        assert_eq!(regs.registries.len(), 1);
        assert_eq!(regs.registries[0].name, "qOS main");
        assert!(td.path().join("registries.toml").exists());
    }

    #[test]
    fn hand_edited_registries_roundtrip() {
        let td = TempDir::new().unwrap();
        let _g = scoped_dir(&td);
        let text = r#"
[[registry]]
name = "my mirror"
base_url = "https://mirror.example.com"
owner_pubkey_hex = "ab"
enabled = false
"#;
        std::fs::write(td.path().join("registries.toml"), text).unwrap();
        let regs = load_registries();
        assert_eq!(regs.registries.len(), 1);
        assert_eq!(regs.registries[0].name, "my mirror");
        assert!(!regs.registries[0].enabled);
    }

    #[test]
    fn config_roundtrip() {
        let td = TempDir::new().unwrap();
        let _g = scoped_dir(&td);
        let cfg = StoreConfig {
            last_filter: Some(SourceFilter::Qos),
            last_query: Some("term".into()),
        };
        save_config(&cfg);
        let back = load_config();
        assert_eq!(back.last_filter, Some(SourceFilter::Qos));
        assert_eq!(back.last_query.as_deref(), Some("term"));
    }
}
