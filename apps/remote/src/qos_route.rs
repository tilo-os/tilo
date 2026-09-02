//! qOS-device session route adapter.
//!
//! Discovery remains owned by the device-mesh runtime. Remote accepts a
//! verified snapshot, delegates direct/relay selection to `qos-relay`, and
//! reports the decision to session setup. It never turns configuration into a
//! reachability claim.

use qos_relay::{RemoteControlRoute, ServerAdvertisement, route_remote_control};

/// Certified-device routing key.
pub type DeviceKey = [u8; 32];

/// Route selected for an end-to-end qOS control session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QosRouteDecision {
    /// Direct device-to-device reachability is live.
    Direct,
    /// The encrypted connection must tunnel through this always-on device.
    Relayed { via: DeviceKey },
}

/// Route lookup consumed at the same point SSH/VNC create their transports.
/// Implementations must only return observations from authenticated discovery.
pub trait QosRouteProvider: Send + Sync {
    /// Select a route for the saved target hint.
    fn route(&self, target_hint: &str) -> Result<QosRouteDecision, String>;
}

/// Default provider until the shell injects its live device-mesh snapshot.
pub struct UnavailableQosRouteProvider;

impl QosRouteProvider for UnavailableQosRouteProvider {
    fn route(&self, _target_hint: &str) -> Result<QosRouteDecision, String> {
        Err(
            "Device-mesh reachability is not attached to this Remote process; no route was attempted"
                .to_string(),
        )
    }
}

/// Authenticated discovery state supplied by the shell/device runtime.
#[derive(Clone, Debug)]
pub struct MeshRouteSnapshot {
    /// Saved friendly/node identifier this snapshot resolves.
    pub target_hint: String,
    /// Current device.
    pub client: DeviceKey,
    /// Certified target device.
    pub target: DeviceKey,
    /// Workspace whose sharing/authorization scope governs the session.
    pub workspace_id: u64,
    /// Whether authenticated discovery currently has a direct target path.
    pub direct_reachable: bool,
    /// Verified same-workspace server advertisements.
    pub servers: Vec<ServerAdvertisement>,
}

/// Provider backed by one fresh, authenticated mesh snapshot.
pub struct MeshQosRouteProvider {
    snapshot: MeshRouteSnapshot,
}

impl MeshQosRouteProvider {
    /// Wrap a runtime-produced snapshot.
    pub fn new(snapshot: MeshRouteSnapshot) -> Self {
        Self { snapshot }
    }
}

impl QosRouteProvider for MeshQosRouteProvider {
    fn route(&self, target_hint: &str) -> Result<QosRouteDecision, String> {
        if target_hint != self.snapshot.target_hint {
            return Err(
                "Saved device does not match the authenticated discovery record".to_string(),
            );
        }
        match route_remote_control(
            self.snapshot.client,
            self.snapshot.target,
            self.snapshot.workspace_id,
            self.snapshot.direct_reachable,
            &self.snapshot.servers,
        ) {
            RemoteControlRoute::Direct { .. } => Ok(QosRouteDecision::Direct),
            RemoteControlRoute::Relayed { via, .. } => Ok(QosRouteDecision::Relayed { via }),
            RemoteControlRoute::Unavailable => Err(
                "No direct path or eligible online server is available for this workspace"
                    .to_string(),
            ),
        }
    }
}

/// Short non-authoritative device fingerprint for status/error copy.
pub fn short_device_id(device: &DeviceKey) -> String {
    let mut bytes = device.iter().copied();
    format!(
        "{:02x}{:02x}{:02x}{:02x}",
        bytes.next().unwrap_or_default(),
        bytes.next().unwrap_or_default(),
        bytes.next().unwrap_or_default(),
        bytes.next().unwrap_or_default()
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use qos_relay::{DeviceRole, ServiceCapabilities};

    use super::*;

    fn server(id: u8) -> ServerAdvertisement {
        ServerAdvertisement {
            device: [id; 32],
            role: DeviceRole::AlwaysOnServer,
            services: ServiceCapabilities::personal_server(),
            workspaces: BTreeSet::from([7]),
            apps: BTreeSet::new(),
            priority: 0,
            latency_ms: 12,
            active_assignments: 0,
            assignment_capacity: 8,
            online: true,
        }
    }

    fn provider(direct: bool, servers: Vec<ServerAdvertisement>) -> MeshQosRouteProvider {
        MeshQosRouteProvider::new(MeshRouteSnapshot {
            target_hint: "studio-node".to_string(),
            client: [1; 32],
            target: [2; 32],
            workspace_id: 7,
            direct_reachable: direct,
            servers,
        })
    }

    #[test]
    fn remote_consumes_direct_first_mesh_routing() {
        assert_eq!(
            provider(true, vec![server(3)]).route("studio-node"),
            Ok(QosRouteDecision::Direct)
        );
    }

    #[test]
    fn remote_uses_real_relay_fallback() {
        assert_eq!(
            provider(false, vec![server(3)]).route("studio-node"),
            Ok(QosRouteDecision::Relayed { via: [3; 32] })
        );
    }

    #[test]
    fn stale_or_missing_discovery_never_becomes_success() {
        assert!(provider(false, Vec::new()).route("studio-node").is_err());
        assert!(provider(true, Vec::new()).route("different-node").is_err());
        assert!(UnavailableQosRouteProvider.route("studio-node").is_err());
    }
}
