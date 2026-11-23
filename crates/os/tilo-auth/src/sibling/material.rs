//! The secret this machine keeps for a sibling unlock, and the key it derives.
//!
//! * the **device half**, kept here, in [`Material`];
//! * the **sibling half**, held only by the person's other devices, which
//!   release it for one boot after the owner approves.

use anyhow::{Context, Result, bail};
use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use tilo_unlock_protocol::{HALF_LEN, Half};
use zeroize::{Zeroize, Zeroizing};

/// A network early boot may join to reach a sibling: one the owner chose for
/// unlocking. The profile is a `NetworkManager` keyfile, secrets included.
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Network {
    pub id: String,
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

/// The TPM attestation key this machine quotes with, as `tpm2_create` left
/// it. Both blobs are bound to the TPM's owner-hierarchy primary: another
/// machine cannot load them, and the private half is encrypted by the TPM.
#[derive(Clone, Serialize, Deserialize)]
pub struct Attestation {
    /// The key's `TPM2B_PUBLIC`, hex.
    pub public: String,
    /// The key's `TPM2B_PRIVATE`, hex.
    pub private: String,
}

impl std::fmt::Debug for Attestation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Attestation { .. }")
    }
}

impl Attestation {
    /// The `TPM2B_PUBLIC` a quote is verified against, through the SEC1
    /// point the identity certified.
    ///
    /// # Errors
    /// When either blob is not hex.
    pub fn blobs(&self) -> Result<(Vec<u8>, Vec<u8>)> {
        Ok((
            hex::decode(&self.public).context("the attestation key is not hex")?,
            hex::decode(&self.private).context("the attestation key's private half is not hex")?,
        ))
    }
}

#[derive(Serialize, Deserialize)]
pub struct Material {
    pub device_half: String,
    /// The boot key's 32-byte Ed25519 secret, hex.
    pub boot_secret: String,
    /// Owner-chosen unlock networks. Ethernet needs none: every wired link is
    /// tried with DHCP.
    #[serde(default)]
    pub networks: Vec<Network>,
    /// The attestation key, when the machine has a TPM. Only a machine with
    /// one can attest its boot chain and be released without a prompt.
    #[serde(default)]
    pub attestation: Option<Attestation>,
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
        if let Some(attestation) = &mut self.attestation {
            attestation.private.zeroize();
        }
    }
}

impl Material {
    pub fn new(
        device_half: &Half,
        boot_key: &SigningKey,
        networks: Vec<Network>,
        attestation: Option<Attestation>,
    ) -> Self {
        Self {
            device_half: hex::encode(device_half.expose()),
            boot_secret: hex::encode(boot_key.to_bytes()),
            networks,
            attestation,
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
        if let Some(attestation) = &material.attestation {
            attestation.blobs()?;
        }
        Ok(material)
    }
}

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
