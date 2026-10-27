//! Device-mesh policy.
//!
//! This module is an orchestration layer over the existing Tilo
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
//! existing replication streams a device requests, which services a device
//! offers, and the advertisement a compute worker is admitted against. Hosts
//! are chosen by configuration (workspace bindings, home-peer registration);
//! the worker a compute job runs on is chosen among advertisements by
//! [`crate::placement`].

use std::collections::{BTreeMap, BTreeSet};

use iroh_base::EndpointAddr;
use serde::{Deserialize, Serialize};

/// Stable certified-device routing key.
pub type DeviceKey = [u8; 32];

/// Effective, certificate-bounded mesh policy of a running device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceMeshProfile {
    /// Certified device public key.
    pub device: DeviceKey,
    /// Interactive/server/hybrid role.
    pub role: DeviceRole,
    /// Service claims after intersection with the device certificate.
    pub services: ServiceCapabilities,
    /// Per-workspace residency requests consumed by collaboration and fs.
    pub replication: BTreeMap<u64, ReplicationIntent>,
    /// Direct endpoints for the authenticated filesystem/blob protocols. This
    /// is intentionally distinct from the relay endpoint: a client selected
    /// for content replication must dial one of these addresses.
    pub filesystem_endpoints: Vec<EndpointAddr>,
}

impl DeviceMeshProfile {
    /// Select the direct filesystem endpoint from a verified mesh profile.
    /// The relay endpoint is deliberately not a fallback: it speaks relay
    /// ALPN only and cannot serve authenticated blobs while an owner is away.
    pub fn content_endpoint(&self) -> Option<EndpointAddr> {
        self.services
            .full_content_replication
            .then(|| self.filesystem_endpoints.first().cloned())
            .flatten()
    }
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
///
/// Human-readable formats (the JSON of published mesh profiles, `relay.toml`)
/// carry it adjacently tagged, `{"kind": "app_records", "id": "…"}`. Serde's
/// adjacent tagging cannot be decoded by a format that is not
/// self-describing, so binary formats such as postcard carry the ordinary
/// variant-index form instead — the same bytes the tagged encoder already
/// produced there. See the `Serialize` / `Deserialize` impls below.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
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
    /// Ebook records and reading state; book bytes resolve through `FileBlobs`.
    EbookLibrary,
    /// Music/video library records; media bytes resolve through `FileBlobs`.
    MediaLibrary,
    /// An app-defined record family carried by the collaboration runtime.
    AppRecords(String),
}

/// The serialized shape of [`ReplicationResource`]; `S` is `&str` when
/// encoding and `String` when decoding. Both representations below expand
/// from this one definition so their variants cannot drift apart.
macro_rules! replication_resource_repr {
    ($(#[$attr:meta])* $name:ident) => {
        #[derive(Serialize, Deserialize)]
        $(#[$attr])*
        enum $name<S> {
            Workspace,
            FileTree,
            FileThumbnails,
            FileBlobs,
            EbookLibrary,
            MediaLibrary,
            AppRecords(S),
        }

        impl<'a> From<&'a ReplicationResource> for $name<&'a str> {
            fn from(resource: &'a ReplicationResource) -> Self {
                match resource {
                    ReplicationResource::Workspace => Self::Workspace,
                    ReplicationResource::FileTree => Self::FileTree,
                    ReplicationResource::FileThumbnails => Self::FileThumbnails,
                    ReplicationResource::FileBlobs => Self::FileBlobs,
                    ReplicationResource::EbookLibrary => Self::EbookLibrary,
                    ReplicationResource::MediaLibrary => Self::MediaLibrary,
                    ReplicationResource::AppRecords(id) => Self::AppRecords(id),
                }
            }
        }

        impl From<$name<String>> for ReplicationResource {
            fn from(repr: $name<String>) -> Self {
                match repr {
                    $name::Workspace => Self::Workspace,
                    $name::FileTree => Self::FileTree,
                    $name::FileThumbnails => Self::FileThumbnails,
                    $name::FileBlobs => Self::FileBlobs,
                    $name::EbookLibrary => Self::EbookLibrary,
                    $name::MediaLibrary => Self::MediaLibrary,
                    $name::AppRecords(id) => Self::AppRecords(id),
                }
            }
        }
    };
}

replication_resource_repr!(
    /// Adjacently tagged form for human-readable formats.
    #[serde(
        rename = "ReplicationResource",
        rename_all = "snake_case",
        tag = "kind",
        content = "id"
    )]
    TaggedReplicationResource
);
replication_resource_repr!(
    /// Variant-index form for binary formats.
    #[serde(rename = "ReplicationResource")]
    IndexedReplicationResource
);

