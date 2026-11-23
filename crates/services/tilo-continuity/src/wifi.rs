//! Wi-Fi continuity, published as [`PROVIDER`]: joining a network shares its passphrase with the
//! person's other devices, sealed to each of them ([`crate::secrets`]), and a device joins with one
//! another device shared. Settings also asks here whether this machine's interfaces wake it. All of
//! it is under the person's "Wi-Fi and personal settings" switch.

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use tilo_actions::remote::{ActionCallError, ActionProvider};
use tilo_fabric::actions::{Asker, Provider};
use tilo_fabric::{DeviceId, Fabric, FabricError, FabricStamp};
use zeroize::Zeroizing;

use crate::Switches;
use crate::secrets::{open_wifi_credential, seal_wifi_credential, validate_wifi_envelopes};

/// The provider Wi-Fi continuity is published under.
pub const PROVIDER: &str = "so.tilo.wifi-continuity";
pub const MAX_WIFI_CREDENTIALS: usize = 256;
pub const MAX_SEALED_SECRET_BYTES: usize = 16 * 1024;

/// Versioned per-recipient ciphertext envelopes. Plaintext credentials are
/// intentionally impossible to represent in fabric state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WifiCredential {
    pub network_id: [u8; 32],
    pub ssid: String,
    pub security: os_actions::WifiSecurity,
    pub sealed_secret: Vec<u8>,
    pub nonce: [u8; 24],
    pub stamp: FabricStamp,
}

impl WifiCredential {
    /// Whether this is a sealed passphrase of a network one could join.
    /// # Errors
    /// [`FabricError::InvalidWifiCredential`] for anything else.
    pub fn validate(&self) -> Result<(), FabricError> {
        if self.ssid.is_empty()
            || self.ssid.len() > 128
            || matches!(
                self.security,
                os_actions::WifiSecurity::Open | os_actions::WifiSecurity::Unknown
            )
            || self.sealed_secret.is_empty()
            || self.sealed_secret.len() > MAX_SEALED_SECRET_BYTES
        {
            return Err(FabricError::InvalidWifiCredential);
        }
        validate_wifi_envelopes(&self.sealed_secret)
            .map_err(|_| FabricError::InvalidWifiCredential)?;
        Ok(())
    }
}

/// Where the person's shared passphrases are kept: one per network, in the person's store.
pub trait Credentials: Send + Sync {
    /// Keep `credential` in place of what was kept for its network.
    ///
    /// # Errors
    /// Why the store did not take it.
    fn keep(&self, credential: &WifiCredential) -> Result<(), String>;

    /// Every passphrase the person's devices share.
    fn all(&self) -> Vec<WifiCredential>;
}

/// The passphrase of `ssid` with `security` that one of the person's devices shared, opened with
/// this device's key `key`.
#[must_use]
pub fn password_of(
    credentials: &[WifiCredential],
    ssid: &str,
    security: os_actions::WifiSecurity,
    key: &SigningKey,
) -> Option<String> {
    if !security.supports_portable_passphrase() {
        return None;
    }
    credentials
        .iter()
        .filter(|credential| credential.ssid == ssid && credential.security == security)
        .find_map(|credential| open_wifi_credential(credential, key).ok())
}

/// `password` of `ssid`, sealed by `author` to each of `recipients`: what a device keeps for the
/// person's devices when it joins. Sealed to every device of theirs the store holds, the author
/// included: a device certified later opens it once a device that knows it shares it again.
///
/// # Errors
/// Why it could not be sealed.
pub fn sealed(
    ssid: &str,
    security: os_actions::WifiSecurity,
    password: &str,
    author: &DeviceId,
    recipients: Vec<DeviceId>,
) -> Result<WifiCredential, String> {
    seal_wifi_credential(
        ssid,
        security,
        password,
        author,
        recipients,
        FabricStamp {
            counter: tilo_clock::unix_now_ms(),
            device: author.clone(),
        },
    )
    .map_err(|error| error.to_string())
}

