//! The registries the workspace this device keeps fetches app entries from: its
//! `workspace/registries` resource, read through the desktop's action API and changed by its
//! actions. A device keeps no list of its own.

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
    #[error("'{0}' is the last repository; disable it instead (`app repo disable '{0}'`)")]
    LastRegistry(String),
    #[error("the registry URL must be credential-free HTTPS (or loopback HTTP for development)")]
    InvalidUrl,
    #[error("a non-placeholder owner key of 64 hexadecimal characters is required")]
    InvalidOwnerKey,
    #[error("could not determine the Tilo Store config directory")]
    NoConfigDir,
    #[error("could not read the existing registry configuration; fix it before saving: {0}")]
    Read(#[source] std::io::Error),
    #[error("the existing registry configuration is invalid; fix it before saving: {0}")]
    Decode(#[from] toml::de::Error),
    #[error("could not encode the registry configuration: {0}")]
    Encode(#[from] toml::ser::Error),
    #[error("could not save the registry configuration: {0}")]
    Write(#[from] std::io::Error),
}

/// Default hosted registry base URL. Overridable per-entry in the
/// config file; this is only used to seed the default `registries.toml`.
pub const DEFAULT_REGISTRY_BASE_URL: &str = "https://apps.tilo.so";

/// Names the official registry owner key file instead of the image's.
pub const OFFICIAL_REGISTRY_OWNER_KEY_PATH_ENV: &str = "TILO_OFFICIAL_REGISTRY_OWNER_KEY_PATH";

/// Pinned Tilo-main owner identity. Installed images can rotate the public trust
/// input without rebuilding app packages. A missing/placeholder value disables
/// the default registry rather than substituting a developer key or TOFU.
///
/// [`OFFICIAL_REGISTRY_OWNER_KEY_PATH_ENV`] names another key file in a
/// developer build only (see [`package_store::dev_env`]).
pub fn official_owner_pubkey() -> Option<String> {
    let path = official_owner_key_path_from(package_store::dev_env::dev_override(
        OFFICIAL_REGISTRY_OWNER_KEY_PATH_ENV,
    ));
    let value = fs::read_to_string(path).ok()?;
    let value = value.trim().to_ascii_lowercase();
    (value.len() == 64
        && value != placeholder_owner_pubkey()
        && hex::decode(&value).is_ok_and(|bytes| bytes.len() == 32))
    .then_some(value)
}

pub const OFFICIAL_OWNER_KEY_PATH: &str = "/usr/share/tilo/trust/tilo-main-registry-owner.pub";

fn official_owner_key_path_from(allowed_override: Option<std::ffi::OsString>) -> PathBuf {
    allowed_override.map_or_else(|| PathBuf::from(OFFICIAL_OWNER_KEY_PATH), PathBuf::from)
}

pub fn placeholder_owner_pubkey() -> String {
    "00".repeat(32)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryConfig {
    /// Human-readable short name, shown wherever an entry has to say
    /// where it came from.
    pub name: String,
    pub base_url: String,
    /// Registry owner's pinned Ed25519 identity, hex-encoded. Missing keys
    /// fail closed; registry discovery never silently creates trust.
    #[serde(default)]
    pub owner_pubkey_hex: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Stable by default; development explicitly opts into prerelease apps.
    #[serde(default)]
    pub channel: tilo_app_package::version::ReleaseChannel,
}

fn default_enabled() -> bool {
    true
}

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
                    channel: tilo_app_package::version::ReleaseChannel::Stable,
                }]
            }),
        }
    }

    pub fn enabled(&self) -> impl Iterator<Item = &RegistryConfig> {
        self.registries.iter().filter(|r| r.enabled)
    }
}

/// The root config dir; `TILO_STORE_CONFIG_DIR` overrides it for tests.
pub fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("TILO_STORE_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }
    tilo_paths::AppDirs::new("tilo-store").config()
}

/// `$XDG_CACHE_HOME/tilo-store`, where downloaded `.tapp`s are staged.
pub fn cache_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("TILO_STORE_CACHE_DIR") {
        return Some(PathBuf::from(dir));
    }
    tilo_paths::AppDirs::new("tilo-store").cache()
}

/// The registries the desktop answers for the workspace this process belongs
/// to ([`tilo_control::front::call_action`]), as its `registry.list` reading
/// reports them.
///
/// A reading that cannot be made — no desktop session, or no workspace — is
/// no registries: a machine with nothing configured has nothing available to
/// offer, and an empty list offers nothing rather than trusting something
/// nobody configured.
///
/// `TILO_STORE_CONFIG_DIR` names a `registries.toml` a test or a development
/// build reads instead; production reads the workspace alone.
#[must_use]
pub fn load_registries() -> RegistriesFile {
    if let Some(registries) = ONCE_FOR_THIS_CALL.with(|slot| slot.borrow().clone()) {
        return registries;
    }
    if let Some(dir) = std::env::var_os("TILO_STORE_CONFIG_DIR") {
        let path = PathBuf::from(dir).join("registries.toml");
        return fs::read_to_string(path)
            .ok()
            .and_then(|text| toml::from_str::<RegistriesFile>(&text).ok())
            .unwrap_or_default();
    }
    RegistriesFile {
        registries: tilo_control::packages::registries()
            .into_iter()
            .map(|registry| RegistryConfig {
                name: registry.name,
                base_url: registry.base_url,
                owner_pubkey_hex: registry.owner_pubkey_hex,
                enabled: registry.enabled,
                channel: channel(&registry.channel),
            })
            .collect(),
    }
}

