//! Host-to-app projection of human identities admitted by governance.
//!
//! This is not an address-book format. A producer may add a
//! row only after it has associated the human root key with an authoritative
//! governance name.  Device labels, presence payloads and Contacts entries
//! are not such an association.  When that association is unavailable the
//! correct projection is an empty one.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use crate::{DeviceCert, verify_device_cert};

/// Current JSON projection schema.
pub const TRUSTED_IDENTITY_PROJECTION_V1_VERSION: u16 = 1;
/// Current projection schema, adding root-signed device certificates.
pub const TRUSTED_IDENTITY_PROJECTION_VERSION: u16 = 2;

/// Maximum serialized projection accepted across the host/app boundary.
pub const MAX_TRUSTED_IDENTITY_PROJECTION_BYTES: u64 = 128 * 1024;
/// Maximum number of human roots in one projection.
pub const MAX_TRUSTED_IDENTITY_PROJECTION_HUMANS: usize = 256;
/// Maximum certified devices across all humans in one projection.
pub const MAX_TRUSTED_IDENTITY_PROJECTION_DEVICES: usize = 1024;
/// Maximum UTF-8 byte length of a governance-controlled display name.
pub const MAX_TRUSTED_IDENTITY_DISPLAY_NAME_BYTES: usize = 128;

/// Why a host identity projection is not safe to publish or consume.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TrustedIdentityProjectionError {
    /// The reader does not implement this schema version.
    #[error("unsupported trusted identity projection schema")]
    UnsupportedSchema,
    /// The projection exceeds its row bound.
    #[error("trusted identity projection contains too many humans")]
    TooManyHumans,
    /// A root is not a valid Ed25519 verification key.
    #[error("trusted identity projection contains an invalid human root key")]
    InvalidRootKey,
    /// Two labels attempt to describe the same authority key.
    #[error("trusted identity projection contains a duplicate human root key")]
    DuplicateRootKey,
    /// A label is empty, untrimmed, too long, or contains a control character.
    #[error("trusted identity projection contains an invalid display name")]
    InvalidDisplayName,
    /// The projection exceeds its device-row bound.
    #[error("trusted identity projection contains too many certified devices")]
    TooManyDevices,
    /// A device certificate is invalid or does not belong to its enclosing root.
    #[error("trusted identity projection contains an invalid device certificate")]
    InvalidDeviceCertificate,
    /// One device key appears under more than one row/root.
    #[error("trusted identity projection contains a duplicate device key")]
    DuplicateDeviceKey,
}

/// A bounded, versioned snapshot written by the session and read by an app.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedIdentityProjectionV1 {
    /// Must equal [`TRUSTED_IDENTITY_PROJECTION_V1_VERSION`].
    pub schema_version: u16,
    /// Governance-admitted human root keys and their governed labels.
    pub humans: Vec<GovernedHumanIdentityV1>,
}

impl Default for TrustedIdentityProjectionV1 {
    fn default() -> Self {
        Self::empty()
    }
}

impl TrustedIdentityProjectionV1 {
    /// Fail-closed projection used until governance has an authoritative
    /// root-key-to-name association to publish.
    pub const fn empty() -> Self {
        Self {
            schema_version: TRUSTED_IDENTITY_PROJECTION_V1_VERSION,
            humans: Vec::new(),
        }
    }

