//! Where remembered network secrets are kept: sealed, never in settings.
//!
//! A WPA passphrase goes to the person's roaming Wi-Fi credentials when
//! their devices share them — sealed to each of their certified devices
//! (docs/device-mesh.md, "Roaming settings"). Everything else, and a
//! passphrase no roaming store took, goes to the network secrets vault: one
//! `secret-vault` file whose key the Secret Service holds.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use secret_vault::{Custody, VaultFormat};
use zeroize::Zeroizing;

use super::SettingSecrets;

/// Which remembered secrets: one setting of one connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreKey {
    pub uuid: String,
    pub setting: String,
    /// The network and its security when the secret is a WPA passphrase
    /// the person's devices may share.
    pub roaming_wifi: Option<(String, os_actions::WifiSecurity)>,
}

/// Keeps remembered secrets. Calls may block (file and Secret Service
/// I/O); the agent runs them off the async threads.
pub trait SecretStore: Send + Sync {
    fn load(&self, key: &StoreKey) -> Option<SettingSecrets>;
    /// Remember `secrets` for `key`, replacing what was there.
    ///
    /// # Errors
    ///
    /// When nothing could keep them.
    fn keep(&self, key: &StoreKey, secrets: &SettingSecrets) -> Result<(), String>;
    /// Forget what is remembered for `key` on this device.
    ///
    /// # Errors
    ///
    /// When the vault cannot be rewritten.
    fn forget(&self, key: &StoreKey) -> Result<(), String>;
    /// Forget everything remembered for a connection.
    ///
    /// # Errors
    ///
    /// When the vault cannot be rewritten.
    fn forget_connection(&self, uuid: &str) -> Result<(), String>;
}

/// The person's roaming Wi-Fi credentials.
pub trait RoamingWifi: Send + Sync {
    fn password(&self, ssid: &str, security: os_actions::WifiSecurity) -> Option<String>;
    /// # Errors
    ///
    /// When the credential cannot be sealed to the person's devices.
    fn share(
        &self,
        ssid: &str,
        security: os_actions::WifiSecurity,
        password: &str,
    ) -> Result<(), String>;
}

/// The roaming store, once peer sync has started (it may never). Clones
/// share one slot: the host keeps the session's and fills it when peer sync
/// starts.
#[derive(Clone, Default)]
pub struct RoamingWifiSlot {
    inner: Arc<Mutex<Option<Arc<dyn RoamingWifi>>>>,
}

impl RoamingWifiSlot {
    pub fn set(&self, roaming: Arc<dyn RoamingWifi>) {
        *self.inner.lock().unwrap_or_else(PoisonError::into_inner) = Some(roaming);
    }