/// Wi-Fi continuity on this device. Cloning shares it.
#[derive(Clone)]
pub struct Wifi {
    fabric: Fabric,
    key: Arc<SigningKey>,
    credentials: Arc<dyn Credentials>,
    switches: Switches,
}

/// `wifi#password`: the network asked for.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Network {
    pub ssid: String,
    pub security: os_actions::WifiSecurity,
}

/// What `wifi#password` answers: the passphrase another device shared, if one did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Password {
    pub password: Option<String>,
}

/// `wifi#share`: a passphrase the person typed to join `ssid`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Share {
    pub ssid: String,
    pub security: os_actions::WifiSecurity,
    pub password: String,
}

/// `wifi#share-saved`, `wifi#network-wake`: a profile saved on this machine.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Profile {
    pub profile_uuid: String,
    /// For `wifi#network-wake`: turn waking through it on or off, or only say how it stands.
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// What a share answers: the network it shared.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Shared {
    pub ssid: String,
}

impl Wifi {
    /// Wi-Fi continuity of the device `fabric` is, whose key is `key`.
    #[must_use]
    pub fn new(
        fabric: Fabric,
        key: SigningKey,
        credentials: Arc<dyn Credentials>,
        switches: Switches,
    ) -> Self {
        Self {
            fabric,
            key: Arc::new(key),
            credentials,
            switches,
        }
    }

    fn permitted(&self) -> Result<(), ActionCallError> {
        self.switches
            .wifi_settings_permitted()
            .then_some(())
            .ok_or_else(|| ActionCallError::Refused {
                reason: "Wi-Fi continuity is turned off in this device's settings".to_owned(),
            })
    }

    /// The passphrase of `network` one of the person's devices shared.
    #[must_use]
    pub fn password(&self, network: &Network) -> Option<String> {
        self.permitted().ok()?;
        password_of(
            &self.credentials.all(),
            &network.ssid,
            network.security,
            &self.key,
        )
    }

    /// Share `password` of `ssid` with the person's devices.
    ///
    /// # Errors
    /// Continuity is off, or the passphrase could not be sealed or kept.
    pub fn share(
        &self,
        ssid: &str,
        security: os_actions::WifiSecurity,
        password: &str,
    ) -> Result<Shared, ActionCallError> {
        self.permitted()?;
        let refused = |reason: String| ActionCallError::Refused { reason };
        let recipients = self.fabric.devices().devices().keys().cloned().collect();
        let credential =
            sealed(ssid, security, password, self.fabric.local(), recipients).map_err(refused)?;
        self.credentials.keep(&credential).map_err(refused)?;
        Ok(Shared {
            ssid: ssid.to_owned(),
        })
    }

    /// Wi-Fi continuity's actions, as the device's action host mounts them under [`PROVIDER`].
    #[must_use]
    pub fn provider(&self) -> Arc<dyn ActionProvider + Send + Sync> {
        let at = || self.clone();
        Provider::new(PROVIDER, self.fabric.local().clone(), *self.fabric.person())
            .act(
                "wifi",
                "password",
                "The passphrase another of the person's devices shared",
                Asker::Here,
                {
                    let wifi = at();
                    move |_, network: Network| {
                        Ok(Password {
                            password: wifi.password(&network),
                        })
                    }
                },
            )
            .act(
                "wifi",
                "share",
                "Share a network's passphrase with the person's devices",
                Asker::Here,
                {
                    let wifi = at();
                    move |_, share: Share| {
                        let password = Zeroizing::new(share.password);
                        wifi.share(&share.ssid, share.security, &password)
                    }
                },
            )
            .act(
                "wifi",
                "share-saved",
                "Share a saved network's passphrase with the person's devices",
                Asker::Here,
                {
                    let wifi = at();
                    move |_, profile: Profile| {
                        let (ssid, security, password) = saved_profile(&profile.profile_uuid)
                            .map_err(|reason| ActionCallError::Refused { reason })?;
                        wifi.share(&ssid, security, &password)
                    }
                },
            )
            .act(
                "wifi",
                "network-wake",
                "Whether a saved network wakes this computer",
                Asker::Here,
                |_, profile: Profile| {
                    network_wake(&profile.profile_uuid, profile.enabled)
                        .map_err(|reason| ActionCallError::Refused { reason })
                },
            )
            .shared()
    }
}

