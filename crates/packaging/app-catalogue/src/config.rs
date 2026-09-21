//! The registries this machine pulls app entries from.
//!
//! One file, `registries.toml`, under `$XDG_CONFIG_HOME/tilo-store/`.
//! The store app and desktop read this same list so they agree about
//! which software is available.
//!
//! Reading is split in two:
//!
//! * [`load_registries`] seeds a default file when none exists — what
//!   an app the user opened on purpose should do.
//! * [`load_registries_no_seed`] never writes. The compositor consults
//!   the catalogue on a timer, and a background timer that
//!   materialises config files in `~/.config` as a side effect.

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
    #[error("the registry URL must be credential-free HTTPS (or loopback HTTP for development)")]
    InvalidUrl,
    #[error("a non-placeholder owner key of 64 hexadecimal characters is required")]
    InvalidOwnerKey,
    #[error("could not determine the Tilo Store config directory")]
    NoConfigDir,
    #[error("could not encode the registry configuration: {0}")]
    Encode(#[from] toml::ser::Error),
    #[error("could not save the registry configuration: {0}")]
    Write(#[from] std::io::Error),
}

/// Default hosted registry base URL. Overridable per-entry in the
/// config file; this is only used to seed the default `registries.toml`.
pub const DEFAULT_REGISTRY_BASE_URL: &str = "https://registry.tilo.dev";

/// Pinned Tilo-main owner identity. Installed images can rotate the public trust
/// input without rebuilding app packages. A missing/placeholder value disables
/// the default registry rather than substituting a developer key or TOFU.
pub fn official_owner_pubkey() -> Option<String> {
    let path = std::env::var_os("TILO_OFFICIAL_REGISTRY_OWNER_KEY_PATH").map_or_else(
        || PathBuf::from("/usr/share/tilo/trust/tilo-main-registry-owner.pub"),
        PathBuf::from,
    );
    let value = fs::read_to_string(path).ok()?;
    let value = value.trim().to_ascii_lowercase();
    (value.len() == 64
        && value != placeholder_owner_pubkey()
        && hex::decode(&value).is_ok_and(|bytes| bytes.len() == 32))
    .then_some(value)
}

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
    /// Registry owner's pinned Ed25519 identity, hex-encoded. Missing keys
    /// fail closed; registry discovery never silently creates trust.
    #[serde(default)]
    pub owner_pubkey_hex: String,
    /// Users can silence a registry without removing it.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Stable by default; development explicitly opts into prerelease apps.
    #[serde(default)]
    pub channel: pkg::version::ReleaseChannel,
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
    /// The default file: one hosted registry pinned to the OS trust input.
    /// If packaging omitted/corrupted that input, no default is enabled.
    pub fn seeded() -> Self {
        Self {
            registries: official_owner_pubkey().map_or_else(Vec::new, |owner_pubkey_hex| {
                vec![RegistryConfig {
                    name: "Tilo main".into(),
                    base_url: DEFAULT_REGISTRY_BASE_URL.into(),
                    owner_pubkey_hex,
                    enabled: true,
                    channel: pkg::version::ReleaseChannel::Stable,
                }]
            }),
        }
    }

    /// The registries the user has left switched on.
    pub fn enabled(&self) -> impl Iterator<Item = &RegistryConfig> {
        self.registries.iter().filter(|r| r.enabled)
    }
}

/// Root config dir. `TILO_STORE_CONFIG_DIR` overrides for tests.
pub fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("TILO_STORE_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }
    dirs::config_dir().map(|d| d.join("tilo-store"))
}

/// Cache dir (`$XDG_CACHE_HOME/tilo-store`). Downloaded `.qpkg`s are
/// staged here before they are handed to the installer.
pub fn cache_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("TILO_STORE_CACHE_DIR") {
        return Some(PathBuf::from(dir));
    }
    dirs::cache_dir().map(|d| d.join("tilo-store"))
}

/// The path `registries.toml` lives at, when a config dir resolves.
pub fn registries_path() -> Option<PathBuf> {
    config_dir().map(|d| d.join("registries.toml"))
}

/// Read `registries.toml` without ever writing one.
///
/// A missing file means "no registries configured", which is a valid
/// answer rather than an error: a machine that has not been
/// pointed at a registry has nothing available to offer.
pub fn load_registries_no_seed() -> RegistriesFile {
    let Some(path) = registries_path() else {
        return RegistriesFile::default();
    };
    let Ok(text) = fs::read_to_string(&path) else {
        return RegistriesFile::default();
    };
    let mut registries = toml::from_str::<RegistriesFile>(&text).unwrap_or_default();
    migrate_official_pin(&mut registries);
    registries
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
        && let Ok(mut parsed) = toml::from_str::<RegistriesFile>(&text)
    {
        if migrate_official_pin(&mut parsed)
            && let Ok(updated) = toml::to_string_pretty(&parsed)
        {
            let _ = atomic_file::write(&path, updated.as_bytes());
        }
        return parsed;
    }
    let seeded = RegistriesFile::seeded();
    if let Ok(text) = toml::to_string_pretty(&seeded) {
        let _ = fs::create_dir_all(&dir);
        let _ = fs::write(&path, text);
    }
    seeded
}

