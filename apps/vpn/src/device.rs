//! What this device knows of the shared profiles. Never synced, never in the
//! document, never in an action, a search index or a log.
//!
//! [`VpnDevice`] is the app's device-local setting: for each shared profile
//! set up here, the `NetworkManager` UUID of its exact local profile and the
//! display facts parsed from the provider file (endpoint, address, DNS,
//! routes, public key), plus whether this device routes all traffic over
//! Tor. `NetworkManager` keeps the configuration and every credential; a
//! lost entry is recoverable, the UI asks to set the profile up here again.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use ui::storage::{Scope, SettingsSchema};

use crate::model::{ConnId, ConnKind, ConnStatus, LocalProfileDetails, Vault};

/// This device's VPN state.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct VpnDevice {
    /// Shared profile id → its exact local profile and display facts.
    #[serde(default)]
    pub profiles: HashMap<ConnId, LocalProfileDetails>,
    /// Whether this device's Tor service was last started by the app.
    #[serde(default)]
    pub tor_global: bool,
}

impl SettingsSchema for VpnDevice {
    const SCOPE: Scope = Scope::DeviceLocal;
}

/// What a screenshot shows of a device in place of `NetworkManager`: the
/// profiles set up there and their tunnels' states.
#[derive(Clone, Debug, Default)]
pub struct DeviceFacts {
    pub profiles: HashMap<ConnId, LocalProfileDetails>,
    pub status: HashMap<ConnId, ConnStatus>,
}

impl DeviceFacts {
    /// A device with every profile of `vault` set up, from deterministic
    /// fictional values, and its tunnels in the vault's states.
    pub fn demo(vault: &Vault) -> Self {
        let profiles = vault
            .connections
            .iter()
            .map(|connection| {
                let details = match connection.kind {
                    ConnKind::WireGuard => LocalProfileDetails {
                        endpoint: "demo-gateway.example:51820".into(),
                        address: "10.77.0.2/32".into(),
                        dns: "10.77.0.1".into(),
                        public_key: "demo public key ••••7Jq".into(),
                        allowed_ips: "0.0.0.0/0, ::/0".into(),
                        ..LocalProfileDetails::default()
                    },
                    ConnKind::OpenVpn => LocalProfileDetails {
                        endpoint: "demo-vpn.example:1194 (UDP)".into(),
                        address: "172.20.77.2/24".into(),
                        dns: "172.20.77.1".into(),
                        allowed_ips: "172.20.0.0/16".into(),
                        ..LocalProfileDetails::default()
                    },
                };
                (connection.id, details)
            })
            .collect();
        let status = vault
            .connections
            .iter()
            .map(|connection| (connection.id, connection.status))
            .collect();
        Self { profiles, status }
    }
}
