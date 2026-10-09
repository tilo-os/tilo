//! Tilo-device session route adapter.
//!
//! Discovery remains owned by the device-mesh runtime. Remote accepts a
//! verified snapshot and decides from it: a direct path is the only route
//! accepted for control. A workspace-wide relay is reported as unavailable
//! rather than leaking input through fan-out, since no exact-target relayed
//! route exists yet.

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
    /// Verified, online relays of this workspace that offer the
    /// remote-control relay service.
    pub relays: Vec<DeviceKey>,
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
        let MeshRouteSnapshot {
            client,
            target,
            direct_reachable,
            relays,
            ..
        } = &self.snapshot;
        if client == target {
            return Err(
                "No direct path or eligible online server is available for this workspace"
                    .to_string(),
            );
        }
        if *direct_reachable {
            return Ok(TiloRouteDecision::Direct);
        }
        // A relay is never one end of the session it would carry.
        if relays
            .iter()
            .any(|relay| relay != client && relay != target)
        {
            return Err(
                "Workspace control needs an exact-target route; the available relay is workspace-wide"
                    .to_string(),
            );
        }
        Err("No direct path or eligible online server is available for this workspace".to_string())
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
    use super::*;

    fn server(id: u8) -> DeviceKey {
        [id; 32]
    }

    fn provider(direct: bool, relays: Vec<DeviceKey>) -> MeshTiloRouteProvider {
        MeshTiloRouteProvider::new(MeshRouteSnapshot {
            target_hint: "studio-node".to_string(),
            client: [1; 32],
            target: [2; 32],
            workspace_id: 7,
            direct_reachable: direct,
            relays,
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
