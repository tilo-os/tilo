use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use launcher::egress_proxy::{EgressPolicy, EgressProxy, RELAY_PORT};
use launcher::sandbox::EgressProxyBind;

use launcher::audio_graph::{AudioGraph, AudioGraphError};

use crate::{
    AudioBinding, AudioProtocol, LaunchLease, LaunchResourceBroker, NetworkShape, ResourceError,
    ResourceRequest,
};

const LEASE_ROOT: &str = "/run/tilo/windows-compat/leases";
const SANDBOX_PROXY: &str = "/run/tilo/network/proxy.sock";

pub(crate) type PrepareAudioGraph = fn(PathBuf, u32, bool) -> Result<AudioGraph, AudioGraphError>;

/// Concrete Tilo adapter for resources that already have a secure broker.
/// Host-filtered networking reuses the launcher's net-proxy implementation.
/// Each capsule receives a private `PipeWire` graph, never the session audio
/// socket. The graph has a playback tunnel on every launch and materializes a
/// microphone source only when the canonical Tilo device grant says so. The
/// broker prepares the graph; the launch starts it right before its sandbox,
/// so the graph lives exactly as long as the program
/// ([`launcher::audio_graph`]).
pub struct SystemResourceBroker {
    next: AtomicU64,
    proxies: Mutex<BTreeMap<String, EgressProxy>>,
    audio: Mutex<BTreeMap<String, AudioGraph>>,
    uid: u32,
    prepare_audio_graph: PrepareAudioGraph,
}

impl SystemResourceBroker {
    #[must_use]
    pub fn new() -> Self {
        let uid = rustix::process::geteuid().as_raw();
        Self::for_uid(uid)
    }

    /// Construct for a session uid. This is deliberately numeric
    /// rather than a path supplied by a caller; the only host endpoint the
    /// audio proxy names is `/run/user/<uid>/pulse/native`.
    #[must_use]
    pub fn for_uid(uid: u32) -> Self {
        Self {
            next: AtomicU64::new(1),
            proxies: Mutex::new(BTreeMap::new()),
            audio: Mutex::new(BTreeMap::new()),
            uid,
            prepare_audio_graph: AudioGraph::prepare,
        }
    }

    #[cfg(test)]
    fn for_uid_with_audio_graph(uid: u32, prepare_audio_graph: PrepareAudioGraph) -> Self {
        Self {
            next: AtomicU64::new(1),
            proxies: Mutex::new(BTreeMap::new()),
            audio: Mutex::new(BTreeMap::new()),
            uid,
            prepare_audio_graph,
        }
    }

    fn lease_id(&self) -> String {
        let sequence = self.next.fetch_add(1, Ordering::Relaxed);
        format!("{}-{sequence}", std::process::id())
    }

    /// A lease and its audio graph, prepared under a name no other lease has: a program kept
    /// open while the desktop restarted holds the lease a desktop before this one named.
    fn prepare(&self, microphone: bool) -> Result<(String, AudioGraph), AudioGraphError> {
        loop {
            let id = self.lease_id();
            match (self.prepare_audio_graph)(
                PathBuf::from(LEASE_ROOT).join(&id),
                self.uid,
                microphone,
            ) {
                Ok(graph) => return Ok((id, graph)),
                Err(AudioGraphError::Taken(_)) => {}
                Err(error) => return Err(error),
            }
        }
    }

    /// Remove the leases of this session's programs that ended while another desktop served
    /// them: their graph's daemons ended with them, and nobody else gives them back.
    fn remove_ended(&self) {
        let Ok(held) = self.audio.lock() else {
            return;
        };
        let Ok(leases) = std::fs::read_dir(LEASE_ROOT) else {
            return;
        };
        for lease in leases.flatten() {
            let ours = std::os::unix::fs::MetadataExt::uid(&match lease.metadata() {
                Ok(metadata) => metadata,
                Err(_) => continue,
            }) == self.uid;
            let name = lease.file_name();
            if !ours || held.contains_key(&*name.to_string_lossy()) {
                continue;
            }
            let graph = AudioGraph::at(lease.path());
            if graph.ended() {
                graph.remove();
            }
        }
    }
}

impl Default for SystemResourceBroker {
    fn default() -> Self {
        Self::new()
    }
}

impl LaunchResourceBroker for SystemResourceBroker {
    fn materialize(&self, request: ResourceRequest<'_>) -> Result<LaunchLease, ResourceError> {
        self.remove_ended();
        let microphone = request
            .capabilities
            .devices
            .contains(&collab_types::caps::DeviceCap::Microphone);
        let (id, graph) = self
            .prepare(microphone)
            .map_err(|error| ResourceError::Refused(format!("audio proxy: {error}")))?;
        let audio = vec![
            AudioBinding {
                protocol: AudioProtocol::PipeWire,
                host_socket: graph.pipewire_socket(),
                input_enabled: microphone,
            },
            AudioBinding {
                protocol: AudioProtocol::Pulse,
                host_socket: graph.pulse_socket(),
                input_enabled: microphone,
            },
        ];
        let audio_graph = Some(graph.root().to_path_buf());
        let network = NetworkShape::from_capabilities(request.capabilities);
        let egress_proxy = if network == NetworkShape::Filtered {
            let host_socket = PathBuf::from(LEASE_ROOT).join(&id).join("net.sock");
            let proxy = match EgressProxy::start(
                &host_socket,
                EgressPolicy::from_grants(
                    request.capabilities.net_hosts.iter().map(String::as_str),
                ),
            ) {
                Ok(proxy) => proxy,
                Err(error) => {
                    graph.remove();
                    return Err(ResourceError::Refused(error.to_string()));
                }
            };
            self.proxies
                .lock()
                .map_err(|_| ResourceError::Unavailable)?
                .insert(id.clone(), proxy);
            Some(EgressProxyBind {
                host_socket,
                sandbox_socket: PathBuf::from(SANDBOX_PROXY),
                port: RELAY_PORT,
            })
        } else {
            None
        };
        self.audio
            .lock()
            .map_err(|_| ResourceError::Unavailable)?
            .insert(id.clone(), graph);
        Ok(LaunchLease {
            id,
            audio,
            audio_graph,
            egress_proxy,
        })
    }

    fn revoke(&self, lease_id: &str) {
        if let Ok(mut proxies) = self.proxies.lock() {
            proxies.remove(lease_id);
        }
        if let Some(graph) = self
            .audio
            .lock()
            .ok()
            .and_then(|mut audio| audio.remove(lease_id))
        {
            graph.remove();
        }
    }
}

#[cfg(test)]
mod tests {
    use launcher::capabilities::CapabilityPlan;

    use super::*;

    #[test]
    fn audio_graph_failure_is_fail_closed() {
        fn unavailable_proxy(
            root: PathBuf,
            _uid: u32,
            _microphone: bool,
        ) -> Result<AudioGraph, AudioGraphError> {
            Err(AudioGraphError::UnsafePath(root))
        }

        // Model an unavailable private PipeWire runtime explicitly. The
        // broker must refuse the launch instead of substituting a session
        // socket, independent of which audio packages the test host has.
        let broker = SystemResourceBroker::for_uid_with_audio_graph(99_999, unavailable_proxy);
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
