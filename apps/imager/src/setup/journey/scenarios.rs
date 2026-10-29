//! The screenshot scenarios of the guided setup: which page each one shows,
//! and the journey it shows it on.

use super::{Destination, Identity, Journey, Page};

pub const SCENARIOS: &[&str] = &[
    "installer-destination",
    "installer-migration",
    "installer-identity",
    "installer-profile",
    "installer-password",
    "installer-connect",
    "installer-desktop-connect",
    "installer-devices",
    "installer-purpose",
    "installer-belongings",
    "installer-accounts",
    "installer-belongings-details",
    "installer-storage",
    "installer-usb",
    "installer-desktop",
    "installer-review",
    "installer-ready",
];

/// The page `scenario` shows, with what it needs set on `journey`.
pub(super) fn page(journey: &mut Journey, scenario: &str) -> Page {
    match scenario {
        "installer-migration" => Page::Migration,
        "installer-identity" => Page::Identity,
        "installer-profile" => Page::Profile,
        "installer-password" => Page::Password,
        "installer-connect" => {
            journey.identity = Some(Identity::Existing);
            Page::Connect
        }
        "installer-desktop-connect" => {
            journey.destination = Some(Destination::Desktop);
            journey.migrate = Some(false);
            journey.identity = Some(Identity::Existing);
            Page::Connect
        }
        "installer-devices" => Page::Devices,
        "installer-purpose" => Page::Purpose,
        "installer-belongings" => Page::Belongings,
        "installer-belongings-details" => {
            journey.details = true;
            Page::Belongings
        }
        "installer-accounts" => Page::Accounts,
        "installer-storage" => Page::Storage,
        "installer-usb" => {
            journey.destination = Some(Destination::Another);
            Page::Storage
        }
        "installer-desktop" => {
            journey.destination = Some(Destination::Desktop);
            Page::Storage
        }
        "installer-review" => Page::Review,
        "installer-ready" => Page::Ready,
        _ => {
            journey.destination = None;
            Page::Destination
        }
    }
}
