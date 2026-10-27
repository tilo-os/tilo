//! The secret this machine keeps for a sibling unlock, and the key it derives.
//!
//! The volume key slot opens with a passphrase derived from two halves:
//!
//! * the **device half**, kept here, in [`Material`];
//! * the **sibling half**, held only by the person's other devices, which
//!   release it for one boot after the owner approves.
//!
//! Neither half alone derives anything
//! ([`tilo_unlock_protocol::derive_slot_key`]); the boot key's certificate
//! binds the enrolment to this device and one slot.

use anyhow::{Context, Result, bail};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use tilo_unlock_protocol::{HALF_LEN, Half};
use zeroize::{Zeroize, Zeroizing};

/// A network early boot may join to reach a sibling: one the owner chose for
/// unlocking. The profile is a `NetworkManager` keyfile, secrets included.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Network {
    /// The connection's name, which is also its keyfile's name.
    pub id: String,
    /// The `.nmconnection` keyfile.
    pub keyfile: String,
}

impl std::fmt::Debug for Network {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Network")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl Drop for Network {
    fn drop(&mut self) {
        self.keyfile.zeroize();
    }
}

/// What the token's sealed blob holds.
#[derive(Serialize, Deserialize)]
pub struct Material {
    pub device_half: String,
    /// The boot key's 32-byte Ed25519 secret, hex.
    pub boot_secret: String,
    /// Owner-chosen unlock networks. Ethernet needs none: every wired link is
    /// tried with DHCP.
    #[serde(default)]
    pub networks: Vec<Network>,
}

impl std::fmt::Debug for Material {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Material")
            .field("networks", &self.networks)
            .finish_non_exhaustive()
    }
}

impl Drop for Material {
    fn drop(&mut self) {
        self.device_half.zeroize();
        self.boot_secret.zeroize();
    }
}

impl Material {
    pub fn new(device_half: &Half, boot_key: &SigningKey, networks: Vec<Network>) -> Self {
        Self {
            device_half: hex::encode(device_half.expose()),
            boot_secret: hex::encode(boot_key.to_bytes()),
            networks,
        }
    }

    /// The device half.
    ///
    /// # Errors
    /// When it is not 32 bytes of hex.
    pub fn device_half(&self) -> Result<Half> {
        Ok(Half::from_bytes(*decode_secret(
            &self.device_half,
            "device half",
        )?))
    }

    /// The boot key.
    ///
    /// # Errors
    /// When it is not 32 bytes of hex.
    pub fn boot_key(&self) -> Result<SigningKey> {
        let secret = decode_secret(&self.boot_secret, "boot key")?;
        Ok(SigningKey::from_bytes(&secret))
    }

    /// Serialises the material for sealing.
    ///
    /// # Errors
    /// Never in practice; `serde_json` reports allocation failure.
    pub fn to_json(&self) -> Result<Zeroizing<Vec<u8>>> {
        Ok(Zeroizing::new(serde_json::to_vec(self)?))
    }

    /// Parses unsealed material.
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        let material: Self =
            serde_json::from_slice(bytes).context("the unlock material is malformed")?;
        material.device_half()?;
        material.boot_key()?;
        for network in &material.networks {
            if !valid_network_id(&network.id) {
                bail!("unlock network {:?} has an unusable name", network.id);
            }
        }
        Ok(material)
    }
}

/// A connection name that is safe as a file name in early boot.
pub fn valid_network_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ' '))
        && !id.starts_with('.')
}

fn decode_secret(hex_value: &str, what: &str) -> Result<Zeroizing<[u8; HALF_LEN]>> {
    let mut out = Zeroizing::new([0_u8; HALF_LEN]);
    hex::decode_to_slice(hex_value, out.as_mut_slice())
        .with_context(|| format!("the {what} is not {HALF_LEN} bytes of hex"))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn material_round_trips_and_hides_its_secrets() {
        let material = Material {
            device_half: "11".repeat(HALF_LEN),
            boot_secret: "22".repeat(HALF_LEN),
            networks: vec![Network {
                id: "Home".into(),
                keyfile: "psk=hunter2".into(),
            }],
        };
        let json = material.to_json().unwrap();
        let back = Material::from_json(&json).unwrap();
        assert_eq!(back.device_half().unwrap().expose(), &[0x11; HALF_LEN]);
        assert_eq!(back.boot_key().unwrap().to_bytes(), [0x22; HALF_LEN]);
        let shown = format!("{back:?}");
        assert!(
            !shown.contains("hunter2") && !shown.contains("1111"),
            "{shown}"
        );
    }

    #[test]
    fn malformed_material_is_refused() {
        assert!(Material::from_json(br#"{"device_half":"11","boot_secret":"22"}"#).is_err());
        let bad_name = serde_json::json!({
            "device_half": "11".repeat(HALF_LEN),
            "boot_secret": "22".repeat(HALF_LEN),
            "networks": [{ "id": "../etc/passwd", "keyfile": "" }],
        });
        assert!(Material::from_json(bad_name.to_string().as_bytes()).is_err());
    }
}