    /// Validate all semantic bounds before a projection crosses the
    /// host/app boundary.
    ///
    /// # Errors
    ///
    /// Returns an error when the schema, root keys, cardinality, uniqueness,
    /// or governed display names violate the projection contract.
    pub fn validate(&self) -> Result<(), TrustedIdentityProjectionError> {
        if self.schema_version != TRUSTED_IDENTITY_PROJECTION_V1_VERSION {
            return Err(TrustedIdentityProjectionError::UnsupportedSchema);
        }
        if self.humans.len() > MAX_TRUSTED_IDENTITY_PROJECTION_HUMANS {
            return Err(TrustedIdentityProjectionError::TooManyHumans);
        }
        let mut roots = BTreeSet::new();
        for human in &self.humans {
            ed25519_dalek::VerifyingKey::from_bytes(&human.root_public_key)
                .map_err(|_| TrustedIdentityProjectionError::InvalidRootKey)?;
            if !roots.insert(human.root_public_key) {
                return Err(TrustedIdentityProjectionError::DuplicateRootKey);
            }
            if human.display_name.is_empty()
                || human.display_name.trim() != human.display_name
                || human.display_name.len() > MAX_TRUSTED_IDENTITY_DISPLAY_NAME_BYTES
                || human.display_name.chars().any(char::is_control)
            {
                return Err(TrustedIdentityProjectionError::InvalidDisplayName);
            }
        }
        Ok(())
    }
}

/// Current bounded snapshot: governed human labels plus exact root-signed
/// device certificates. The certificate label is device presentation only;
/// the enclosing governed name is the human label Files may use for sharing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedIdentityProjectionV2 {
    /// Must equal [`TRUSTED_IDENTITY_PROJECTION_VERSION`].
    pub schema_version: u16,
    /// Governance-admitted human roots, grouped with their certified devices.
    pub humans: Vec<GovernedHumanIdentityV2>,
}

impl Default for TrustedIdentityProjectionV2 {
    fn default() -> Self {
        Self::empty()
    }
}

impl TrustedIdentityProjectionV2 {
    /// Empty fail-closed current projection.
    pub const fn empty() -> Self {
        Self {
            schema_version: TRUSTED_IDENTITY_PROJECTION_VERSION,
            humans: Vec::new(),
        }
    }

    /// Verify schema, cardinality, names, unique roots/devices, and every
    /// root-signed device certificate.
    ///
    /// # Errors
    ///
    /// Returns an error when any projection field or device certificate is
    /// malformed, duplicated, unsupported, or fails signature verification.
    pub fn validate(&self) -> Result<(), TrustedIdentityProjectionError> {
        if self.schema_version != TRUSTED_IDENTITY_PROJECTION_VERSION {
            return Err(TrustedIdentityProjectionError::UnsupportedSchema);
        }
        if self.humans.len() > MAX_TRUSTED_IDENTITY_PROJECTION_HUMANS {
            return Err(TrustedIdentityProjectionError::TooManyHumans);
        }
        let mut roots = BTreeSet::new();
        let mut devices = BTreeSet::new();
        let mut device_count = 0usize;
        for human in &self.humans {
            ed25519_dalek::VerifyingKey::from_bytes(&human.root_public_key)
                .map_err(|_| TrustedIdentityProjectionError::InvalidRootKey)?;
            if !roots.insert(human.root_public_key) {
                return Err(TrustedIdentityProjectionError::DuplicateRootKey);
            }
            validate_name(&human.display_name)?;
            device_count = device_count.saturating_add(human.devices.len());
            if device_count > MAX_TRUSTED_IDENTITY_PROJECTION_DEVICES {
                return Err(TrustedIdentityProjectionError::TooManyDevices);
            }
            for device in &human.devices {
                verify_device_cert(&device.certificate)
                    .map_err(|_| TrustedIdentityProjectionError::InvalidDeviceCertificate)?;
                if device.certificate.user_public_key != human.root_public_key {
                    return Err(TrustedIdentityProjectionError::InvalidDeviceCertificate);
                }
                if !devices.insert(device.certificate.device_public_key) {
                    return Err(TrustedIdentityProjectionError::DuplicateDeviceKey);
                }
            }
        }
        Ok(())
    }
}

/// One governance-named human root and its certified devices.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GovernedHumanIdentityV2 {
    /// Ed25519 human root authority.
    pub root_public_key: [u8; 32],
    /// Name admitted by room governance.
    pub display_name: String,
    /// Root-signed devices currently known to room governance.
    pub devices: Vec<GovernedDeviceIdentityV2>,
}

