//! Device location, as the host provides it.
//!
//! Apps ask [`crate::host::HostServices::location`] for a fix and drain the
//! returned channel once per frame; the UI thread never blocks. The host
//! mediates the permission prompt and never fabricates a position: when none
//! can be obtained the answer is an honest [`LocationOutcome::Unavailable`].

use std::sync::mpsc::Receiver;

/// The result of a location request, delivered back to the UI thread over a
/// channel.
#[derive(Clone, Debug, PartialEq)]
pub enum LocationOutcome {
    /// A real fix from the host location service.
    Located {
        lat: f64,
        lon: f64,
        /// Reported horizontal accuracy in metres (0 when unknown).
        accuracy_m: f64,
    },
    /// No position could be obtained: the service was missing, the user
    /// denied permission, or no fix arrived. Carries a human-readable reason
    /// so the UI can say *why* rather than inventing a location.
    Unavailable(String),
}

/// A pending location request: the receiver the UI thread drains each frame.
pub type LocationChannel = Receiver<LocationOutcome>;

/// Ask the current host for the device's location.
pub fn request_location() -> LocationChannel {
    crate::host::current().location.locate()
}
