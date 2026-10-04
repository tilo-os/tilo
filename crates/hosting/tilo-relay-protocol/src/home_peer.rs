//! Versioned local registry records for enrolled always-on peers.

use serde::{Deserialize, Serialize};

pub use tilo_user_identity::home_peer_registry_path;

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
    /// What the peer last said it runs for environments: whether a worker
    /// hosts environment sessions, and whether one runs legacy apps. A
    /// device refreshes it from the peer's management status; a relay
    /// with no environment worker is not offered as a place to run one.
    #[serde(default)]
    pub environments: RelayEnvironments,
}

/// What a device last heard from its relays about environments, newer than
/// their registrations: `relay service node id (hex) → offer`. Kept per
/// user in their cache ([`RELAY_OFFERS_FILE`]), since the registrations
/// beside the identity keys are not the user's to rewrite.
pub type RelayOffers = std::collections::BTreeMap<String, RelayOffer>;

/// The file below the user's cache directory holding [`RelayOffers`].
pub const RELAY_OFFERS_FILE: &str = "tilo/relay-offers.json";

/// One relay's answer, and when it was asked.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayOffer {
    /// What it said its workers run; kept from before when it did not
    /// answer.
    pub environments: RelayEnvironments,
    /// When it was last asked, Unix milliseconds.
    pub checked_at_ms: u64,
}

/// What a relay currently runs for environments, from its workers' live
/// advertisements.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayEnvironments {
    /// A worker hosts interactive environment sessions now.
    #[serde(default)]
    pub sessions: bool,
    /// A worker advertising `kvm` runs legacy apps now.
    #[serde(default)]
    pub legacy_apps: bool,
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
