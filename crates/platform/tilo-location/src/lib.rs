#![cfg_attr(
    test,
    allow(
        clippy::disallowed_methods,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

//! Device location: one API on Linux, macOS and the web.
//!
//! Apps call [`request_location`] and drain the returned channel once per
//! frame; the UI thread never blocks. The platform mediates the permission
//! prompt and nothing here fabricates a position: when none can be obtained
//! the answer is an honest [`LocationOutcome::Unavailable`].
//!
//! | Target | Backend |
//! |---|---|
//! | Linux | the XDG Desktop Portal's `Location` (GeoClue, or the Tilo compositor) |
//! | macOS | CoreLocation, for a bundle whose `Info.plist` has `NSLocationWhenInUseUsageDescription`; an unbundled binary is answered [`LocationOutcome::Unavailable`] |
//! | Web | the Geolocation API, after the browser's prompt |

use std::sync::mpsc::Receiver;
use std::sync::{Arc, OnceLock};

mod caps;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod web;

pub use caps::{Availability, CAPABILITIES, Feature, Unsupported, availability};

/// The result of a location request, delivered back to the UI thread over a
/// channel.
#[derive(Clone, Debug, PartialEq)]
pub enum LocationOutcome {
    /// A real fix from the platform's location service.
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

/// Host-provided device location.
pub trait LocationProvider: Send + Sync {
    /// Request one fix. Answers over the channel, never blocking the caller.
    fn locate(&self) -> LocationChannel;
}

/// Ask the current host for the device's location.
pub fn request_location() -> LocationChannel {
    RESOLVER
        .get()
        .map_or_else(unavailable, |resolve| resolve())
        .locate()
}

static RESOLVER: OnceLock<fn() -> Arc<dyn LocationProvider>> = OnceLock::new();

/// Install how [`request_location`] finds the current host's provider. The
/// first call wins; `ui::host` installs one that resolves through its scoped
/// `HostServices`. Without one, requests answer
/// [`LocationOutcome::Unavailable`].
pub fn install_resolver(resolve: fn() -> Arc<dyn LocationProvider>) {
    let _ = RESOLVER.set(resolve);
}

/// This target's location provider.
pub fn system() -> Arc<dyn LocationProvider> {
    #[cfg(target_os = "linux")]
    return Arc::new(linux::PortalLocation);
    #[cfg(target_os = "macos")]
    return Arc::new(macos::CoreLocation);
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    return Arc::new(web::Geolocation);
    #[allow(unreachable_code, reason = "a target without a backend yet")]
    Arc::new(Refusing(caps::unsupported(Feature::Locate).reason))
}

/// A provider that never locates, for hosts that offer none.
pub fn unavailable() -> Arc<dyn LocationProvider> {
    Arc::new(Refusing("location is unavailable in this host"))
}

struct Refusing(&'static str);

impl LocationProvider for Refusing {
    fn locate(&self) -> LocationChannel {
        let (tx, rx) = std::sync::mpsc::channel();
        let _ = tx.send(LocationOutcome::Unavailable(self.0.to_owned()));
        rx
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_a_host_a_request_is_answered_unavailable() {
        assert!(matches!(
            request_location().recv(),
            Ok(LocationOutcome::Unavailable(_))
        ));
    }

    #[test]
    fn every_target_prompts_for_location() {
        assert_eq!(availability(Feature::Locate), Availability::Prompt);
    }
}
