use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use launcher::netgate::{EgressPolicy, NetGate, RELAY_PORT};
use launcher::sandbox::NetGateBind;

use crate::{
    AudioBinding, AudioProtocol, AudioProxyError, AudioProxyLease, LaunchLease,
    LaunchResourceBroker, NetworkShape, ResourceError, ResourceRequest,
};

const LEASE_ROOT: &str = "/run/tilo/windows-compat/leases";
const SANDBOX_GATE: &str = "/run/tilo/network/gate.sock";

type StartAudioProxy = fn(PathBuf, u32, bool) -> Result<AudioProxyLease, AudioProxyError>;

/// Concrete Tilo adapter for resources that already have a secure broker.
/// Host-filtered networking reuses the launcher's net-gate implementation.
/// Each capsule receives a private `PipeWire` graph, never the session audio
/// socket. The graph has a playback tunnel on every launch and materializes a
/// microphone source only when the canonical Tilo device grant says so.
pub struct SystemResourceBroker {
    next: AtomicU64,
    gates: Mutex<BTreeMap<String, NetGate>>,
    audio: Mutex<BTreeMap<String, AudioProxyLease>>,
    uid: u32,
    start_audio_proxy: StartAudioProxy,
}

impl SystemResourceBroker {
    #[must_use]
    pub fn new() -> Self {
        let uid = rustix::process::geteuid().as_raw();
        Self::for_uid(uid)
    }

    /// Construct for a compositor session uid. This is deliberately numeric
    /// rather than a path supplied by a caller; the only host endpoint the
    /// audio proxy names is `/run/user/<uid>/pulse/native`.
    #[must_use]
    pub fn for_uid(uid: u32) -> Self {
        Self {
            next: AtomicU64::new(1),
            gates: Mutex::new(BTreeMap::new()),
            audio: Mutex::new(BTreeMap::new()),
            uid,
            start_audio_proxy: AudioProxyLease::start,
        }
    }

    #[cfg(test)]
    fn for_uid_with_audio_proxy(uid: u32, start_audio_proxy: StartAudioProxy) -> Self {
        Self {
            next: AtomicU64::new(1),
            gates: Mutex::new(BTreeMap::new()),
            audio: Mutex::new(BTreeMap::new()),
            uid,
            start_audio_proxy,
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
        let microphone = request
            .capabilities
            .devices
            .iter()
            .any(|device| device == "microphone");
        let audio_proxy =
            (self.start_audio_proxy)(PathBuf::from(LEASE_ROOT).join(&id), self.uid, microphone)
                .map_err(|error| ResourceError::Refused(format!("audio proxy: {error}")))?;
        let audio = vec![
            AudioBinding {
                protocol: AudioProtocol::PipeWire,
                host_socket: audio_proxy.pipewire_socket(),
                input_enabled: microphone,
            },
            AudioBinding {
                protocol: AudioProtocol::Pulse,
                host_socket: audio_proxy.pulse_socket(),
                input_enabled: microphone,
            },
        ];
        let network = NetworkShape::from_capabilities(request.capabilities);
        let net_gate = if network == NetworkShape::Filtered {
            let host_socket = PathBuf::from(LEASE_ROOT).join(&id).join("net.sock");
            let gate = match NetGate::start(
                &host_socket,
                EgressPolicy::from_grants(
                    request.capabilities.net_hosts.iter().map(String::as_str),
                ),
            ) {
                Ok(gate) => gate,
                Err(error) => return Err(ResourceError::Refused(error.to_string())),
            };
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
        self.audio
            .lock()
            .map_err(|_| ResourceError::Unavailable)?
            .insert(id.clone(), audio_proxy);
        Ok(LaunchLease {
            id,
            audio,
            net_gate,
        })
    }

    fn revoke(&self, lease_id: &str) {
        if let Ok(mut gates) = self.gates.lock() {
            gates.remove(lease_id);
        }
        if let Ok(mut audio) = self.audio.lock() {
            // Dropping the lease kills both private broker processes before
            // the socket tree is removed, revoking existing client fds.
            audio.remove(lease_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use launcher::capabilities::CapabilityPlan;

    use super::*;

    #[test]
    fn audio_proxy_start_failure_is_fail_closed() {
        fn unavailable_proxy(
            root: PathBuf,
            _uid: u32,
            _microphone: bool,
        ) -> Result<AudioProxyLease, AudioProxyError> {
            Err(AudioProxyError::UnsafeLeasePath(root))
        }

        // Model an unavailable private PipeWire runtime explicitly. The
        // broker must refuse the launch instead of substituting a session
        // socket, independent of which audio packages the test host has.
        let broker = SystemResourceBroker::for_uid_with_audio_proxy(99_999, unavailable_proxy);
        let caps = CapabilityPlan::empty();
        let result = broker.materialize(ResourceRequest {
            app_id: "so.tilo.windows.test",
            workspace_id: "workspace",
            capabilities: &caps,
        });
        assert!(
            matches!(result, Err(ResourceError::Refused(message)) if message.starts_with("audio proxy:"))
        );
    }
}
