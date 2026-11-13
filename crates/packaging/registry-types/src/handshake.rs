//! The `/registry` endpoint response: the greeting a server returns on first contact, carrying the
//! owner key to pin, the protocol version and how the operator introduces the registry.

use serde::{Deserialize, Serialize};

/// Increment on any change to the JSON shape or the canonical signing form.
pub const PROTOCOL_VERSION: u32 = 1;

/// Everything a client learns on the first `GET /registry` call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryHandshake {
    /// Clients bail on a version they do not understand.
    pub protocol_version: u32,
    /// The owner's Ed25519 public key, hex: federation signatures verify against it.
    pub owner_pubkey_hex: String,
    pub name: String,
    pub description: String,
    /// Entry and artifact URLs are relative to this.
    pub base_url: String,
}

impl RegistryHandshake {
    pub fn new(
        owner_pubkey_hex: impl Into<String>,
        name: impl Into<String>,
        description: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            owner_pubkey_hex: owner_pubkey_hex.into(),
            name: name.into(),
            description: description.into(),
            base_url: base_url.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_json_roundtrip() {
        let h = RegistryHandshake::new(
            "aa".repeat(32),
            "Tilo official",
            "The default registry shipped with Tilo.",
            "https://apps.tilo.so",
        );
        let s = serde_json::to_string(&h).expect("encode");
        let back: RegistryHandshake = serde_json::from_str(&s).expect("decode");
        assert_eq!(back, h);
        assert_eq!(back.protocol_version, PROTOCOL_VERSION);
    }
}
