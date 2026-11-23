//! The screenshot scenarios of the guided setup: which page each one shows,
//! and the journey it shows it on.

use super::{Destination, Journey, Page, machine};

pub const SCENARIOS: &[&str] = &[
    "installer-destination",
    "installer-migrate",
    "installer-password",
    "installer-devices",
    "installer-purpose",
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
        "installer-migrate" => Page::Migrate,
        "installer-password" => Page::Password,
        "installer-devices" => Page::Devices,
        "installer-purpose" => Page::Purpose,
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
