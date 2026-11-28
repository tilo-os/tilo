//! The registries the workspace this device keeps fetches app entries from: its
//! `workspace/registries` resource, read through the desktop's action API and changed by its
//! actions. A device keeps no list of its own.

use std::fs;
use std::path::{Path, PathBuf};

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

/// The Tilo main registry's owner public key: the trust root every catalogue `apps.tilo.so`
/// serves verifies against.
///
/// It is compiled in so every device trusts the main registry the same way, with no file to
/// install: an OS image, a distribution's package, or a phone whose apps arrive as bundles and
/// whose system never saw the packaging. Signature checks are unchanged — a package installs
/// only when a registry this key vouches for listed it — and the value is the public half
/// `os/publication.env` installs as `TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY`.
///
/// Rotating it is a release: the registry keeps signing with the key the shipped releases pin
/// (the owner may authorize a signing channel for a bounded time) until the release carrying
/// this new value has reached devices, and a device changes what it trusts only by installing
/// that release. An image may ship [`OFFICIAL_OWNER_KEY_PATH`] to pin another key sooner — a
/// distribution's own registry, or a pre-release — and that file is the only override.
pub const MAIN_REGISTRY_OWNER_PUBKEY_HEX: &str =
    "55d9a9c748bcfe34200832c18c5f0cf7932f17571612321eaa542474fa2abe06";

/// The Tilo-main owner identity this device trusts: an image's key file when it ships a valid
/// one, and otherwise [`MAIN_REGISTRY_OWNER_PUBKEY_HEX`].
///
/// A missing, placeholder or malformed file does not disable the trust root and cannot
/// substitute a developer key or trust on first contact: it leaves the compiled key standing.
///
/// [`OFFICIAL_REGISTRY_OWNER_KEY_PATH_ENV`] names another key file in a
/// developer build only (see [`package_store::dev_env`]).
#[must_use]
pub fn official_owner_pubkey() -> Option<String> {
    image_owner_pubkey().or_else(compiled_owner_pubkey)
}

/// The owner key an image ships at [`OFFICIAL_OWNER_KEY_PATH`], when it ships a valid one.
fn image_owner_pubkey() -> Option<String> {
    let path = official_owner_key_path_from(package_store::dev_env::dev_override(
        OFFICIAL_REGISTRY_OWNER_KEY_PATH_ENV,
    ));
    fs::read_to_string(path)
        .ok()
        .and_then(|value| checked_owner_pubkey(&value))
}

fn compiled_owner_pubkey() -> Option<String> {
    checked_owner_pubkey(MAIN_REGISTRY_OWNER_PUBKEY_HEX)
}

