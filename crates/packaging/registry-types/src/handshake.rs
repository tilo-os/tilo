//! The `/registry` endpoint response: the greeting the server
//! returns to any client on first contact so the client knows
//! which owner-pubkey to pin, what protocol version to speak,
//! and how the operator wants the registry introduced.

use serde::{Deserialize, Serialize};

/// The wire protocol version this crate defines. Increment on any
/// change to the JSON shape or the canonical signing form.
pub const PROTOCOL_VERSION: u32 = 1;

/// Everything a client learns on the first `GET /registry` call.
///
/// The `owner_pubkey_hex` is the identity a client must pin to
/// trust anything else this registry serves. Clients that already
/// pin the same key will accept the entries; clients hitting a
/// fresh registry are expected to prompt the user before
/// pinning.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryHandshake {
    /// Wire protocol version. Starts at 1; clients bail if they
    /// see a version they don't understand.
    pub protocol_version: u32,
    /// The registry owner's Ed25519 public key (32 bytes, hex).
    /// This is the key that federation signatures verify
    /// against, and the anchor of trust for the whole registry.
    pub owner_pubkey_hex: String,
    /// Human-readable registry name (for UI listings).
    pub name: String,
    /// Human-readable description shown alongside the name.
    pub description: String,
    /// The URL the registry is reachable at. Used by clients when
    /// composing entry-download URLs relative to the registry.
    pub base_url: String,
}

impl RegistryHandshake {
    /// Convenience constructor for the current protocol version.
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
            "qOS official",
            "The default registry shipped with qOS.",
            "https://apps.qos.dev",
        );
        let s = serde_json::to_string(&h).expect("encode");
        let back: RegistryHandshake = serde_json::from_str(&s).expect("decode");
        assert_eq!(back, h);
        assert_eq!(back.protocol_version, PROTOCOL_VERSION);
    }
}
