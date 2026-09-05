//! `$XDG_CONFIG_HOME/qos-store/` on-disk state.
//!
//! Two files, and only one of them is defined here:
//!
//!   * `registries.toml` — the list of qOS registries the user pulls
//!     entries from. **Owned by [`app_catalogue::config`]**, not by
//!     this app. The desktop's search box reads the same file to decide
//!     what software the machine could open, so a second definition of
//!     its shape would eventually mean the store and the palette
//!     disagreed about which registries exist. One reader had to become
//!     the owner; the shared crate is it, because it is the one both
//!     the app and the compositor can depend on.
//!   * `config.toml` — a UI-settings file that nothing in the app reads
//!     or writes. It was never wired up, and the UI state it describes
//!     now lives in the app's Automerge document, which `ui::persist`
//!     saves and restores. [`load_config`] / [`save_config`] remain only
//!     so an existing file can still be read by whoever wants it.
//!
//! Everything here is best-effort I/O: a missing / malformed file is
//! never fatal — the UI falls back to defaults and keeps rendering.

use std::fs;

use serde::{Deserialize, Serialize};

use crate::model::SourceFilter;

// The registry list, its file format and the two directories it lives
// under, re-exported so existing `store::config::…` paths keep working
// and there is still exactly one definition of each.
pub use app_catalogue::config::{
    DEFAULT_REGISTRY_BASE_URL, RegistriesFile, RegistryConfig, cache_dir, config_dir,
    load_registries, placeholder_owner_pubkey, registries_path,
};

/// Persistent UI state — the filter and search bar are the two things
/// worth restoring across restarts. Everything else lives in-memory.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct StoreConfig {
    #[serde(default)]
    pub last_filter: Option<SourceFilter>,
    #[serde(default)]
    pub last_query: Option<String>,
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
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    struct Guard<'a> {
        _lock: MutexGuard<'a, ()>,
        prev: Option<std::ffi::OsString>,
        prev_official: Option<std::ffi::OsString>,
    }
    impl Drop for Guard<'_> {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => unsafe { std::env::set_var("QOS_STORE_CONFIG_DIR", v) },
                None => unsafe { std::env::remove_var("QOS_STORE_CONFIG_DIR") },
            }
            match &self.prev_official {
                Some(v) => unsafe { std::env::set_var("QOS_OFFICIAL_REGISTRY_OWNER_KEY_PATH", v) },
                None => unsafe { std::env::remove_var("QOS_OFFICIAL_REGISTRY_OWNER_KEY_PATH") },
            }
        }
    }

    fn scoped_dir(td: &TempDir) -> Guard<'static> {
        let lock = env_lock();
        let prev = std::env::var_os("QOS_STORE_CONFIG_DIR");
        let prev_official = std::env::var_os("QOS_OFFICIAL_REGISTRY_OWNER_KEY_PATH");
        let trust = td.path().join("registry-owner.pub");
        std::fs::write(
            &trust,
            "d5dd5851bebac876b19986485184213ead36b67595b7efedf1144c120033b0bb\n",
        )
        .unwrap();
        unsafe { std::env::set_var("QOS_STORE_CONFIG_DIR", td.path()) };
        unsafe { std::env::set_var("QOS_OFFICIAL_REGISTRY_OWNER_KEY_PATH", trust) };
        Guard {
            _lock: lock,
            prev,
            prev_official,
        }
    }

    /// The store and the desktop have to resolve `registries.toml` to
    /// the same path, or "what this machine can install" and "what this
    /// machine can open" become two different answers. Asserted here
    /// rather than left to the re-export being obviously correct: the
    /// re-export is what someone would undo first when adding a
    /// store-only setting.
    #[test]
    fn the_store_reads_the_same_registry_file_the_desktop_does() {
        let td = TempDir::new().unwrap();
        let _g = scoped_dir(&td);
        assert_eq!(
            registries_path(),
            app_catalogue::config::registries_path(),
            "one registry list, two readers"
        );
        assert_eq!(config_dir().as_deref(), Some(td.path()));
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