/// `value` as this build would pin it: lowercase 64 hexadecimal digits naming a 32-byte Ed25519
/// key, and not the all-zero placeholder that means "no key".
fn checked_owner_pubkey(value: &str) -> Option<String> {
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
    /// The default list: one hosted registry pinned to [`MAIN_REGISTRY_OWNER_PUBKEY_HEX`] (or
    /// the image's override). A workspace made on this device starts with it.
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

/// Why a device's own registries file could not be read.
#[derive(Debug, thiserror::Error)]
pub enum RegistriesFileError {
    #[error("could not read the registries file {}: {source}", .path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("the registries file {} is invalid: {source}", .path.display())]
    Decode {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
}

/// The registries the file at `path` names, when a device's configuration ships one: an image's
/// or a distribution's own catalogue beside the official one. `Ok(None)` when there is no file,
/// which leaves the official registry standing — a device that names none starts from it.
///
/// A file that is there but cannot be read or parsed is an error, never the official list by
/// another name: a device whose configuration is broken says so where it starts.
///
/// # Errors
/// The file is there but cannot be read, or is not a registries file.
pub fn registries_file_at(path: &Path) -> Result<Option<RegistriesFile>, RegistriesFileError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(RegistriesFileError::Read {
                path: path.to_owned(),
                source,
            });
        }
    };
    toml::from_str(&text)
        .map(Some)
        .map_err(|source| RegistriesFileError::Decode {
            path: path.to_owned(),
            source,
        })
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
/// A process acting in a workspace it names (`tilo -w <workspace> app …`) reads
/// that workspace's registries, as `app repo list` does, rather than those of
/// the computer's own workspace, which lists none. A reading the workspace
/// refuses (an agent's, on a device the workspace does not let it change apps
/// from) is said as a refusal: an empty list in its place would read as a
/// workspace whose repositories offer nothing.
///
/// `TILO_STORE_CONFIG_DIR` names a `registries.toml` a test or a development
/// build reads instead; production reads the workspace alone.
///
/// # Errors
/// The workspace refused the reading, its daemon could not be reached, or the
/// registries do not read.
pub fn load_registries() -> Result<RegistriesFile, String> {
    if let Some(registries) = ONCE_FOR_THIS_CALL.with(|slot| slot.borrow().clone()) {
        return Ok(registries);
    }
    if let Some(dir) = std::env::var_os("TILO_STORE_CONFIG_DIR") {
        return registries_file_at(&PathBuf::from(dir).join("registries.toml"))
            .map(Option::unwrap_or_default)
            .map_err(|error| error.to_string());
    }
    #[cfg(any(unix, windows))]
    if crate::device::named().is_some() {
        let value = crate::device::registries("registry.list", &serde_json::json!({}))
            .map_err(|refused| format!("the workspace refused reading them: {refused}"))?;
        return registries_from_rows(&value)
            .ok_or_else(|| format!("the workspace's answer names no repositories: {value}"));
    }
    Ok(RegistriesFile {
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
    })
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

    /// A directory no key file was written to, and the environment scoped to it.
    fn no_trust_input() -> (tempfile::TempDir, tilo_test_env::ScopedEnv) {
        let dir = tempfile::tempdir().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(
            OFFICIAL_REGISTRY_OWNER_KEY_PATH_ENV,
            dir.path().join("missing.pub"),
        );
        (dir, env)
    }

    #[test]
    fn a_device_with_no_trust_file_trusts_the_compiled_main_registry() {
        let (_dir, _env) = no_trust_input();
        assert_eq!(
            official_owner_pubkey().as_deref(),
            Some(MAIN_REGISTRY_OWNER_PUBKEY_HEX)
        );
        let seeded = RegistriesFile::seeded();
        assert_eq!(seeded.registries.len(), 1);
        assert_eq!(seeded.registries[0].name, "Tilo main");
        assert_eq!(seeded.registries[0].base_url, DEFAULT_REGISTRY_BASE_URL);
        assert_eq!(
            seeded.registries[0].owner_pubkey_hex,
            MAIN_REGISTRY_OWNER_PUBKEY_HEX
        );
        assert!(seeded.registries[0].enabled);
    }

    #[test]
    fn the_compiled_root_is_the_key_the_release_publishes() {
        let Ok(published) = fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../os/publication.env"
        )) else {
            return;
        };
        let line = published
            .lines()
            .find_map(|line| line.strip_prefix("TILO_OFFICIAL_REGISTRY_OWNER_PUBKEY="))
            .expect("os/publication.env names the registry owner key");
        assert_eq!(line.trim(), MAIN_REGISTRY_OWNER_PUBKEY_HEX);
    }

    #[test]
    fn an_images_owner_key_file_overrides_the_compiled_root() {
        if !package_store::dev_env::DEV_ENV_OVERRIDES {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("owner.pub");
        let image_key = "cd".repeat(32);
        fs::write(&path, format!("{image_key}\n")).unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(OFFICIAL_REGISTRY_OWNER_KEY_PATH_ENV, &path);
        assert_eq!(official_owner_pubkey().as_deref(), Some(image_key.as_str()));
        assert_eq!(
            RegistriesFile::seeded().registries[0].owner_pubkey_hex,
            image_key
        );
    }

    #[test]
    fn a_placeholder_or_malformed_image_key_leaves_the_compiled_root() {
        if !package_store::dev_env::DEV_ENV_OVERRIDES {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("owner.pub");
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(OFFICIAL_REGISTRY_OWNER_KEY_PATH_ENV, &path);
        let placeholder = placeholder_owner_pubkey();
        for (shipped, written) in [
            ("no file", None),
            ("the placeholder", Some(placeholder.as_str())),
            ("a malformed key", Some("not a key")),
        ] {
            match written {
                Some(text) => fs::write(&path, text).unwrap(),
                None => {
                    let _ = fs::remove_file(&path);
                }
            }
            assert_eq!(
                official_owner_pubkey().as_deref(),
                Some(MAIN_REGISTRY_OWNER_PUBKEY_HEX),
                "an image that ships {shipped} does not replace the trust root"
            );
        }
    }

    #[test]
    fn a_device_naming_its_own_registries_file_founds_with_that_list() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("registries.toml");
        assert!(
            registries_file_at(&path).unwrap().is_none(),
            "no file, no list"
        );
        fs::write(&path, "").unwrap();
        assert_eq!(
            registries_file_at(&path).unwrap(),
            Some(RegistriesFile::default()),
            "an empty file names no registry"
        );
        fs::write(
            &path,
            "[[registry]]\nname = \"fixture\"\nbase_url = \"http://127.0.0.1:8081\"\n\
             owner_pubkey_hex = \"ab\"\n",
        )
        .unwrap();
        let named = registries_file_at(&path).unwrap().unwrap();
        assert_eq!(named.registries[0].name, "fixture");
        fs::write(&path, "not toml").unwrap();
        assert!(
            registries_file_at(&path).is_err(),
            "a file that does not parse is an error, never the official list"
        );
    }

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
