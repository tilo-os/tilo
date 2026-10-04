//! The secret this machine keeps for a sibling unlock, and the key it derives.
//!
//! The volume key slot opens with a passphrase derived from two halves:
//!
//! * the **device half**, kept here, in [`Material`];
//! * the **sibling half**, held only by the person's other devices, which
//!   release it for one boot after the owner approves.
//!
//! Neither half alone derives anything. The derivation is bound to the volume
//! and the slot, so a half enrolled for one slot opens no other.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

/// Length of each half and of the boot key's secret, in bytes.
pub const HALF_LEN: usize = 32;

const DERIVE_CONTEXT: &str = "tilo sibling unlock 2026-10 luks2 slot passphrase";

/// A network early boot may join to reach a sibling: one the owner chose for
/// unlocking. The profile is a NetworkManager keyfile, secrets included.
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
    /// The device half, hex.
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
    /// The device half as bytes.
    ///
    /// # Errors
    /// When it is not 32 bytes of hex.
    pub fn device_half(&self) -> Result<Zeroizing<[u8; HALF_LEN]>> {
        decode_secret(&self.device_half, "device half")
    }

    /// The boot key's secret as bytes.
    ///
    /// # Errors
    /// When it is not 32 bytes of hex.
    pub fn boot_secret(&self) -> Result<Zeroizing<[u8; HALF_LEN]>> {
        decode_secret(&self.boot_secret, "boot key")
    }

    /// Serialises the material for sealing.
    ///
    /// # Errors
    /// Never in practice; serde_json reports allocation failure.
    pub fn to_json(&self) -> Result<Zeroizing<Vec<u8>>> {
        Ok(Zeroizing::new(serde_json::to_vec(self)?))
    }

    /// Parses unsealed material.
    ///
    /// # Errors
    /// When the bytes are not the material's JSON or its secrets are malformed.
    pub fn from_json(bytes: &[u8]) -> Result<Self> {
        let material: Self =
            serde_json::from_slice(bytes).context("the unlock material is malformed")?;
        material.device_half()?;
        material.boot_secret()?;
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

/// The passphrase of the sibling key slot: 64 hex characters derived from
/// both halves, the volume's UUID and the slot.
pub fn slot_passphrase(
    device_half: &[u8; HALF_LEN],
    sibling_half: &[u8; HALF_LEN],
    luks_uuid: &str,
    keyslot: u32,
) -> Zeroizing<String> {
    let mut input = Zeroizing::new(Vec::with_capacity(HALF_LEN * 2 + 64));
    input.extend_from_slice(device_half);
    input.extend_from_slice(sibling_half);
    input.extend_from_slice(luks_uuid.to_ascii_lowercase().as_bytes());
    input.extend_from_slice(&keyslot.to_le_bytes());
    let key = Zeroizing::new(blake3::derive_key(DERIVE_CONTEXT, &input));
    Zeroizing::new(hex::encode(key.as_slice()))
}

/// Fresh random bytes for a half or a key.
///
/// # Errors
/// When the OS has no randomness to give.
pub fn random_half() -> Result<Zeroizing<[u8; HALF_LEN]>> {
    let mut out = Zeroizing::new([0_u8; HALF_LEN]);
    getrandom::fill(out.as_mut_slice())
        .map_err(|error| anyhow::anyhow!("no randomness: {error}"))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_passphrase_needs_both_halves_and_is_bound_to_the_slot() {
        let device = [1_u8; HALF_LEN];
        let sibling = [2_u8; HALF_LEN];
        let uuid = "6f1c5ae2-1111-2222-3333-444455556666";
        let key = slot_passphrase(&device, &sibling, uuid, 3);
        assert_eq!(key.len(), 64);
        assert_eq!(
            *key,
            *slot_passphrase(&device, &sibling, &uuid.to_uppercase(), 3)
        );
        assert_ne!(*key, *slot_passphrase(&device, &[3; HALF_LEN], uuid, 3));
        assert_ne!(*key, *slot_passphrase(&[0; HALF_LEN], &sibling, uuid, 3));
        assert_ne!(*key, *slot_passphrase(&device, &sibling, uuid, 4));
        assert_ne!(
            *key,
            *slot_passphrase(&device, &sibling, "6f1c5ae2-1111-2222-3333-444455556667", 3)
        );
    }

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
        assert_eq!(*back.device_half().unwrap(), [0x11; HALF_LEN]);
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
