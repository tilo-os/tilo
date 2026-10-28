//! Hosting the person's workspaces on their home relay as well as on the
//! device that governs them: per workspace, the relay is one of the room's
//! hosts or it is not, as the room's governor records its hosts. It is
//! answered under the host's provider, which a caller names without reading
//! a catalogue, for the person at this device.

use serde::{Deserialize, Serialize};

use super::{ActionAddress, RemoteActionTarget};

/// The provider the host answers home-relay hosting under.
pub const PROVIDER: &str = "tilo.hosting";
/// The resource the steps are actions of.
pub const RESOURCE: &str = "home-relay";
/// The workspaces this device governs, each with whether the home relay
/// hosts it too, as a [`HomeRelayHosting`].
pub const WORKSPACES: &str = "workspaces";
/// Have the home relay host workspace `room` as well (`on`), or no
/// longer.
pub const SET: &str = "set";

/// Every step, in the order a catalogue lists them.
pub const STEPS: [&str; 2] = [WORKSPACES, SET];

#[must_use]
pub fn target(step: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, step),
    }
}

/// The person's home relay and the workspaces it may host, as
/// [`WORKSPACES`] answers them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomeRelayHosting {
    /// The name the home relay was paired with; `None` when the person
    /// has none.
    pub relay: Option<String>,
    /// The workspaces this device governs, in the order the person
    /// keeps them.
    pub workspaces: Vec<HostedWorkspace>,
}

/// One workspace this device governs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostedWorkspace {
    /// Its room's id.
    pub room: String,
    /// Its name.
    pub name: String,
    /// Whether the home relay is one of its hosts.
    pub on_relay: bool,
}