    fn get(&self) -> Option<Arc<dyn RoamingWifi>> {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// The on-disk identity of the network secrets vault. Every value is part
/// of the persisted format.
const FORMAT: VaultFormat = VaultFormat {
    name: "network-secrets",
    // Tilo Network Secret Vault.
    magic: b"TNSV",
    secret_service_label: "Tilo network secrets",
    secret_service_attributes: &[("application", "Tilo"), ("purpose", "network-secrets")],
    dpapi_entropy: b"Tilo network secrets vault key v1",
};

/// uuid → setting → key → value.
type Entries = BTreeMap<String, BTreeMap<String, BTreeMap<String, String>>>;

/// The network secrets vault, plus the roaming store for passphrases.
pub struct VaultStore {
    dir: PathBuf,
    custody: Custody,
    roaming: RoamingWifiSlot,
    /// One read-modify-write at a time.
    lock: Mutex<()>,
}

impl VaultStore {
    /// The session's vault under `$XDG_CONFIG_HOME/tilo/network-secrets`.
    pub fn for_session(roaming: RoamingWifiSlot) -> Option<Self> {
        let dir = tilo_paths::xdg::config_home()?
            .join("tilo")
            .join("network-secrets");
        Some(Self::at(dir, Custody::for_session(), roaming))
    }

    pub fn at(dir: PathBuf, custody: Custody, roaming: RoamingWifiSlot) -> Self {
        Self {
            dir,
            custody,
            roaming,
            lock: Mutex::new(()),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn read(&self) -> Result<Entries, String> {
        let blob = match std::fs::read(self.dir.join(secret_vault::VAULT_FILE)) {
            Ok(blob) => blob,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Entries::new());
            }
            Err(error) => return Err(format!("could not read the network secrets vault: {error}")),
        };
        let key = FORMAT
            .load_or_create_key(&self.dir, self.custody)
            .ok_or_else(|| "the network secrets vault key is unavailable".to_string())?;
        let plaintext = FORMAT
            .open(&key, &blob)
            .ok_or_else(|| "the network secrets vault failed its integrity check".to_string())?;
        serde_json::from_slice(&plaintext)
            .map_err(|_| "the network secrets vault is not valid".to_string())
    }

    fn write(&self, entries: &Entries) -> Result<(), String> {
        std::fs::create_dir_all(&self.dir)
            .map_err(|error| format!("could not create the vault directory: {error}"))?;
        let key = FORMAT
            .load_or_create_key(&self.dir, self.custody)
            .ok_or_else(|| "no vault key is available".to_string())?;
        let plaintext = Zeroizing::new(
            serde_json::to_vec(entries)
                .map_err(|error| format!("could not encode the vault: {error}"))?,
        );
        let blob = FORMAT
            .seal(&key, &plaintext)
            .ok_or_else(|| "sealing the vault failed".to_string())?;
        tilo_files::native::atomic::write_private(&self.dir.join(secret_vault::VAULT_FILE), &blob)
            .map_err(|error| format!("could not write the vault: {error}"))
    }

    fn modify(&self, change: impl FnOnce(&mut Entries) -> bool) -> Result<(), String> {
        let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        let mut entries = self.read()?;
        if change(&mut entries) {
            self.write(&entries)?;
        }
        Ok(())
    }

    fn portable_passphrase<'a>(
        key: &'a StoreKey,
        secrets: &'a SettingSecrets,
    ) -> Option<(&'a str, os_actions::WifiSecurity, &'a str)> {
        let (ssid, security) = key.roaming_wifi.as_ref()?;
        let psk = secrets.get("psk")?;
        (security.supports_portable_passphrase() && secrets.0.len() == 1).then_some((
            ssid.as_str(),
            *security,
            psk,
        ))
    }
}

impl SecretStore for VaultStore {
    fn load(&self, key: &StoreKey) -> Option<SettingSecrets> {
        let vaulted = {
            let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
            match self.read() {
                Ok(entries) => entries
                    .get(&key.uuid)
                    .and_then(|settings| settings.get(&key.setting))
                    .map(|values| {
                        let mut secrets = SettingSecrets::default();
                        for (name, value) in values {
                            secrets.insert(name.clone(), value.clone());
                        }
                        secrets
                    }),
                Err(error) => {
                    tracing::warn!(target: "shell::network_secrets", %error, "vault unreadable");
                    None
                }
            }
        };
        if vaulted.is_some() {
            return vaulted;
        }
        let (ssid, security) = key.roaming_wifi.as_ref()?;
        let password = self.roaming.get()?.password(ssid, *security)?;
        let mut secrets = SettingSecrets::default();
        secrets.insert("psk", password);
        Some(secrets)
    }

    fn keep(&self, key: &StoreKey, secrets: &SettingSecrets) -> Result<(), String> {
        if let (Some((ssid, security, psk)), Some(roaming)) =
            (Self::portable_passphrase(key, secrets), self.roaming.get())
        {
            match roaming.share(ssid, security, psk) {
                // Kept once, where the person's other devices find it too.
                Ok(()) => return self.forget(key),
                Err(error) => tracing::info!(
                    target: "shell::network_secrets",
                    %error,
                    "Wi-Fi password not shared across devices; keeping it on this one"
                ),
            }
        }
        self.modify(|entries| {
            let values = secrets
                .0
                .iter()
                .map(|(name, value)| (name.clone(), value.to_string()))
                .collect();
            entries
                .entry(key.uuid.clone())
                .or_default()
                .insert(key.setting.clone(), values);
            true
        })
    }

    fn forget(&self, key: &StoreKey) -> Result<(), String> {
        self.modify(|entries| {
            let Some(settings) = entries.get_mut(&key.uuid) else {
                return false;
            };
            let removed = settings.remove(&key.setting).is_some();
            if settings.is_empty() {
                entries.remove(&key.uuid);
            }
            removed
        })
    }

    fn forget_connection(&self, uuid: &str) -> Result<(), String> {
        self.modify(|entries| entries.remove(uuid).is_some())
    }
}