/// Exact signed device detail nested under its human root.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GovernedDeviceIdentityV2 {
    /// Root-signed device certificate, including label, capabilities and expiry.
    pub certificate: DeviceCert,
}

fn validate_name(name: &str) -> Result<(), TrustedIdentityProjectionError> {
    if name.is_empty()
        || name.trim() != name
        || name.len() > MAX_TRUSTED_IDENTITY_DISPLAY_NAME_BYTES
        || name.chars().any(char::is_control)
    {
        return Err(TrustedIdentityProjectionError::InvalidDisplayName);
    }
    Ok(())
}

/// One human root key whose display name came from room governance.
///
/// The root key is the authority handed to sharing APIs. `display_name` is
/// presentation metadata and must never be converted back into authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GovernedHumanIdentityV1 {
    /// Ed25519 human root key certified by an admitted device chain.
    pub root_public_key: [u8; 32],
    /// Governance-controlled presentation label for this root key.
    pub display_name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn certificate(root: &ed25519_dalek::SigningKey, device_seed: u8) -> DeviceCert {
        let device = ed25519_dalek::SigningKey::from_bytes(&[device_seed; 32]);
        let mut certificate = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Framework 16".into(),
            capabilities: crate::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        crate::sign_device_cert(&mut certificate, root).unwrap();
        certificate
    }

    /// Validation rejects ambiguous or unpresentable rows, and v2 binds each device certificate to one
    /// governed human root.
    #[test]
    fn identity_projections_are_unambiguous_and_bind_devices_to_one_root() {
        // Phase: validation_rejects_ambiguous_or_unpresentable_rows.
        {
            let root = ed25519_dalek::SigningKey::from_bytes(&[4; 32])
                .verifying_key()
                .to_bytes();
            let row = |name: &str| GovernedHumanIdentityV1 {
                root_public_key: root,
                display_name: name.to_string(),
            };
            let duplicate = TrustedIdentityProjectionV1 {
                schema_version: TRUSTED_IDENTITY_PROJECTION_V1_VERSION,
                humans: vec![row("Ada"), row("Grace")],
            };
            assert_eq!(
                duplicate.validate(),
                Err(TrustedIdentityProjectionError::DuplicateRootKey)
            );
            let untrimmed = TrustedIdentityProjectionV1 {
                schema_version: TRUSTED_IDENTITY_PROJECTION_V1_VERSION,
                humans: vec![row(" Ada ")],
            };
            assert_eq!(
                untrimmed.validate(),
                Err(TrustedIdentityProjectionError::InvalidDisplayName)
            );
        }

        // Phase: v2_binds_each_device_certificate_to_one_governed_human_root.
        {
            let root = ed25519_dalek::SigningKey::from_bytes(&[21; 32]);
            let certificate = certificate(&root, 22);
            let projection = TrustedIdentityProjectionV2 {
                schema_version: TRUSTED_IDENTITY_PROJECTION_VERSION,
                humans: vec![GovernedHumanIdentityV2 {
                    root_public_key: root.verifying_key().to_bytes(),
                    display_name: "Ada Lovelace".into(),
                    devices: vec![GovernedDeviceIdentityV2 { certificate }],
                }],
            };
            projection.validate().unwrap();

            let mut wrong_root = projection.clone();
            wrong_root.humans[0].root_public_key = ed25519_dalek::SigningKey::from_bytes(&[23; 32])
                .verifying_key()
                .to_bytes();
            assert_eq!(
                wrong_root.validate(),
                Err(TrustedIdentityProjectionError::InvalidDeviceCertificate)
            );
            let mut tampered = projection;
            tampered.humans[0].devices[0].certificate.device_label = "Impostor laptop".into();
            assert_eq!(
                tampered.validate(),
                Err(TrustedIdentityProjectionError::InvalidDeviceCertificate)
            );
        }
    }
}
