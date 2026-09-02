//! Device-mesh policy and placement.
//!
//! This module is deliberately an orchestration layer over the existing qOS
//! transports:
//!
//! - workspace/app records continue to replicate as signed collaboration ops;
//! - file, ebook and media bytes continue to hydrate through the filesystem's
//!   content-addressed blob transport;
//! - interactive control continues to use the shell's consented remote-input
//!   path; and
//! - hosts continue to authenticate as ordinary certified devices.
//!
//! It therefore does not define another sync protocol.  It describes which
//! existing replication streams a device requests and deterministically picks
//! an eligible always-on peer for relay, application or future compute work.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// Stable certified-device routing key.
pub type DeviceKey = [u8; 32];

/// Effective, certificate-bounded mesh policy of a running device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceMeshProfile {
    /// Certified device public key.
    pub device: DeviceKey,
    /// Interactive/server/hybrid role.
    pub role: DeviceRole,
    /// Service claims after intersection with the device certificate.
    pub services: ServiceCapabilities,
    /// Per-workspace residency requests consumed by collaboration and fs.
    pub replication: BTreeMap<u64, ReplicationIntent>,
}

/// How a device participates in the personal mesh.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceRole {
    /// A normal client: it renders workspaces and may be remotely controlled
    /// while the local user has granted a control session.
    #[default]
    Interactive,
    /// An unattended peer expected to remain reachable and retain requested
    /// replicas. It is still a certified device, not a privileged identity.
    AlwaysOnServer,
    /// A desktop that is both actively used and available as a host while on.
    Hybrid,
}

impl DeviceRole {
    /// Whether this role may advertise durable serving capacity.
    pub const fn can_serve(self) -> bool {
        matches!(self, Self::AlwaysOnServer | Self::Hybrid)
    }
}

/// Existing data plane used for a replicated resource.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "id")]
pub enum ReplicationResource {
    /// Workspace topology, windows, strips and shared app records.
    Workspace,
    /// Filesystem names, metadata, versions and ACL records.
    FileTree,
    /// Bounded previews used by Files and open-workspace cards. This is
    /// deliberately separate from [`Self::FileBlobs`]: an ordinary device may
    /// show a useful workspace before the user opens a file without silently
    /// downloading the file itself.
    FileThumbnails,
    /// File bytes addressed by their existing content hash.
    FileBlobs,
    /// Ebook records and reading state; book bytes resolve through FileBlobs.
    EbookLibrary,
    /// Music/video library records; media bytes resolve through FileBlobs.
    MediaLibrary,
    /// An app-defined record family carried by the collaboration runtime.
    AppRecords(String),
}

/// Replication residency requested by a device for one workspace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplicationIntent {
    /// Keep the small workspace collaboration graph available.
    WorkspaceOnly,
    /// Hydrate selected data when opened, retaining the ordinary cache policy.
    OnDemand,
    /// Retain all user-visible workspace records and content bytes.
    FullWorkspace,
    /// Explicit app/resource selection. This still uses the same collaboration
    /// and blob transports; it is not an app-owned sync implementation.
    Resources(BTreeSet<ReplicationResource>),
}

impl Default for ReplicationIntent {
    fn default() -> Self {
        Self::OnDemand
    }
}

impl ReplicationIntent {
    /// Resolve an intent to the concrete existing replication streams that
    /// consumers should subscribe to.
    pub fn resources(&self) -> BTreeSet<ReplicationResource> {
        match self {
            Self::WorkspaceOnly => BTreeSet::from([ReplicationResource::Workspace]),
            Self::OnDemand => BTreeSet::from([
                ReplicationResource::Workspace,
                ReplicationResource::FileTree,
                ReplicationResource::FileThumbnails,
            ]),
            Self::FullWorkspace => BTreeSet::from([
                ReplicationResource::Workspace,
                ReplicationResource::FileTree,
                ReplicationResource::FileThumbnails,
                ReplicationResource::FileBlobs,
                ReplicationResource::EbookLibrary,
                ReplicationResource::MediaLibrary,
            ]),
            Self::Resources(resources) => {
                let mut resolved = resources.clone();
                resolved.insert(ReplicationResource::Workspace);
                if resolved.contains(&ReplicationResource::EbookLibrary)
                    || resolved.contains(&ReplicationResource::MediaLibrary)
                {
                    resolved.insert(ReplicationResource::FileTree);
                    resolved.insert(ReplicationResource::FileThumbnails);
                    resolved.insert(ReplicationResource::FileBlobs);
                }
                resolved
            }
        }
    }
}

