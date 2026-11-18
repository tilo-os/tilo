//! The screenshot scenarios of the guided setup: which page each one shows,
//! and the journey it shows it on.

use super::{Destination, Identity, Journey, Page, machine};

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
    "installer-disk",
    "installer-method",
    "installer-machine",
    "installer-plan-os",
    "installer-plan-waiting",
    "installer-plan-review",
    "installer-plan-ready",
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
        "installer-disk" => {
            journey.destination = Some(Destination::Another);
            journey.plan.os = Some(machine::Os::TiloOs);
            Page::Storage
        }
        "installer-method" => {
            journey.destination = Some(Destination::Another);
            journey.plan.os = Some(machine::Os::TiloServer);
            journey.plan.hosting = Some(machine::Hosting::HoldsNone);
            Page::Method
        }
        "installer-machine" => {
            journey.destination = Some(Destination::Another);
            journey.plan.os = Some(machine::Os::TiloServer);
            journey.plan.hosting = Some(machine::Hosting::HoldsNone);
            journey.device_name = "cupboard".into();
            Page::Machine
        }
        "installer-plan-os" => {
            journey.destination = Some(Destination::Another);
            Page::Destination
        }
        "installer-plan-waiting" => Page::Destination,
        "installer-plan-review" => {
            journey.destination = Some(Destination::Another);
            journey.plan.os = Some(machine::Os::TiloServer);
            journey.plan.hosting = Some(machine::Hosting::HoldsNone);
            journey.plan.name_open = true;
            journey.device_name = "cupboard".into();
            journey.plan.disk = None;
            // A server takes files from this desktop, never its apps or
            // sign-ins.
            journey.migrate = Some(true);
            journey.categories = [true, false, false];
            Page::Review
        }
        "installer-plan-ready" => {
            journey.destination = Some(Destination::Another);
            journey.plan.os = Some(machine::Os::TiloServer);
            journey.plan.hosting = Some(machine::Hosting::HoldsNone);
            Page::Ready
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