fn channel(channel: &str) -> tilo_app_package::version::ReleaseChannel {
    match channel {
        "development" => tilo_app_package::version::ReleaseChannel::Development,
        _ => tilo_app_package::version::ReleaseChannel::Stable,
    }
}

/// The trust a workspace keeps for packages: the attestors it accepts and the
/// publisher keys it refuses.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TrustList {
    #[serde(default)]
    pub attestors: Vec<TrustAttestorRow>,
    #[serde(default)]
    pub revoked: Vec<RevokedRow>,
}

/// An attestor a workspace trusts, as its trust list reads it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TrustAttestorRow {
    pub id: String,
    pub pubkey_hex: String,
    pub scope: String,
}

/// A publisher key a workspace refuses.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RevokedRow {
    pub pubkey_hex: String,
    #[serde(default)]
    pub reason: String,
}

/// The workspace's trust list, as its `trust.list` reading answers it.
///
/// A reading that cannot be made — no desktop session, or no workspace — is
/// an empty list: nothing trusted beyond what a package's own signature and
/// the image's roots say, which is the safe reading of "nothing configured".
/// `TILO_WORKSPACE_TRUST` names the list (its JSON) a test or a development
/// build reads instead.
#[must_use]
pub fn trust() -> TrustList {
    let (attestors, revoked) = tilo_control::packages::trust();
    TrustList {
        attestors: attestors
            .into_iter()
            .map(|attestor| TrustAttestorRow {
                id: attestor.id,
                pubkey_hex: attestor.pubkey_hex,
                scope: attestor.scope,
            })
            .collect(),
        revoked: revoked
            .into_iter()
            .map(|key| RevokedRow {
                pubkey_hex: key.pubkey_hex,
                reason: key.reason,
            })
            .collect(),
    }
}

thread_local! {
    /// The list a caller hands [`load_registries`] for the duration of one call:
    /// a daemon reads one repository's catalogue with the row its caller named.
    static ONCE_FOR_THIS_CALL: std::cell::RefCell<Option<RegistriesFile>> =
        const { std::cell::RefCell::new(None) };
}

/// Run `read` with `registries` as the list every [`load_registries`] inside it
/// answers: a catalogue read made for one registry the workspace names.
pub fn with_registries<T>(registries: RegistriesFile, read: impl FnOnce() -> T) -> T {
    struct PutBack(Option<RegistriesFile>);
    impl Drop for PutBack {
        fn drop(&mut self) {
            ONCE_FOR_THIS_CALL.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let previous = ONCE_FOR_THIS_CALL.with(|slot| slot.borrow_mut().replace(registries));
    let _put_back = PutBack(previous);
    read()
}

/// The registry list a `registry.list` reading answered: `{"registries": [...]}`
/// in the workspace's spelling, as this module's list.
#[must_use]
pub fn registries_from_rows(value: &serde_json::Value) -> Option<RegistriesFile> {
    let rows = value.get("registries")?.as_array()?;
    Some(RegistriesFile {
        registries: rows
            .iter()
            .filter_map(|row| {
                let name = row.get("name")?.as_str()?.to_owned();
                let base_url = row.get("base_url")?.as_str()?.to_owned();
                let owner_pubkey_hex = row.get("owner_pubkey_hex")?.as_str()?.to_owned();
                let channel = match row.get("channel").and_then(serde_json::Value::as_str) {
                    Some("development") => tilo_app_package::version::ReleaseChannel::Development,
                    _ => tilo_app_package::version::ReleaseChannel::Stable,
                };
                Some(RegistryConfig {
                    name,
                    base_url,
                    owner_pubkey_hex,
                    enabled: row
                        .get("enabled")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(true),
                    channel,
                })
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_registry_list_reading_reads_as_the_list_the_cli_uses() {
        let value = serde_json::json!({
            "registries": [
                {
                    "name": "Tilo main",
                    "base_url": "https://apps.tilo.so",
                    "owner_pubkey_hex": "ab".repeat(32),
                    "enabled": true,
                    "channel": "stable"
                },
                {
                    "name": "nightly",
                    "base_url": "http://localhost:8080",
                    "owner_pubkey_hex": "cd".repeat(32),
                    "enabled": false,
                    "channel": "development"
                }
            ]
        });
        let registries = registries_from_rows(&value).expect("a registry list");
        assert_eq!(registries.registries.len(), 2);
        assert_eq!(registries.registries[0].name, "Tilo main");
        assert_eq!(
            registries.registries[0].channel,
            tilo_app_package::version::ReleaseChannel::Stable
        );
        assert!(!registries.registries[1].enabled);
        assert_eq!(
            registries.registries[1].channel,
            tilo_app_package::version::ReleaseChannel::Development
        );
        assert_eq!(registries.enabled().count(), 1);
        assert!(registries_from_rows(&serde_json::json!({})).is_none());
    }
}