/// Services an always-on device is willing and certified to provide.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent service capabilities are intersected with signed certificate authority"
)]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServiceCapabilities {
    /// Pin collaboration state and provide anti-entropy/store-and-forward.
    pub workspace_replication: bool,
    /// Retain all file blobs requested by the workspace replication policy.
    pub full_content_replication: bool,
    /// Relay end-to-end encrypted collaboration and chat traffic.
    pub inbound_relay: bool,
    /// Relay an end-to-end remote-control session when direct reachability
    /// fails. Consent and input authorization remain enforced by the target.
    pub remote_control_relay: bool,
    /// Run app backends for eligible clients.
    pub app_serving: bool,
    /// Accept distributed jobs. This is a placement seam only; it grants no
    /// ability to execute code outside the app sandbox.
    pub compute: bool,
}

impl ServiceCapabilities {
    /// Capabilities implemented by the standalone relay daemon.
    ///
    /// The relay durably carries authenticated workspace CRDT updates and
    /// presence. It does not run the filesystem/blob host plane, an app
    /// runtime, a compute worker, or the remote-control tunnel endpoint, so
    /// none of those capabilities may be advertised merely because this is
    /// an always-on machine.
    pub const fn relay_only() -> Self {
        Self {
            workspace_replication: true,
            full_content_replication: false,
            inbound_relay: true,
            remote_control_relay: false,
            app_serving: false,
            compute: false,
        }
    }

    /// Services expected from a personal always-on server.
    ///
    /// This is a policy template for a future composite host which also
    /// starts the filesystem/blob host and app workers. The standalone
    /// `qos-relay` binary deliberately uses [`Self::relay_only`] instead.
    pub const fn personal_server() -> Self {
        Self {
            workspace_replication: true,
            full_content_replication: true,
            inbound_relay: true,
            remote_control_relay: true,
            app_serving: true,
            compute: true,
        }
    }

    /// Intersect a mutable service advertisement with the immutable authority
    /// in a verified device certificate. A peer cannot turn itself into a host
    /// by merely claiming services in discovery metadata.
    pub fn authorized_by(&self, certified: &qos_user_identity::DeviceCapabilities) -> Self {
        if !certified.can_act_as_host {
            return Self::default();
        }
        Self {
            workspace_replication: self.workspace_replication,
            full_content_replication: self.full_content_replication
                && certified.can_read_all_workspaces,
            inbound_relay: self.inbound_relay,
            remote_control_relay: self.remote_control_relay,
            app_serving: self.app_serving,
            compute: self.compute,
        }
    }

    /// Intersect two service sets. Used by a process to ensure its wire
    /// advertisement cannot exceed the planes it has actually started.
    #[must_use]
    pub const fn limited_to(&self, available: &Self) -> Self {
        Self {
            workspace_replication: self.workspace_replication && available.workspace_replication,
            full_content_replication: self.full_content_replication
                && available.full_content_replication,
            inbound_relay: self.inbound_relay && available.inbound_relay,
            remote_control_relay: self.remote_control_relay && available.remote_control_relay,
            app_serving: self.app_serving && available.app_serving,
            compute: self.compute && available.compute,
        }
    }
}

/// Locally observed state for a certified device advertisement.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerAdvertisement {
    /// Certified device public key.
    pub device: DeviceKey,
    /// Human-oriented device role.
    pub role: DeviceRole,
    /// Services this peer advertises. Callers must intersect these with the
    /// immutable capabilities from its verified device certificate.
    pub services: ServiceCapabilities,
    /// Workspaces this server currently hosts.
    pub workspaces: BTreeSet<u64>,
    /// Apps whose backend can run here. Empty means none, not "all".
    pub apps: BTreeSet<String>,
    /// Operator preference; higher is preferred before load balancing.
    pub priority: i32,
    /// Smoothed round-trip latency in milliseconds.
    pub latency_ms: u32,
    /// Active assignments observed for this server.
    pub active_assignments: u32,
    /// Maximum concurrent assignments. Zero makes the server ineligible.
    pub assignment_capacity: u32,
    /// Whether liveness is fresh enough to route new work.
    pub online: bool,
}

