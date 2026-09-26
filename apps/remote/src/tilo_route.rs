//! Tilo-device session route adapter.
//!
//! Discovery remains owned by the device-mesh runtime. Remote accepts a
//! verified snapshot and delegates route selection to the relay's shared
//! mesh placement (`tilo-relay-protocol`). Only an
//! exact-target route is accepted for control; a workspace-wide relay is
//! reported as unavailable rather than leaking input through fan-out.

use tilo_relay_protocol::{RemoteControlRoute, ServerAdvertisement, route_remote_control};

/// Certified-device routing key.
pub type DeviceKey = [u8; 32];

/// Route selected for an end-to-end Tilo control session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TiloRouteDecision {
    /// Direct device-to-device reachability is live.
    Direct,
    /// A relay candidate, retained for callers that need to explain why the
    /// route was rejected for exact-target control.
    Relayed { via: DeviceKey },
}

/// Route lookup consumed at the same point SSH/VNC create their transports.
/// Implementations must only return observations from authenticated discovery.
pub trait TiloRouteProvider: Send + Sync {
    /// Select a route for the saved target hint.
    ///
    /// # Errors
    ///
    /// Returns an error when authenticated discovery has no acceptable
    /// exact-target route for the saved device.
    fn route(&self, target_hint: &str) -> Result<TiloRouteDecision, String>;
}

/// Default provider until the shell injects its live device-mesh snapshot.
pub struct UnavailableTiloRouteProvider;

impl TiloRouteProvider for UnavailableTiloRouteProvider {
    fn route(&self, _target_hint: &str) -> Result<TiloRouteDecision, String> {
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
pub struct MeshTiloRouteProvider {
    snapshot: MeshRouteSnapshot,
}

impl MeshTiloRouteProvider {
    /// Wrap a runtime-produced snapshot.
    pub fn new(snapshot: MeshRouteSnapshot) -> Self {
        Self { snapshot }
    }
}

impl TiloRouteProvider for MeshTiloRouteProvider {
    fn route(&self, target_hint: &str) -> Result<TiloRouteDecision, String> {
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
            RemoteControlRoute::Direct { .. } => Ok(TiloRouteDecision::Direct),
            RemoteControlRoute::Relayed { .. } => Err(
                "Workspace control needs an exact-target route; the available relay is workspace-wide"
                    .to_string(),
            ),
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

    use tilo_relay_protocol::{DeviceRole, ServiceCapabilities};

    use super::*;

    fn server(id: u8) -> ServerAdvertisement {
        ServerAdvertisement {
            device: [id; 32],
            role: DeviceRole::AlwaysOnServer,
            services: ServiceCapabilities::personal_server(),
            workspaces: BTreeSet::from([7]),
            apps: BTreeSet::new(),
            accelerators: BTreeSet::new(),
            priority: 0,
            latency_ms: 12,
            power: tilo_relay_protocol::PowerAvailability::External,
            energy_saver: false,
            active_assignments: 0,
            assignment_capacity: 8,
            online: true,
        }
    }

    fn provider(direct: bool, servers: Vec<ServerAdvertisement>) -> MeshTiloRouteProvider {
        MeshTiloRouteProvider::new(MeshRouteSnapshot {
            target_hint: "studio-node".to_string(),
            client: [1; 32],
            target: [2; 32],
            workspace_id: 7,
            direct_reachable: direct,
            servers,
        })
    }

    #[test]
    fn tilo_routes_prefer_direct_mesh_and_never_fake_success() {
        eprintln!("phase remote_consumes_direct_first_mesh_routing");
        {
            assert_eq!(
                provider(true, vec![server(3)]).route("studio-node"),
                Ok(TiloRouteDecision::Direct)
            );
        }

        eprintln!("phase workspace_wide_relay_is_not_a_control_route");
        {
            assert!(
                provider(false, vec![server(3)])
                    .route("studio-node")
                    .is_err()
            );
        }

        eprintln!("phase stale_or_missing_discovery_never_becomes_success");
        {
            assert!(provider(false, Vec::new()).route("studio-node").is_err());
            assert!(provider(true, Vec::new()).route("different-node").is_err());
            assert!(UnavailableTiloRouteProvider.route("studio-node").is_err());
        }
    }
}
