use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use launcher::netgate::{EgressPolicy, NetGate, RELAY_PORT};
use launcher::sandbox::NetGateBind;

use crate::{LaunchLease, LaunchResourceBroker, NetworkShape, ResourceError, ResourceRequest};

const LEASE_ROOT: &str = "/run/qos/windows-compat/leases";
const SANDBOX_GATE: &str = "/run/qos/network/gate.sock";

/// Concrete qOS adapter for resources that already have a secure broker.
/// Host-filtered networking reuses the launcher's net-gate implementation.
/// Audio deliberately remains absent: qOS currently has no playback-only
/// `PipeWire` proxy, and binding the session socket would also grant capture.
pub struct SystemResourceBroker {
    next: AtomicU64,
    gates: Mutex<BTreeMap<String, NetGate>>,
}

impl SystemResourceBroker {
    #[must_use]
    pub fn new() -> Self {
        Self {
            next: AtomicU64::new(1),
            gates: Mutex::new(BTreeMap::new()),
        }
    }

    fn lease_id(&self) -> String {
        let sequence = self.next.fetch_add(1, Ordering::Relaxed);
        format!("{}-{sequence}", std::process::id())
    }
}

impl Default for SystemResourceBroker {
    fn default() -> Self {
        Self::new()
    }
}

impl LaunchResourceBroker for SystemResourceBroker {
    fn materialize(&self, request: ResourceRequest<'_>) -> Result<LaunchLease, ResourceError> {
        let id = self.lease_id();
        let network = NetworkShape::from_capabilities(request.capabilities);
        let net_gate = if network == NetworkShape::Filtered {
            let host_socket = PathBuf::from(LEASE_ROOT).join(&id).join("net.sock");
            let gate = NetGate::start(
                &host_socket,
                EgressPolicy::new(request.capabilities.net_hosts.clone()),
            )
            .map_err(|error| ResourceError::Refused(error.to_string()))?;
            self.gates
                .lock()
                .map_err(|_| ResourceError::Unavailable)?
                .insert(id.clone(), gate);
            Some(NetGateBind {
                host_socket,
                sandbox_socket: PathBuf::from(SANDBOX_GATE),
                port: RELAY_PORT,
            })
        } else {
            None
        };
        Ok(LaunchLease {
            id,
            // No direct PipeWire/Pulse socket: without a filtering proxy it
            // would silently turn playback into microphone authority.
            audio: Vec::new(),
            net_gate,
        })
    }

    fn revoke(&self, lease_id: &str) {
        if let Ok(mut gates) = self.gates.lock() {
            gates.remove(lease_id);
        }
        let root = PathBuf::from(LEASE_ROOT).join(lease_id);
        let _ = std::fs::remove_dir(&root);
    }
}

#[cfg(test)]
mod tests {
    use launcher::capabilities::CapabilityPlan;

    use super::*;

    #[test]
    fn offline_lease_exposes_no_audio_or_network_socket() {
        let broker = SystemResourceBroker::new();
        let caps = CapabilityPlan::empty();
        let lease = broker
            .materialize(ResourceRequest {
                app_id: "dev.qos.windows.test",
                workspace_id: "workspace",
                capabilities: &caps,
            })
            .unwrap();
        assert!(lease.audio.is_empty());
        assert!(lease.net_gate.is_none());
        broker.revoke(&lease.id);
    }
}
