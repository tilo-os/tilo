//! The registries this machine pulls app entries from.
//!
//! One file, `registries.toml`, under `$XDG_CONFIG_HOME/qos-store/`.
//! That path is not a typo and not a new invention: it is where the
//! store app has always kept the user's registry list, and a second
//! list somewhere else would mean the desktop and the store disagreed
//! about what software exists. One list, two readers.
//!
//! Reading is deliberately split in two:
//!
//! * [`load_registries`] seeds a default file when none exists — what
//!   an app the user opened on purpose should do.
//! * [`load_registries_no_seed`] never writes. The compositor consults
//!   the catalogue on a timer, and a background timer that
//!   materialises config files in `~/.config` is a side effect nobody
//!   asked for.

use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("the registry name cannot be empty")]
    EmptyName,
    #[error("registry '{0}' already exists")]
    DuplicateName(String),
    #[error("registry '{0}' is not configured")]
    NotFound(String),
    #[error("the registry URL must be http or https")]
    InvalidUrl,
    #[error("the owner key must be 64 hexadecimal characters")]
    InvalidOwnerKey,
    #[error("could not determine the qOS Store config directory")]
    NoConfigDir,
    #[error("could not encode the registry configuration: {0}")]
    Encode(#[from] toml::ser::Error),
    #[error("could not save the registry configuration: {0}")]
    Write(#[from] std::io::Error),
}

/// Default hosted registry base URL. Overridable per-entry in the
/// config file; this is only used to seed the default `registries.toml`.
pub const DEFAULT_REGISTRY_BASE_URL: &str = "https://registry.qos.dev";

/// The placeholder owner pubkey a seeded file carries — 64 zeros, so
/// "not yet bootstrapped" is unambiguous.
pub fn placeholder_owner_pubkey() -> String {
    "00".repeat(32)
}

/// A single configured registry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryConfig {
    /// Human-readable short name, shown wherever an entry has to say
    /// where it came from.
    pub name: String,
    /// Base URL — `GET {base_url}/entries` returns the catalogue.
    pub base_url: String,
    /// Registry owner's ed25519 identity, hex-encoded. Optional so a
    /// fresh install can boot without pinning a key first; when unset
    /// the registry's signed entries are still readable but every one
    /// of them classifies as trust-on-first-use.
    #[serde(default)]
    pub owner_pubkey_hex: String,
    /// Users can silence a registry without removing it.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

/// Top-level shape of `registries.toml`. TOML's array-of-tables serde
/// form is `[[registry]] { … }` per entry.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistriesFile {
    #[serde(default, rename = "registry")]
    pub registries: Vec<RegistryConfig>,
}

impl RegistriesFile {
    /// The default file: one hosted-registry entry with a placeholder
    /// owner pubkey the user replaces on bootstrap.
    pub fn seeded() -> Self {
        Self {
            registries: vec![RegistryConfig {
                name: "qOS main".into(),
                base_url: DEFAULT_REGISTRY_BASE_URL.into(),
                owner_pubkey_hex: placeholder_owner_pubkey(),
                enabled: true,
            }],
        }
    }

    /// Just the registries the user has left switched on.
    pub fn enabled(&self) -> impl Iterator<Item = &RegistryConfig> {
        self.registries.iter().filter(|r| r.enabled)
    }
}

/// Root config dir. `QOS_STORE_CONFIG_DIR` overrides for tests.
pub fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("QOS_STORE_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }
    dirs::config_dir().map(|d| d.join("qos-store"))
}

/// Cache dir (`$XDG_CACHE_HOME/qos-store`). Downloaded `.qpkg`s are
/// staged here before they are handed to the installer.
pub fn cache_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("QOS_STORE_CACHE_DIR") {
        return Some(PathBuf::from(dir));
    }
    dirs::cache_dir().map(|d| d.join("qos-store"))
}

/// The path `registries.toml` lives at, when a config dir resolves.
pub fn registries_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("registries.toml"))
}

/// Read `registries.toml` without ever writing one.
///
/// A missing file means "no registries configured", which is a real
/// and correct answer, not an error: a machine that has not been
/// pointed at a registry has nothing available to offer.
pub fn load_registries_no_seed() -> RegistriesFile {
    let Some(path) = registries_path() else {
        return RegistriesFile::default();
    };
    let Ok(text) = fs::read_to_string(&path) else {
        return RegistriesFile::default();
    };
    toml::from_str::<RegistriesFile>(&text).unwrap_or_default()
}

/// Read `registries.toml`, seeding a default file if none exists.
///
/// The seed is written to disk on first read so users can hand-edit
/// it. If the write fails (e.g. a read-only home in a sandbox) we
/// still return the seeded shape from memory.
pub fn load_registries() -> RegistriesFile {
    let Some(dir) = config_dir() else {
        return RegistriesFile::seeded();
    };
    let path = dir.join("registries.toml");
    if let Ok(text) = fs::read_to_string(&path)
        && let Ok(parsed) = toml::from_str::<RegistriesFile>(&text)
    {
        return parsed;
    }
    let seeded = RegistriesFile::seeded();
    if let Ok(text) = toml::to_string_pretty(&seeded) {
        let _ = fs::create_dir_all(&dir);
        let _ = fs::write(&path, text);
    }
    seeded
}