/// Replace only the exact legacy Tilo-main placeholder with the immutable OS
/// trust input. Custom registries never acquire trust implicitly.
fn migrate_official_pin(registries: &mut RegistriesFile) -> bool {
    let Some(owner) = official_owner_pubkey() else {
        return false;
    };
    let mut changed = false;
    for registry in &mut registries.registries {
        if registry.name == "Tilo main"
            && registry.base_url.trim_end_matches('/') == DEFAULT_REGISTRY_BASE_URL
            && (registry.owner_pubkey_hex.is_empty()
                || registry.owner_pubkey_hex == placeholder_owner_pubkey())
        {
            registry.owner_pubkey_hex.clone_from(&owner);
            changed = true;
        }
    }
    changed
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
/// missing, placeholder, or malformed owner key.
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
    let loopback_http =
        url.scheme() == "http" && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"));
    if (url.scheme() != "https" && !loopback_http)
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ConfigError::InvalidUrl);
    }
    if owner_pubkey_hex.len() != 64
        || owner_pubkey_hex == placeholder_owner_pubkey()
        || hex::decode(owner_pubkey_hex).is_err()
    {
        return Err(ConfigError::InvalidOwnerKey);
    }
    registries.registries.push(RegistryConfig {
        name: name.to_string(),
        base_url: base_url.trim_end_matches('/').to_string(),
        owner_pubkey_hex: owner_pubkey_hex.to_ascii_lowercase(),
        enabled: true,
        channel: pkg::version::ReleaseChannel::Stable,
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
    /// parallel runner otherwise races on `TILO_STORE_CONFIG_DIR`.
    fn env_lock() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
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
                Some(v) => unsafe { std::env::set_var("TILO_STORE_CONFIG_DIR", v) },
                None => unsafe { std::env::remove_var("TILO_STORE_CONFIG_DIR") },
            }
            match &self.prev_official {
                Some(v) => unsafe { std::env::set_var("TILO_OFFICIAL_REGISTRY_OWNER_KEY_PATH", v) },
                None => unsafe { std::env::remove_var("TILO_OFFICIAL_REGISTRY_OWNER_KEY_PATH") },
            }
        }
    }

    fn scoped_dir(td: &TempDir) -> Guard<'static> {
        let lock = env_lock();
        let prev = std::env::var_os("TILO_STORE_CONFIG_DIR");
        let prev_official = std::env::var_os("TILO_OFFICIAL_REGISTRY_OWNER_KEY_PATH");
        let trust = td.path().join("registry-owner.pub");
        std::fs::write(
            &trust,
            "d5dd5851bebac876b19986485184213ead36b67595b7efedf1144c120033b0bb\n",
        )
        .unwrap();
        unsafe { std::env::set_var("TILO_STORE_CONFIG_DIR", td.path()) };
        unsafe { std::env::set_var("TILO_OFFICIAL_REGISTRY_OWNER_KEY_PATH", trust) };
        Guard {
            _lock: lock,
            prev,
            prev_official,
        }
    }

    #[test]
    fn seed_written_when_missing() {
        let td = TempDir::new().unwrap();
        let _g = scoped_dir(&td);
        let regs = load_registries();
        assert_eq!(regs.registries.len(), 1);
        assert_eq!(regs.registries[0].name, "Tilo main");
        assert_ne!(
            regs.registries[0].owner_pubkey_hex,
            placeholder_owner_pubkey()
        );
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
        assert!(matches!(
            add_registry(&mut regs, "x", "https://example.test", ""),
            Err(ConfigError::InvalidOwnerKey)
        ));
        assert!(matches!(
            add_registry(&mut regs, "x", "http://example.test", &"ab".repeat(32)),
            Err(ConfigError::InvalidUrl)
        ));
        assert!(matches!(
            add_registry(
                &mut regs,
                "x",
                "https://user:secret@example.test",
                &"ab".repeat(32)
            ),
            Err(ConfigError::InvalidUrl)
        ));
    }

    #[test]
    fn omitted_channel_stays_stable_and_development_requires_opt_in() {
        let old = "name = 'test'\nbase_url = 'https://example.invalid'\nenabled = true\n";
        let stable: RegistryConfig = toml::from_str(old).unwrap();
        assert_eq!(stable.channel, pkg::version::ReleaseChannel::Stable);
        let dev: RegistryConfig =
            toml::from_str(&format!("{old}channel = 'development'\n")).unwrap();
        assert_eq!(dev.channel, pkg::version::ReleaseChannel::Development);
        assert!(toml::from_str::<RegistryConfig>(&format!("{old}channel = 'typo'\n")).is_err());
    }

    #[test]
    fn only_exact_legacy_main_registry_receives_the_os_pin() {
        let td = TempDir::new().unwrap();
        let _guard = scoped_dir(&td);
        let mut registries = RegistriesFile {
            registries: vec![
                RegistryConfig {
                    name: "Tilo main".into(),
                    base_url: DEFAULT_REGISTRY_BASE_URL.into(),
                    owner_pubkey_hex: placeholder_owner_pubkey(),
                    enabled: true,
                    channel: pkg::version::ReleaseChannel::Stable,
                },
                RegistryConfig {
                    name: "custom".into(),
                    base_url: "https://registry.example".into(),
                    owner_pubkey_hex: placeholder_owner_pubkey(),
                    enabled: true,
                    channel: pkg::version::ReleaseChannel::Stable,
                },
            ],
        };
        assert!(migrate_official_pin(&mut registries));
        assert_eq!(
            registries.registries[0].owner_pubkey_hex,
            official_owner_pubkey().unwrap()
        );
        assert_eq!(
            registries.registries[1].owner_pubkey_hex,
            placeholder_owner_pubkey()
        );
    }
}
