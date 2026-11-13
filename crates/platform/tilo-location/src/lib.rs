#![cfg_attr(
    test,
    allow(
        clippy::disallowed_methods,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

//! Device location: one API on Linux, macOS and the web. Apps call
//! [`request_location`] and drain the returned channel once per frame, so the
//! UI thread never blocks. The platform mediates the permission prompt, and
//! nothing here fabricates a position.

use std::sync::mpsc::Receiver;
use std::sync::{Arc, OnceLock};

mod caps;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod web;

pub use caps::{Availability, Feature, Unsupported, availability, available};

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

pub type LocationChannel = Receiver<LocationOutcome>;

pub trait LocationProvider: Send + Sync {
    fn locate(&self) -> LocationChannel;
}

pub fn request_location() -> LocationChannel {
    RESOLVER
        .get()
        .map_or_else(unavailable, |resolve| resolve())
        .locate()
}

static RESOLVER: OnceLock<fn() -> Arc<dyn LocationProvider>> = OnceLock::new();

pub fn install_resolver(resolve: fn() -> Arc<dyn LocationProvider>) {
    let _ = RESOLVER.set(resolve);
}

pub fn system() -> Arc<dyn LocationProvider> {
    #[cfg(target_os = "linux")]
    return Arc::new(linux::PortalLocation);
    #[cfg(target_os = "macos")]
    return Arc::new(macos::CoreLocation);
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    return Arc::new(web::Geolocation);
    #[allow(unreachable_code, reason = "a target without a backend yet")]
    Arc::new(Refusing(Unsupported::of(Feature::Locate).reason))
}

pub fn unavailable() -> Arc<dyn LocationProvider> {
    Arc::new(Refusing("Location isn't available here"))
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