/// Durably replace the shared Store/catalogue registry configuration.
///
/// # Errors
///
/// Returns an error when no config directory is available, serialization
/// fails, or the crash-safe write cannot be completed.
pub fn save_registries(registries: &RegistriesFile) -> Result<(), ConfigError> {
    let path = registries_path().ok_or(ConfigError::NoConfigDir)?;
    let text = toml::to_string_pretty(registries)?;
    atomic_file::write_creating_parent(&path, text.as_bytes())?;
    Ok(())
}

/// Add a registry after validating the fields users most often mistype.
///
/// # Errors
///
/// Returns an error for an empty/duplicate name, a non-HTTP(S) URL, or a
/// malformed non-empty owner key.
pub fn add_registry(
    registries: &mut RegistriesFile,
    name: &str,
    base_url: &str,
    owner_pubkey_hex: &str,
) -> Result<(), ConfigError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(ConfigError::EmptyName);
    }
    if registries.registries.iter().any(|r| r.name == name) {
        return Err(ConfigError::DuplicateName(name.to_string()));
    }
    let url = reqwest::Url::parse(base_url).map_err(|_| ConfigError::InvalidUrl)?;
    if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
        return Err(ConfigError::InvalidUrl);
    }
    if !owner_pubkey_hex.is_empty()
        && (owner_pubkey_hex.len() != 64 || hex::decode(owner_pubkey_hex).is_err())
    {
        return Err(ConfigError::InvalidOwnerKey);
    }
    registries.registries.push(RegistryConfig {
        name: name.to_string(),
        base_url: base_url.trim_end_matches('/').to_string(),
        owner_pubkey_hex: owner_pubkey_hex.to_ascii_lowercase(),
        enabled: true,
    });
    Ok(())
}

/// Remove the registry named `name`.
///
/// # Errors
///
/// Returns [`ConfigError::NotFound`] when there is no exact name match.
pub fn remove_registry(registries: &mut RegistriesFile, name: &str) -> Result<(), ConfigError> {
    let before = registries.registries.len();
    registries.registries.retain(|r| r.name != name);
    if registries.registries.len() == before {
        return Err(ConfigError::NotFound(name.to_string()));
    }
    Ok(())
}

/// Enable or disable the registry named `name`.
///
/// # Errors
///
/// Returns [`ConfigError::NotFound`] when there is no exact name match.
pub fn set_registry_enabled(
    registries: &mut RegistriesFile,
    name: &str,
    enabled: bool,
) -> Result<(), ConfigError> {
    let registry = registries
        .registries
        .iter_mut()
        .find(|r| r.name == name)
        .ok_or_else(|| ConfigError::NotFound(name.to_string()))?;
    registry.enabled = enabled;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard, OnceLock};
    use tempfile::TempDir;

    /// Serialises the env-var-touching tests in this module — cargo's
    /// parallel runner otherwise races on `QOS_STORE_CONFIG_DIR`.
    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    struct Guard<'a> {
        _lock: MutexGuard<'a, ()>,
        prev: Option<std::ffi::OsString>,
    }
    impl Drop for Guard<'_> {
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
        assert_eq!(
            regs.enabled().count(),
            0,
            "a disabled registry is not consulted"
        );
    }

    /// The compositor's read must not materialise a config file — a
    /// background timer that writes `~/.config` is a side effect the
    /// user never asked for.
    #[test]
    fn no_seed_read_never_writes_a_file() {
        let td = TempDir::new().unwrap();
        let _g = scoped_dir(&td);
        let regs = load_registries_no_seed();
        assert!(
            regs.registries.is_empty(),
            "nothing configured means nothing available"
        );
        assert!(
            !td.path().join("registries.toml").exists(),
            "the no-seed read must leave the config dir untouched"
        );
    }

    #[test]
    fn no_seed_read_still_reads_an_existing_file() {
        let td = TempDir::new().unwrap();
        let _g = scoped_dir(&td);
        std::fs::write(
            td.path().join("registries.toml"),
            "[[registry]]\nname = \"local\"\nbase_url = \"http://127.0.0.1:9\"\n",
        )
        .unwrap();
        let regs = load_registries_no_seed();
        assert_eq!(regs.registries.len(), 1);
        assert_eq!(regs.registries[0].name, "local");
        assert!(regs.registries[0].enabled, "enabled defaults to true");
    }

    #[test]
    fn registry_mutations_validate_and_roundtrip() {
        let td = TempDir::new().unwrap();
        let _g = scoped_dir(&td);
        let mut regs = RegistriesFile::default();
        add_registry(
            &mut regs,
            "team",
            "https://apps.example.test/",
            &"ab".repeat(32),
        )
        .unwrap();
        assert_eq!(regs.registries[0].base_url, "https://apps.example.test");
        set_registry_enabled(&mut regs, "team", false).unwrap();
        save_registries(&regs).unwrap();
        assert_eq!(load_registries(), regs);
        remove_registry(&mut regs, "team").unwrap();
        assert!(regs.registries.is_empty());
    }

    #[test]
    fn registry_mutations_reject_ambiguous_input() {
        let mut regs = RegistriesFile::default();
        assert!(matches!(
            add_registry(&mut regs, "", "https://example.test", ""),
            Err(ConfigError::EmptyName)
        ));
        assert!(matches!(
            add_registry(&mut regs, "x", "file:///tmp/apps", ""),
            Err(ConfigError::InvalidUrl)
        ));
        assert!(matches!(
            add_registry(&mut regs, "x", "https://example.test", "bad"),
            Err(ConfigError::InvalidOwnerKey)
        ));
    }
}
