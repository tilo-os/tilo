//! Parser for `$XDG_DATA_HOME/qos-apps/registry.toml` — the file
//! `qos-install` maintains after every install / remove.
//!
//! We deliberately **do not** call into `install::` for this: the
//! install crate ships that shape as private types inside a binary
//! crate (`main.rs`, not `lib.rs`), so importing it would require
//! moving them up-crate. Parsing the exact same file into our own
//! shape is safer and keeps the store isolated — a schema change in
//! `qos-install` would surface here as a `serde` unknown-field error
//! instead of silently mangling our state.
//!
//! `serde(default)` on every field means an older / newer file that
//! omits or adds a field still parses; the store only needs `id +
//! version` to render the "Installed" state on a card.

use std::path::PathBuf;

use serde::Deserialize;

/// Path to the installed-package registry file. `QOS_APPS_DATA_DIR`
/// wins if set (matches `qos-install`'s override). Otherwise
/// `$XDG_DATA_HOME/qos-apps/registry.toml`.
pub fn registry_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("QOS_APPS_DATA_DIR") {
        return Some(PathBuf::from(dir).join("registry.toml"));
    }
    dirs::data_dir().map(|d| d.join("qos-apps").join("registry.toml"))
}

/// One row of `qos-install`'s registry.toml. Only the fields the
/// store's UI needs; everything else is discarded via `#[serde(default)]`
/// blanket-accept-unknown.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct InstalledEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub publisher_key_hex: String,
    #[serde(default)]
    pub installed_at: u64,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct RegistryFile {
    #[serde(default)]
    entry: Vec<InstalledEntry>,
}

/// Read the installed-set snapshot. Missing / malformed → empty list
/// (the store degrades to "nothing installed" — never crashes on a
/// broken registry.toml).
pub fn load_installed() -> Vec<InstalledEntry> {
    let Some(path) = registry_path() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    toml::from_str::<RegistryFile>(&text)
        .map(|r| r.entry)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use tempfile::TempDir;

    /// Serialise the env-var writes across the two tests in this
    /// module so cargo's parallel runner can't race the setter.
    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    struct Guard<'a> {
        _lock: MutexGuard<'a, ()>,
        prev: Option<std::ffi::OsString>,
    }
    impl<'a> Drop for Guard<'a> {
        fn drop(&mut self) {
            match &self.prev {
                Some(v) => unsafe { std::env::set_var("QOS_APPS_DATA_DIR", v) },
                None => unsafe { std::env::remove_var("QOS_APPS_DATA_DIR") },
            }
        }
    }

    fn scoped(td: &TempDir) -> Guard<'static> {
        let lock = env_lock();
        let prev = std::env::var_os("QOS_APPS_DATA_DIR");
        unsafe { std::env::set_var("QOS_APPS_DATA_DIR", td.path()) };
        Guard { _lock: lock, prev }
    }

    #[test]
    fn missing_registry_yields_empty() {
        let td = TempDir::new().unwrap();
        let _g = scoped(&td);
        assert!(load_installed().is_empty());
    }

    #[test]
    fn parses_qos_install_shape_including_unknown_fields() {
        let td = TempDir::new().unwrap();
        let _g = scoped(&td);
        let text = r#"
[[entry]]
id = "dev.qos.notes"
version = "0.1.0"
package_id_hex = "abcdef"
payload_root = "/var/tmp/notes"
manifest_path = "/var/tmp/notes/qos-app.toml"
publisher_key_hex = "aa"
pinned = true
installed_at = 1_800_000_000
attested_by = ""
cli_commands = []

[[entry]]
id = "dev.qos.terminal"
version = "0.2.0"
package_id_hex = "beadfe"
payload_root = "/var/tmp/term"
manifest_path = "/var/tmp/term/qos-app.toml"
publisher_key_hex = "bb"
pinned = false
installed_at = 1_800_000_001
"#;
        std::fs::write(td.path().join("registry.toml"), text).unwrap();
        let entries = load_installed();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, "dev.qos.notes");
        assert_eq!(entries[0].version, "0.1.0");
        assert_eq!(entries[1].id, "dev.qos.terminal");
    }
}