/// Work that needs an always-on serving peer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Workload {
    /// CRDT anti-entropy and durable store-and-forward.
    WorkspaceReplication,
    /// Full file/content residency for a workspace.
    ContentReplication,
    /// Inbound collaboration or chat relay.
    InboundRelay,
    /// Relay an end-to-end remote-control session to `target`.
    RemoteControl {
        /// Device whose compositor owns consent and input authorization.
        target: DeviceKey,
    },
    /// Serve an app backend.
    App {
        /// Stable package/application identifier.
        app_id: String,
    },
    /// Future sandboxed distributed job for an app.
    Compute {
        /// App security domain that owns the sandboxed job.
        app_id: String,
    },
}

/// Stable input to the placement function.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlacementRequest {
    /// Client receiving the service.
    pub client: DeviceKey,
    /// Workspace that scopes data and authorization.
    pub workspace_id: u64,
    /// Requested service.
    pub workload: Workload,
    /// Number of distinct servers desired. One is normal; replication may ask
    /// for two or more.
    pub replicas: usize,
}

/// Why a server was selected, exposed for status UI and diagnostics.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Placement {
    /// Selected devices, primary first.
    pub servers: Vec<DeviceKey>,
}

/// Reachability choice for an existing end-to-end remote-control session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteControlRoute {
    /// Client can reach the target without an intermediate qOS server.
    Direct {
        /// Target device.
        target: DeviceKey,
    },
    /// The encrypted session is tunnelled through an eligible server. The
    /// target remains the endpoint that grants consent and reviews input.
    Relayed {
        /// Target device.
        target: DeviceKey,
        /// Intermediate always-on peer.
        via: DeviceKey,
    },
    /// Neither a direct path nor an eligible live server is available.
    Unavailable,
}

/// Prefer direct remote control, falling back to the same mesh placement used
/// for other server workloads.
pub fn route_remote_control(
    client: DeviceKey,
    target: DeviceKey,
    workspace_id: u64,
    direct_reachable: bool,
    candidates: &[ServerAdvertisement],
) -> RemoteControlRoute {
    if client == target {
        return RemoteControlRoute::Unavailable;
    }
    if direct_reachable {
        return RemoteControlRoute::Direct { target };
    }
    let placement = place(
        &PlacementRequest {
            client,
            workspace_id,
            workload: Workload::RemoteControl { target },
            replicas: 1,
        },
        candidates,
    );
    placement
        .servers
        .first()
        .map_or(RemoteControlRoute::Unavailable, |via| {
            RemoteControlRoute::Relayed { target, via: *via }
        })
}

/// Pick eligible servers using deterministic weighted rendezvous hashing.
///
/// Rendezvous hashing keeps assignments stable as servers come and go and
/// naturally distributes different clients/apps without a central scheduler.
/// Operator priority, available capacity and latency provide bounded nudges;
/// identity/workspace/service eligibility is a hard filter.
pub fn place(request: &PlacementRequest, candidates: &[ServerAdvertisement]) -> Placement {
    let mut ranked: Vec<(u128, DeviceKey)> = candidates
        .iter()
        .filter(|server| eligible(request, server))
        .map(|server| (score(request, server), server.device))
        .collect();
    ranked.sort_unstable_by(|left, right| right.cmp(left));
    ranked.dedup_by_key(|(_, device)| *device);
    Placement {
        servers: ranked
            .into_iter()
            .take(request.replicas)
            .map(|(_, device)| device)
            .collect(),
    }
}

fn eligible(request: &PlacementRequest, server: &ServerAdvertisement) -> bool {
    if !server.online
        || !server.role.can_serve()
        || server.assignment_capacity == 0
        || server.active_assignments >= server.assignment_capacity
        || !server.workspaces.contains(&request.workspace_id)
        || server.device == request.client
    {
        return false;
    }
    match &request.workload {
        Workload::WorkspaceReplication => server.services.workspace_replication,
        Workload::ContentReplication => server.services.full_content_replication,
        Workload::InboundRelay => server.services.inbound_relay,
        Workload::RemoteControl { target } => {
            server.device != *target && server.services.remote_control_relay
        }
        Workload::App { app_id } => server.services.app_serving && server.apps.contains(app_id),
        Workload::Compute { app_id } => server.services.compute && server.apps.contains(app_id),
    }
}

