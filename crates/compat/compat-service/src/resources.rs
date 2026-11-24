use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use launcher::egress_proxy::{EgressPolicy, RELAY_PORT};
use launcher::sandbox::EgressProxyBind;

use launcher::audio_graph::{AudioGraph, AudioGraphError};

use crate::{
    AudioBinding, AudioProtocol, LaunchLease, LaunchResourceBroker, NetworkShape, ResourceError,
    ResourceRequest,
};

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
            audio: Mutex::new(BTreeMap::new()),
            uid,
            prepare_audio_graph: AudioGraph::prepare,
        }
    }

    #[cfg(test)]
    fn for_uid_with_audio_graph(uid: u32, prepare_audio_graph: PrepareAudioGraph) -> Self {
        Self {
            next: AtomicU64::new(1),
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
        own_lease_root(self.uid)?;
        loop {
            let id = self.lease_id();
            match (self.prepare_audio_graph)(
                crate::plan::lease_root().join(&id),
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
        let Ok(leases) = std::fs::read_dir(crate::plan::lease_root()) else {
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

/// This person's lease directory, made private on first use. Anyone who launches may write the
/// computer's root, which has no sticky bit: a directory found there counts only when it is this
/// person's and nobody else can write into it.
fn own_lease_root(uid: u32) -> Result<PathBuf, AudioGraphError> {
    own_lease_root_at(crate::plan::lease_root(), uid)
}

fn own_lease_root_at(root: PathBuf, uid: u32) -> Result<PathBuf, AudioGraphError> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};

    let unsafe_root = || AudioGraphError::UnsafePath(root.clone());
    match std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&root)
    {
        Ok(()) => {}
        Err(_) => return Err(unsafe_root()),
    }
    let metadata = std::fs::symlink_metadata(&root).map_err(|_| unsafe_root())?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o022 != 0 {
        return Err(unsafe_root());
    }
    Ok(root)
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
            // The launch starts the proxy that enforces this policy as the sandbox's child, so
            // the program keeps its network through an in-place restart and the proxy ends with
            // it. The session updates the policy through its control socket, which a kept
            // program's desktop finds again in its keeper's directory.
            let dir = crate::plan::lease_root().join(&id).join("netproxy");
            if let Err(error) = launcher::netproxy::prepare(
                &dir,
                &EgressPolicy::from_grants(
                    request.capabilities.net_hosts.iter().map(String::as_str),
                ),
            ) {
                graph.remove();
                return Err(ResourceError::Refused(error.to_string()));
            }
            Some(EgressProxyBind {
                host_socket: dir.join("proxy.sock"),
                sandbox_socket: PathBuf::from(SANDBOX_PROXY),
                port: RELAY_PORT,
                udp_client: None,
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

    fn release_ended(&self) {
        self.remove_ended();
    }

    fn revoke(&self, lease_id: &str) {
        // The network proxy ends with the program it serves, so the program's end revokes it.
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

    /// The leases' directory is the person's own: made private, and refused when someone else
    /// could write into it.
    #[test]
    fn a_persons_leases_live_in_a_directory_only_they_can_write() {
        use std::os::unix::fs::PermissionsExt as _;

        let home = tempfile::tempdir().unwrap();
        let uid = rustix::process::geteuid().as_raw();
        let root = home.path().join("leases").join(uid.to_string());
        assert_eq!(own_lease_root_at(root.clone(), uid).unwrap(), root);
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o770)).unwrap();
        assert!(matches!(
            own_lease_root_at(root.clone(), uid),
            Err(AudioGraphError::UnsafePath(_))
        ));
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(
            own_lease_root_at(root, uid.wrapping_add(1)),
            Err(AudioGraphError::UnsafePath(_))
        ));
    }

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
