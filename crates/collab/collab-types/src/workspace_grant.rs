//! One grant a workspace admitted: each key the person approved for an agent,
//! a backend or a script, with the workspace it reaches and until when.
//!
//! A grant is written by the host that admitted its key and read by every
//! holder of the workspace, so the grant exists wherever the workspace is
//! held and whichever API server serves it. It names the key by its device
//! public key, which is what the workspace's access admits and refuses; the
//! key's certificate remains the grant's proof, and this record is what the
//! workspace lists and revokes.
use serde::{Deserialize, Serialize};

/// Key of the workspace's grants in its governance document: a map from each
/// grant's id to the grant (JSON).
pub const WORKSPACE_GRANTS_KEY: &str = "key_grants";

/// One grant the workspace admitted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceGrant {
    /// The grant's id, the same at every API server that reads it.
    pub id: String,
    /// The person whose key it is, hex.
    pub person: String,
    /// The key's device public key, hex: the device the workspace admits.
    pub device: String,
    /// What the person approved it for, like "Visa backend".
    pub name: String,
    /// The workspace it reaches alone; `None` for every workspace of the
    /// person's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// When it stops working, Unix milliseconds; `None` until its person
    /// revokes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_ms: Option<u64>,
    /// The grant that minted it, when one did: the grant whose key certified
    /// this one, so the key that made it lists it. `None` for a key its
    /// person approved at an API server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// When the workspace admitted it, Unix milliseconds.
    pub created_ms: u64,
    /// When its person revoked it, Unix milliseconds; `None` while live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_ms: Option<u64>,
}

pub use tilo_action_policy::{key_agent_id, key_of_agent};

impl WorkspaceGrant {
    /// The id a key's grant is known by wherever it is read: its device key's
    /// digest, so every API server and every holder derive the same grant
    /// from the same key.
    #[must_use]
    pub fn id_of(device_public_key: &[u8; 32]) -> String {
        format!(
            "grant-{}",
            hex::encode(&blake3::hash(device_public_key).as_bytes()[..12])
        )
    }
}
