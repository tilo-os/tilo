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

    /// The greeting's JSON is the first thing a client reads; its field names and the version a
    /// client bails on are the wire contract, so a rename must fail here rather than in the field.
    #[test]
    fn the_greeting_json_names_the_fields_clients_read() {
        let owner = "aa".repeat(32);
        let handshake = RegistryHandshake::new(
            owner.clone(),
            "Tilo official",
            "The default registry shipped with Tilo.",
            "https://apps.tilo.so",
        );
        let json = serde_json::to_value(&handshake).expect("encode");
        assert_eq!(
            json,
            serde_json::json!({
                "protocol_version": PROTOCOL_VERSION,
                "owner_pubkey_hex": owner,
                "name": "Tilo official",
                "description": "The default registry shipped with Tilo.",
                "base_url": "https://apps.tilo.so",
            })
        );
        assert_eq!(
            serde_json::from_value::<RegistryHandshake>(json).expect("decode"),
            handshake
        );
    }
}
