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
    LiveCall, LiveCaller, Outbox, OwnPresence, PageShown, Receipt, RosterDevice, Topic, Transport,
    answer, topic_of,
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

mod client;
pub use client::Client;
#[cfg(all(test, target_os = "linux"))]
pub(crate) use client::HostCmd;
#[cfg(target_os = "linux")]
pub(crate) use client::HostOutbox;
#[cfg(target_os = "linux")]
pub use client::{Entry, configured_hosts, device_identity};