fn score(request: &PlacementRequest, server: &ServerAdvertisement) -> u128 {
    let mut bytes = Vec::with_capacity(96);
    bytes.extend_from_slice(b"qos-mesh-placement-v1\0");
    bytes.extend_from_slice(&request.client);
    bytes.extend_from_slice(&request.workspace_id.to_le_bytes());
    bytes.extend_from_slice(&server.device);
    match &request.workload {
        Workload::WorkspaceReplication => bytes.extend_from_slice(b"workspace"),
        Workload::ContentReplication => bytes.extend_from_slice(b"content"),
        Workload::InboundRelay => bytes.extend_from_slice(b"relay"),
        Workload::RemoteControl { target } => {
            bytes.extend_from_slice(b"remote");
            bytes.extend_from_slice(target);
        }
        Workload::App { app_id } => {
            bytes.extend_from_slice(b"app");
            bytes.extend_from_slice(app_id.as_bytes());
        }
        Workload::Compute { app_id } => {
            bytes.extend_from_slice(b"compute");
            bytes.extend_from_slice(app_id.as_bytes());
        }
    }
    let digest = blake3::hash(&bytes);
    let mut first = [0u8; 8];
    first.copy_from_slice(&digest.as_bytes()[..8]);
    let affinity = u128::from(u64::from_le_bytes(first));

    let free = server
        .assignment_capacity
        .saturating_sub(server.active_assignments);
    let capacity_ratio = u128::from(free)
        .saturating_mul(10_000)
        .checked_div(u128::from(server.assignment_capacity))
        .unwrap_or_default();
    let capacity_weight = capacity_ratio.saturating_mul(u128::from(u64::MAX) / 40_000);
    let latency = u128::from(60_000u32.saturating_sub(server.latency_ms.min(60_000)));
    let latency_weight = latency.saturating_mul(u128::from(u64::MAX) / 480_000);
    let priority = i64::from(server.priority.clamp(-1_000, 1_000)).saturating_add(1_000);
    let priority_weight = u128::try_from(priority)
        .unwrap_or_default()
        .saturating_mul(u128::from(u64::MAX) / 32_000);

    // Rendezvous affinity remains the largest term, while health can move a
    // materially saturated/high-latency server behind a healthy alternative.
    affinity
        .saturating_add(capacity_weight)
        .saturating_add(latency_weight)
        .saturating_add(priority_weight)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(id: u8) -> ServerAdvertisement {
        ServerAdvertisement {
            device: [id; 32],
            role: DeviceRole::AlwaysOnServer,
            services: ServiceCapabilities::personal_server(),
            workspaces: BTreeSet::from([7]),
            apps: BTreeSet::from(["dev.qos.word".to_string()]),
            priority: 0,
            latency_ms: 10,
            active_assignments: 0,
            assignment_capacity: 100,
            online: true,
        }
    }

    #[test]
    fn full_workspace_uses_collaboration_and_blob_substrates() {
        let resources = ReplicationIntent::FullWorkspace.resources();
        assert!(resources.contains(&ReplicationResource::Workspace));
        assert!(resources.contains(&ReplicationResource::FileTree));
        assert!(resources.contains(&ReplicationResource::FileThumbnails));
        assert!(resources.contains(&ReplicationResource::FileBlobs));
        assert!(resources.contains(&ReplicationResource::EbookLibrary));
        assert!(resources.contains(&ReplicationResource::MediaLibrary));
    }

    #[test]
    fn on_demand_syncs_previews_without_file_bytes() {
        let resources = ReplicationIntent::OnDemand.resources();
        assert!(resources.contains(&ReplicationResource::Workspace));
        assert!(resources.contains(&ReplicationResource::FileTree));
        assert!(resources.contains(&ReplicationResource::FileThumbnails));
        assert!(!resources.contains(&ReplicationResource::FileBlobs));
        assert!(!resources.contains(&ReplicationResource::EbookLibrary));
        assert!(!resources.contains(&ReplicationResource::MediaLibrary));
    }

    #[test]
    fn relay_only_never_claims_unstarted_host_planes() {
        let services = ServiceCapabilities::relay_only();
        assert!(services.workspace_replication);
        assert!(services.inbound_relay);
        assert!(!services.full_content_replication);
        assert!(!services.remote_control_relay);
        assert!(!services.app_serving);
        assert!(!services.compute);

        assert_eq!(
            ServiceCapabilities::personal_server().limited_to(&services),
            services
        );
    }

    #[test]
    fn selecting_media_also_hydrates_its_file_bytes() {
        let intent =
            ReplicationIntent::Resources(BTreeSet::from([ReplicationResource::MediaLibrary]));
        let resources = intent.resources();
        assert!(resources.contains(&ReplicationResource::Workspace));
        assert!(resources.contains(&ReplicationResource::FileTree));
        assert!(resources.contains(&ReplicationResource::FileThumbnails));
        assert!(resources.contains(&ReplicationResource::FileBlobs));
    }

    #[test]
    fn placement_is_stable_and_returns_distinct_replicas() {
        let request = PlacementRequest {
            client: [99; 32],
            workspace_id: 7,
            workload: Workload::WorkspaceReplication,
            replicas: 2,
        };
        let candidates = vec![server(1), server(2), server(3)];
        let first = place(&request, &candidates);
        let second = place(&request, &candidates);
        assert_eq!(first, second);
        assert_eq!(first.servers.len(), 2);
        assert_ne!(first.servers[0], first.servers[1]);
    }

    #[test]
    fn ineligible_servers_never_receive_work() {
        let request = PlacementRequest {
            client: [99; 32],
            workspace_id: 7,
            workload: Workload::ContentReplication,
            replicas: 4,
        };
        let mut offline = server(1);
        offline.online = false;
        let mut interactive = server(2);
        interactive.role = DeviceRole::Interactive;
        let mut no_content = server(3);
        no_content.services.full_content_replication = false;
        let eligible = server(4);
        assert_eq!(
            place(&request, &[offline, interactive, no_content, eligible]).servers,
            vec![[4; 32]]
        );
    }

    #[test]
    fn app_and_compute_placement_are_capability_and_app_scoped() {
        let mut wrong_app = server(1);
        wrong_app.apps = BTreeSet::from(["dev.qos.sheet".to_string()]);
        let right = server(2);
        let request = PlacementRequest {
            client: [99; 32],
            workspace_id: 7,
            workload: Workload::App {
                app_id: "dev.qos.word".to_string(),
            },
            replicas: 1,
        };
        assert_eq!(place(&request, &[wrong_app, right]).servers, vec![[2; 32]]);
    }

    #[test]
    fn remote_control_relay_cannot_be_client_or_target() {
        let request = PlacementRequest {
            client: [1; 32],
            workspace_id: 7,
            workload: Workload::RemoteControl { target: [2; 32] },
            replicas: 1,
        };
        assert!(place(&request, &[server(1), server(2)]).servers.is_empty());
        assert_eq!(place(&request, &[server(3)]).servers, vec![[3; 32]]);
    }

    #[test]
    fn remote_control_prefers_direct_then_falls_back_to_mesh() {
        assert_eq!(
            route_remote_control([1; 32], [2; 32], 7, true, &[server(3)]),
            RemoteControlRoute::Direct { target: [2; 32] }
        );
        assert_eq!(
            route_remote_control([1; 32], [2; 32], 7, false, &[server(3)]),
            RemoteControlRoute::Relayed {
                target: [2; 32],
                via: [3; 32]
            }
        );
    }

    #[test]
    fn discovery_claims_are_intersected_with_signed_device_authority() {
        let advertised = ServiceCapabilities::personal_server();
        assert_eq!(
            advertised.authorized_by(&qos_user_identity::DeviceCapabilities::member()),
            ServiceCapabilities::default()
        );
        let mut host_without_global_read = qos_user_identity::DeviceCapabilities::host();
        host_without_global_read.can_read_all_workspaces = false;
        let authorized = advertised.authorized_by(&host_without_global_read);
        assert!(authorized.workspace_replication);
        assert!(!authorized.full_content_replication);
    }
}
