//! Versioned local registry records for enrolled always-on peers.

use serde::{Deserialize, Serialize};

/// Durable registry record written after enrollment acknowledgement completes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HomePeerRegistration {
    /// Registry schema version; readers accept only [`Self::CURRENT_SCHEMA`].
    pub schema_version: u16,
    /// Certified filesystem/device endpoint key, lowercase hex.
    pub device_node_id: String,
    /// Domain-separated relay service endpoint key, lowercase hex.
    pub relay_service_node_id: String,
    /// Last authenticated, network-addressable endpoint advertisement for
    /// the relay service. The service id alone is insufficient without an
    /// address lookup service; absent until the peer advertises one.
    #[serde(default)]
    pub relay_endpoint: Option<iroh_base::EndpointAddr>,
    /// User-visible device name.
    pub label: String,
    /// Root identity key, lowercase hex.
    pub user_public_key: String,
    /// Root-signed host certificate, retained so normal fleet revocation can
    /// manage a headless peer.
    #[serde(default)]
    pub device_cert: Option<tilo_user_identity::DeviceCert>,
    /// Relay room shards this peer was last configured to retain.
    pub relay_workspace_ids: Vec<u64>,
}

impl HomePeerRegistration {
    /// Schema version written by this build.
    pub const CURRENT_SCHEMA: u16 = 3;
}

/// Whether an operator-facing device label is safe to render as one line.
pub fn valid_home_peer_label(label: &str) -> bool {
    let label = label.trim();
    !label.is_empty()
        && label.len() <= 80
        && label.chars().all(|character| {
            !character.is_control()
                && !matches!(
                    character,
                    '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
                )
        })
}