impl Serialize for ReplicationResource {
    fn serialize<Ser: serde::Serializer>(&self, serializer: Ser) -> Result<Ser::Ok, Ser::Error> {
        if serializer.is_human_readable() {
            TaggedReplicationResource::from(self).serialize(serializer)
        } else {
            IndexedReplicationResource::from(self).serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for ReplicationResource {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        if deserializer.is_human_readable() {
            TaggedReplicationResource::<String>::deserialize(deserializer).map(Self::from)
        } else {
            IndexedReplicationResource::<String>::deserialize(deserializer).map(Self::from)
        }
    }
}

/// Replication residency requested by a device for one workspace.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplicationIntent {
    /// Keep the small workspace collaboration graph available.
    WorkspaceOnly,
    /// Hydrate selected data when opened, retaining the ordinary cache policy.
    #[default]
    OnDemand,
    /// Retain all user-visible workspace records and content bytes.
    FullWorkspace,
    /// Explicit app/resource selection. This still uses the same collaboration
    /// and blob transports; it is not an app-owned sync implementation.
    Resources(BTreeSet<ReplicationResource>),
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
    /// runtime, or a compute worker. Live workspace-control frames use this
    /// daemon's already-authenticated, non-retained presence fan-out rather
    /// than a second tunnel protocol.
    pub const fn relay_only() -> Self {
        Self {
            workspace_replication: true,
            full_content_replication: false,
            inbound_relay: true,
            remote_control_relay: true,
            app_serving: false,
            compute: false,
        }
    }

    /// Planes started by the composite home-peer process.
    pub const fn home_peer() -> Self {
        Self {
            workspace_replication: true,
            full_content_replication: true,
            inbound_relay: true,
            remote_control_relay: true,
            app_serving: false,
            compute: false,
        }
    }

    /// Services expected from a personal always-on server.
    ///
    /// This is a broader policy template for hosts that also run app,
    /// compute, and remote-control workers. The home-peer deployment uses
    /// [`Self::home_peer`].
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
    #[must_use]
    pub fn authorized_by(&self, certified: &tilo_user_identity::DeviceCapabilities) -> Self {
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
    /// Optional execution hardware available to sandboxed compute jobs.
    pub accelerators: BTreeSet<ComputeAccelerator>,
    /// Operator preference; higher is preferred before load balancing.
    pub priority: i32,
    /// Smoothed round-trip latency in milliseconds.
    pub latency_ms: u32,
    /// Locally observed energy state. This is a routing hint, never trusted
    /// authority: a lying peer can only attract work it was already certified
    /// and sandboxed to run.
    pub power: PowerAvailability,
    /// Whether the device has asked the mesh to avoid discretionary work.
    pub energy_saver: bool,
    /// Active assignments observed for this server.
    pub active_assignments: u32,
    /// Maximum concurrent assignments. Zero makes the server ineligible.
    pub assignment_capacity: u32,
    /// Whether liveness is fresh enough to route new work.
    pub online: bool,
}

/// Hardware class a compute worker can expose through the normal package
/// sandbox. Advertising hardware grants no additional identity authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputeAccelerator {
    /// A sandbox-approved render/compute node selected by the launcher.
    Gpu,
    /// Hardware virtualisation (`/dev/kvm`): the worker runs legacy VMs.
    Kvm,
}

/// Coarse power information used to keep continuity from unexpectedly draining
/// a portable device. Exact battery telemetry remains local to the observer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerAvailability {
    /// No reliable power observation is available.
    #[default]
    Unknown,
    /// Stable external power with no battery constraint.
    External,
    /// Connected to external power and charging at the given percentage.
    Charging(u8),
    /// Running from a battery at the given percentage.
    Battery(u8),
}

impl PowerAvailability {
    /// Whether the device is too low on battery to take discretionary work
    /// such as app backends or compute jobs. This is the single battery rule
    /// shared by every admission path.
    #[must_use]
    pub const fn is_critically_constrained(self) -> bool {
        matches!(self, Self::Battery(percent) if percent <= 15)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replication_intents_and_service_profiles_select_their_substrates() {
        use ReplicationResource::{
            EbookLibrary, FileBlobs, FileThumbnails, FileTree, MediaLibrary, Workspace,
        };
        let cases = [
            (
                "full workspace uses collaboration and blob substrates",
                ReplicationIntent::FullWorkspace,
                vec![
                    Workspace,
                    FileTree,
                    FileThumbnails,
                    FileBlobs,
                    EbookLibrary,
                    MediaLibrary,
                ],
                vec![],
            ),
            (
                "on demand syncs previews without file bytes",
                ReplicationIntent::OnDemand,
                vec![Workspace, FileTree, FileThumbnails],
                vec![FileBlobs, EbookLibrary, MediaLibrary],
            ),
            (
                "selecting media also hydrates its file bytes",
                ReplicationIntent::Resources(BTreeSet::from([MediaLibrary])),
                vec![Workspace, FileTree, FileThumbnails, FileBlobs],
                vec![],
            ),
        ];
        for (name, intent, present, absent) in cases {
            let resources = intent.resources();
            for r in present {
                assert!(resources.contains(&r), "case {name}: {r:?} missing");
            }
            for r in absent {
                assert!(!resources.contains(&r), "case {name}: {r:?} present");
            }
        }

        // A relay-only device claims only its running workspace planes.
        let services = ServiceCapabilities::relay_only();
        assert!(services.workspace_replication);
        assert!(services.inbound_relay);
        assert!(!services.full_content_replication);
        assert!(services.remote_control_relay);
        assert!(!services.app_serving);
        assert!(!services.compute);
        assert_eq!(
            ServiceCapabilities::personal_server().limited_to(&services),
            services
        );

        let advertised = ServiceCapabilities::personal_server();
        assert_eq!(
            advertised.authorized_by(&tilo_user_identity::DeviceCapabilities::member()),
            ServiceCapabilities::default()
        );
        let mut host_without_global_read = tilo_user_identity::DeviceCapabilities::host();
        host_without_global_read.can_read_all_workspaces = false;
        let authorized = advertised.authorized_by(&host_without_global_read);
        assert!(authorized.workspace_replication);
        assert!(!authorized.full_content_replication);
    }

    #[test]
    fn only_a_battery_at_or_below_fifteen_percent_is_critically_constrained() {
        use PowerAvailability::{Battery, Charging, External, Unknown};
        for (power, constrained) in [
            (Battery(0), true),
            (Battery(15), true),
            (Battery(16), false),
            (Charging(5), false),
            (External, false),
            (Unknown, false),
        ] {
            assert_eq!(power.is_critically_constrained(), constrained, "{power:?}");
        }
    }
}
