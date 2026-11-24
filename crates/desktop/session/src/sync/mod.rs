//! The session as a client of the peer network: the workspaces it shows, this person's
//! presence in them, and everyone else's.
//!
//! The client itself — the boards of each workspace, this device's own word on them, and the peer network
//! service every host carries it over — lives in `tilo-session-client`, so an agent or a program links
//! it without a desktop. What this session adds is the desktop's own host: the peer actor on an
//! iroh endpoint, the live session, the workspaces' state and their channels, the device fabric and
//! the transfers, and the roster the bar draws.

pub use tilo_session_client::presence;
pub use tilo_session_client::{
    Answer, CallFrame, CallPicture, CallShown, Closed, Cmd, Device, Enrolled, Event, InCall,
    LiveCall, LiveCaller, Outbox, OwnPresence, PageShown, Receipt, RosterDevice, SayPresence,
    Topic, Transport, answer, topic_of,
};

pub mod layout;

/// How long coming into a workspace may take: redeeming an invitation asks its host three times,
/// each exchange with its own bound, and entering through an API asks the API's host on a session
/// and then takes the workspace's state by the membership exchange.
pub(crate) const REDEEM_WITHIN: std::time::Duration = std::time::Duration::from_secs(120);

/// How long taking the granted workspace in may take: installing what the host granted and telling
/// the peer network to carry it is this device's own work, and the person is told a join did not happen
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

/// One of the person's workspaces this device holds nothing of yet: it takes the workspace in as a
/// further device of their person, at the device hosting it. A device certified by a member's root
/// is that member's device, so nothing has to be invited here
/// ([`tilo_peer_state::owner::enroll::admit_as_member`]).
#[cfg(target_os = "linux")]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entering {
    /// The workspace the person's list names.
    pub workspace_id: tilo_desktop_model::WorkspaceId,
    /// What the person's list calls it.
    pub name: String,
    /// Where the device hosting it is reached, as its host signed it.
    pub host: tilo_peer_state::join::TransportHint,
    /// Already on this session, held as the device this machine was copied
    /// from: an entry that does not land leaves it where it is.
    pub held: bool,
}

mod client;
pub use client::Client;
#[cfg(all(test, target_os = "linux"))]
pub(crate) use client::HostCmd;
#[cfg(target_os = "linux")]
pub(crate) use client::HostOutbox;
#[cfg(target_os = "linux")]
pub(crate) use client::open_alone;
#[cfg(target_os = "linux")]
pub use client::{Entry, PeerDocuments, configured_hosts, device_identity};
