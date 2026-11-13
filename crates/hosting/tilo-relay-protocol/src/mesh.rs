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
//! It therefore does not define another sync protocol. It describes which
//! services a device offers, and the advertisement a compute worker is
//! admitted against. Which device serves a workspace is the workspace's
//! governance and the device's own [`crate::WorkSetting`]; the worker a
//! compute job runs on is chosen among advertisements by
//! [`crate::placement`].

use std::collections::BTreeSet;

use crate::WorkspaceId;
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
    /// Service claims after intersection with the device certificate.
    pub services: ServiceCapabilities,
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
    /// Services this peer advertises. Callers must intersect these with the
    /// immutable capabilities from its verified device certificate.
    pub services: ServiceCapabilities,
    /// Workspaces this server currently hosts.
    pub workspaces: BTreeSet<WorkspaceId>,
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
    fn service_profiles_are_limited_to_the_planes_a_process_starts() {
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
