//! The session as a client of the collaboration plane: the workspaces it shows, this person's
//! presence in them, and everyone else's.
//!
//! [`presence`] is what every host runs: the boards of each workspace and this device's own word on
//! them. What a host binds to reach the others is a transport of its own — this desktop's peer actor
//! on an iroh endpoint, the page's own endpoint — and it reaches the boards through
//! [`Transport`]'s two ends ([`Client::over`]).
#[cfg_attr(
    target_arch = "wasm32",
    allow(dead_code, reason = "the page's transport is handed these boards next")
)]
pub(crate) mod presence;

use tilo_desktop_model::{WorkspaceId as DeskWorkspaceId, WorkspaceSlot as DeskSlot};

pub use crate::platform::plane::{Cmd, Device, Event, Topic, Transport};
pub use presence::OwnPresence;
pub use tilo_live::presence::Call as InCall;
/// A call in progress in a workspace, as the people in it say.
pub use tilo_live::presence::{LiveCall, LiveCaller};

/// How long redeeming an invitation to a workspace may take: the host is asked three times, each
/// exchange with its own bound.
pub(crate) const REDEEM_WITHIN: std::time::Duration = std::time::Duration::from_secs(120);

/// How long taking the granted workspace in may take: installing what the host granted and telling
/// the plane to carry it is this device's own work, and the person is told a join did not happen
/// rather than left waiting on it.
pub(crate) const SETTLE_WITHIN: std::time::Duration = std::time::Duration::from_secs(30);

/// How long a join may take before the person is told it did not happen: the host's answer may take
/// [`REDEEM_WITHIN`], and taking the granted workspace in [`SETTLE_WITHIN`] after it. A test waiting
/// for a join waits past this, so what it reports is the join's outcome, never its own patience
/// running out first.
pub const JOIN_PATIENCE: std::time::Duration = std::time::Duration::from_secs(
    REDEEM_WITHIN
        .as_secs()
        .saturating_add(SETTLE_WITHIN.as_secs()),
);

/// A workspace this session is joining: the invitation that admits this device to its workspace, and
/// the workspace on the desk made for it, which is where what the workspace holds arrives.
#[derive(Debug)]
pub struct Enrolling {
    /// The slot the desk made for it.
    pub slot: DeskSlot,
    /// The workspace the invitation grants.
    pub workspace_id: DeskWorkspaceId,
    /// The invitation itself, verified for this person before it got here.
    pub invitation: String,
    /// Renewing this device's certificate before it is presented, when this device joined a
    /// person's identity: the ask the session's pass may already have under way, waited for rather
    /// than presented past.
    #[cfg(target_os = "linux")]
    pub(crate) renewal: Option<std::sync::Arc<crate::mount::renewal::FleetRenewal>>,
}

/// One of the person's workspaces this device holds nothing of yet: it takes the workspace in as a
/// further device of their person, at the device hosting it. A device certified by a member's root
/// is that member's device, so nothing has to be invited here
/// ([`tilo_peer_state::owner::enroll::admit_as_member`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entering {
    /// The slot the desk makes for it.
    pub slot: DeskSlot,
    /// The workspace the person's list names.
    pub workspace_id: DeskWorkspaceId,
    /// What the person's list calls it.
    pub name: String,
    /// Where the device hosting it is reached, as its host signed it.
    pub host: tilo_peer_state::join::TransportHint,
    /// Already on this desk, held as the device this machine was copied
    /// from: an entry that does not land leaves it where it is.
    pub held: bool,
}

/// What became of an enrollment or an entry, and the workspace it was for.
#[derive(Debug)]
pub struct Enrolled {
    pub slot: DeskSlot,
    pub workspace_id: DeskWorkspaceId,
    /// Whether this device entered one of the person's own workspaces as their further device,
    /// rather than redeeming an invitation into someone's.
    pub member: bool,
    /// The name the person's list gave it, when it was entered from there.
    pub name: Option<String>,
    /// `Ok` once this device holds the workspace and takes part in it.
    pub result: Result<(), String>,
    /// The entry was for a workspace already on this desk ([`Entering::held`]).
    pub held: bool,
}

mod client;
pub use client::Client;
#[cfg(target_os = "linux")]
pub(crate) use client::ReceivedSurface;
#[cfg(target_os = "linux")]
pub(crate) use client::SiblingDials;
#[cfg(target_os = "linux")]
pub use client::{Entry, PeerDocuments, configured_hosts, device_identity};
