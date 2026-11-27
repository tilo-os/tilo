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
//! services a device offers. Which device serves a workspace is the
//! workspace's governance and the device's own [`crate::WorkSetting`].

use serde::{Deserialize, Serialize};

/// Stable certified-device routing key.
pub type DeviceKey = [u8; 32];

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
    /// Server end-to-end encrypted collaboration and chat traffic.
    pub inbound_server: bool,
    /// Relay an end-to-end remote-control session when direct reachability
    /// fails. Consent and input authorization remain enforced by the target.
    pub remote_control_server: bool,
    /// Run app backends for eligible clients.
    pub app_serving: bool,
    /// Accept distributed jobs. This is a placement boundary only; it grants no
    /// ability to execute code outside the app sandbox.
    pub compute: bool,
}

impl ServiceCapabilities {
    /// Capabilities implemented by the standalone server daemon.
    ///
    /// The server durably carries authenticated workspace CRDT updates and
    /// presence. It does not run the filesystem/blob host protocol, an app
    /// runtime, or a compute worker. Live workspace-control frames use this
    /// daemon's already-authenticated, non-retained presence fan-out rather
    /// than a second tunnel protocol.
    pub const fn relay_only() -> Self {
        Self {
            workspace_replication: true,
            inbound_server: true,
            remote_control_server: true,
            app_serving: false,
            compute: false,
        }
    }

    /// Services expected from a personal always-on server.
    ///
    /// This is a broader policy template for hosts that also run app,
    /// compute, and remote-control workers.
    pub const fn personal_server() -> Self {
        Self {
            workspace_replication: true,
            inbound_server: true,
            remote_control_server: true,
            app_serving: true,
            compute: true,
        }
    }

    /// Intersect two service sets. Used by a process to ensure its wire
    /// advertisement cannot exceed the protocols it has actually started.
    #[must_use]
    pub const fn limited_to(&self, available: &Self) -> Self {
        Self {
            workspace_replication: self.workspace_replication && available.workspace_replication,
            inbound_server: self.inbound_server && available.inbound_server,
            remote_control_server: self.remote_control_server && available.remote_control_server,
            app_serving: self.app_serving && available.app_serving,
            compute: self.compute && available.compute,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_profiles_are_limited_to_the_protocols_a_process_starts() {
        // A server-only device claims only its running workspace protocols.
        let services = ServiceCapabilities::relay_only();
        assert!(services.workspace_replication);
        assert!(services.inbound_server);
        assert!(services.remote_control_server);
        assert!(!services.app_serving);
        assert!(!services.compute);
        assert_eq!(
            ServiceCapabilities::personal_server().limited_to(&services),
            services
        );
    }
}
