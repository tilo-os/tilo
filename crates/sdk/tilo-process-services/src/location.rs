//! Location, as an app's process holds it: the device's answer, which the host asks through the
//! device's own action (`devices/<device>/location#locate`) and installs here.
//!
//! The platform's location service belongs to the device, not to an app: a sandboxed process
//! speaks to no D-Bus and reaches no portal. A host that can ask the device installs a
//! [`LocationProvider`] with [`crate::process::ProcessServices::with_location`]; without one the
//! process answers that location is not available here, never a made-up position.

use std::sync::mpsc::Receiver;
use std::sync::{Arc, OnceLock};

/// What one request for the device's position answered.
#[derive(Clone, Debug, PartialEq)]
pub enum LocationOutcome {
    Located {
        lat: f64,
        lon: f64,
        /// Reported horizontal accuracy in metres (0 when unknown).
        accuracy_m: f64,
    },
    /// No position could be obtained: the service was missing, permission was
    /// denied, or no fix arrived. The reason is for the UI to show.
    Unavailable(String),
}

/// The answer a request for a fix arrives on.
pub type LocationChannel = Receiver<LocationOutcome>;

/// Where the device's position comes from in this process: the host's provider, or nothing.
pub trait LocationProvider: Send + Sync {
    fn locate(&self) -> LocationChannel;
}

/// Ask the current process's location service for a fix.
pub fn request_location() -> LocationChannel {
    crate::process::current().location.locate()
}

/// A provider that answers nothing: a process its host gave no way to reach the device's
/// location.
#[must_use]
pub fn unavailable() -> Arc<dyn LocationProvider> {
    static NOWHERE: OnceLock<Arc<dyn LocationProvider>> = OnceLock::new();
    Arc::clone(NOWHERE.get_or_init(|| Arc::new(Refusing("Location isn't available here"))))
}

struct Refusing(&'static str);

impl LocationProvider for Refusing {
    fn locate(&self) -> LocationChannel {
        answered(LocationOutcome::Unavailable(self.0.to_owned()))
    }
}

fn answered(outcome: LocationOutcome) -> LocationChannel {
    let (tx, rx) = std::sync::mpsc::channel();
    let _ = tx.send(outcome);
    rx
}