/// The only parts of a profile saved on this machine that travel between the person's devices.
#[cfg(target_os = "linux")]
fn saved_profile(
    profile_uuid: &str,
) -> Result<(String, os_actions::WifiSecurity, Zeroizing<String>), String> {
    let profile = os_actions::portable_wifi_profile_secret(profile_uuid)?;
    Ok((profile.ssid, profile.security, profile.password))
}

#[cfg(not(target_os = "linux"))]
fn saved_profile(
    _profile_uuid: &str,
) -> Result<(String, os_actions::WifiSecurity, Zeroizing<String>), String> {
    Err("saved Wi-Fi profiles belong to a Linux computer".to_owned())
}

#[cfg(target_os = "linux")]
fn network_wake(
    profile_uuid: &str,
    enabled: Option<bool>,
) -> Result<os_actions::network_wake::NetworkWakeStatus, String> {
    match enabled {
        Some(enabled) => os_actions::network_wake::configure(profile_uuid, enabled),
        None => os_actions::network_wake::status(profile_uuid),
    }
}

#[cfg(not(target_os = "linux"))]
fn network_wake(
    _profile_uuid: &str,
    _enabled: Option<bool>,
) -> Result<os_actions::network_wake::NetworkWakeStatus, String> {
    Err("network wake belongs to a Linux computer".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wifi_secrets_must_be_sealed() {
        let credential = WifiCredential {
            network_id: [1; 32],
            ssid: "Home".into(),
            security: os_actions::WifiSecurity::Wpa3,
            sealed_secret: Vec::new(),
            nonce: [2; 24],
            stamp: FabricStamp {
                counter: 1,
                device: DeviceId::of_key(&[3; 32]),
            },
        };
        assert_eq!(
            credential.validate(),
            Err(FabricError::InvalidWifiCredential)
        );
    }

    #[test]
    fn typed_wifi_security_decodes_the_legacy_string_postcard_shape() {
        #[derive(Serialize)]
        struct LegacyWifiCredential {
            network_id: [u8; 32],
            ssid: String,
            security: String,
            sealed_secret: Vec<u8>,
            nonce: [u8; 24],
            stamp: FabricStamp,
        }

        let legacy = LegacyWifiCredential {
            network_id: [1; 32],
            ssid: "Home".into(),
            security: "wpa3".into(),
            sealed_secret: vec![1, 2, 3],
            nonce: [2; 24],
            stamp: FabricStamp {
                counter: 1,
                device: DeviceId::of_key(&[3; 32]),
            },
        };
        let bytes = postcard::to_allocvec(&legacy).unwrap();
        let decoded: WifiCredential = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.security, os_actions::WifiSecurity::Wpa3);
        assert_eq!(postcard::to_allocvec(&decoded).unwrap(), bytes);
    }

    #[test]
    fn an_enterprise_entry_never_supplies_a_password() {
        let key = SigningKey::from_bytes(&[41; 32]);
        let owner = DeviceId::of_key(&key.verifying_key().to_bytes());
        let mut credential = crate::secrets::seal_wifi_credential(
            "Corp",
            os_actions::WifiSecurity::Wpa2,
            "legacy-secret",
            &owner,
            [owner.clone()],
            FabricStamp {
                counter: 1,
                device: owner.clone(),
            },
        )
        .unwrap();
        assert_eq!(credential.validate(), Ok(()));
        // Version 1 allowed this string even though its secret lacked the EAP
        // identity and CA policy needed to safely recreate the profile.
        credential.security = os_actions::WifiSecurity::Enterprise;

        assert_eq!(
            crate::secrets::open_wifi_credential(&credential, &key),
            Err(crate::secrets::WifiSecretError::InvalidCredential)
        );
    }
}
