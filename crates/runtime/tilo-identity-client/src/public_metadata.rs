use serde::{Deserialize, Serialize};

/// How a record's private share is protected on this device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyProtection {
    /// Stored in the desktop's freedesktop Secret Service collection.
    SecretService,
    /// Stored as a mode-0600 host file, reachable only by the signing broker.
    BrokerFile,
    /// An externally supplied provider has sealed this record.
    HardwareBacked,
    /// Legacy schema; migration has not completed yet.
    LegacyPlaintext,
    /// No share of the identity's key is on this device: a device joined to
    /// the identity acts through its root-signed device certificate.
    DeviceCertificate,
    /// Metadata names a backend this session cannot safely resolve.
    Unavailable,
}

impl KeyProtection {
    pub fn label(self) -> &'static str {
        match self {
            Self::SecretService => "Secret Service + app-scoped signing broker",
            Self::BrokerFile => "Private session file + app-scoped signing broker",
            Self::HardwareBacked => "Hardware-backed + app-scoped signing broker",
            Self::LegacyPlaintext => "Migration required",
            Self::DeviceCertificate => {
                "No identity key here; this device signs with its certificate"
            }
            Self::Unavailable => "Protection provider unavailable",
        }
    }
}

/// Non-secret information Settings may show for an identity record. The
/// private signing share is absent and `Debug` is safe. It is what the
/// host's identity actions answer, with the public key in hex.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentitySummary {
    pub id: String,
    pub display_name: String,
    #[serde(with = "public_key_hex")]
    pub public_key: [u8; 32],
    pub threshold: u16,
    pub total_shares: u16,
    pub active: bool,
    pub protection: KeyProtection,
}

mod public_key_hex {
    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(
        key: &[u8; 32],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&hex::encode(key))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<[u8; 32], D::Error> {
        let text = String::deserialize(deserializer)?;
        let mut key = [0; 32];
        hex::decode_to_slice(text, &mut key).map_err(serde::de::Error::custom)?;
        Ok(key)
    }
}
