//! Whose dates a worker renders: the formats and the zone of the person a view is for.
//!
//! The workers are shared by everyone this device serves, and a confined worker reads neither
//! settings nor the zone database. So every request carries the region its views render in
//! ([`Region`]): the person's formats, and their zone as its `TZif` data. The workspace actions
//! name the person each request is for ([`scope`]): on a server, the formats and zone that
//! person keeps there; with nobody named, the device's own, which on a person's own device are
//! theirs.

use std::cell::RefCell;
use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use ui::format::region::RegionFormats;

/// The region a view is rendered in for a person: their formats, and the zone their clock
/// reads (`Pacific/Auckland`), none for the device's own.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PersonRegion {
    pub formats: RegionFormats,
    pub zone: Option<String>,
}

/// A region as it crosses to a worker: the formats, and the zone by its name with its `TZif`
/// data, which the worker cannot read for itself.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Region {
    pub(crate) formats: RegionFormats,
    pub(crate) zone: Option<(String, Vec<u8>)>,
}

thread_local! {
    /// The region this thread renders in now, while a [`Scope`] holds it.
    static SCOPED: RefCell<Option<PersonRegion>> = const { RefCell::new(None) };
}

/// Holds a thread's region until it is dropped, then gives back the one before.
pub(crate) struct Scope {
    before: Option<PersonRegion>,
}

impl Drop for Scope {
    fn drop(&mut self) {
        let before = self.before.take();
        SCOPED.with(|scoped| *scoped.borrow_mut() = before);
    }
}

/// Render every view this thread asks a worker for in `region` until the scope drops; `None`
/// renders in the device's own.
pub(crate) fn scope(region: Option<PersonRegion>) -> Scope {
    Scope {
        before: SCOPED.with(|scoped| scoped.replace(region)),
    }
}

/// What a request this thread sends carries: the region in scope, else the device's own (the
/// formats its settings hold, and the zone it is in).
pub(crate) fn wire() -> Region {
    let region = SCOPED
        .with(|scoped| scoped.borrow().clone())
        .unwrap_or_else(|| PersonRegion {
            formats: ui::format::region::current(),
            zone: tilo_clock::local_zone_name(),
        });
    Region {
        formats: region.formats,
        zone: region
            .zone
            .and_then(|name| tilo_clock::local::tzif(&name).map(|data| (name, data.to_vec()))),
    }
}

/// What a worker installed last, and the zones it read, by name: one person's views follow
/// another's in the same worker, and each zone is read from its data once.
#[derive(Default)]
pub(super) struct Installed {
    zones: HashMap<String, Option<ui::clock::Zone>>,
}

/// Most zones a worker keeps read; past it, it reads them again rather than grow.
const MAX_ZONES: usize = 64;

impl Installed {
    /// Render what comes next on this thread in `region`: its formats, and its zone, or the
    /// worker's own where it names none or its data is no zone's.
    pub(super) fn install(&mut self, region: Region) {
        let zone = region.zone.and_then(|(name, data)| {
            if self.zones.len() >= MAX_ZONES && !self.zones.contains_key(&name) {
                self.zones.clear();
            }
            self.zones
                .entry(name)
                .or_insert_with_key(|name| ui::clock::Zone::from_tzif(name, &data))
                .clone()
        });
        ui::format::region::install_here(Some(region.formats), zone);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A request carries the region of the person it renders for, and the worker reads its
    /// dates as that person does; with nobody named, the device's own.
    #[test]
    fn a_worker_renders_in_the_region_of_the_person_a_view_is_for() {
        let Some(_) = tilo_clock::local::tzif("Pacific/Auckland") else {
            // A system without a zone database has no zone to hand.
            return;
        };
        let before = ui::format::region::current();
        let at = jiff::Timestamp::from_second(1_773_583_500).unwrap();
        let wired = {
            let _scope = scope(Some(PersonRegion {
                formats: RegionFormats {
                    time: ui::clock::TimeStyle::H24,
                    ..RegionFormats::default()
                },
                zone: Some("Pacific/Auckland".into()),
            }));
            wire()
        };
        assert_eq!(wired.formats.time, ui::clock::TimeStyle::H24);
        assert_eq!(
            wired.zone.as_ref().map(|(name, _)| name.as_str()),
            Some("Pacific/Auckland")
        );
        assert_eq!(
            wire().formats,
            before,
            "once the scope ends, a request carries the device's own formats"
        );
        let mut installed = Installed::default();
        installed.install(wired);
        assert_eq!(
            ui::format::time_of(&at),
            "03:05",
            "14:05 UTC is 03:05 the next morning in Auckland, on a 24-hour clock"
        );
        // Another person's views render beside these on a thread of their own, untouched.
        let beside = std::thread::spawn(move || {
            (
                ui::format::region::current(),
                ui::clock::Zone::here().name(),
            )
        })
        .join()
        .unwrap();
        assert_eq!(
            beside.0, before,
            "another thread keeps the device's formats"
        );
        assert_ne!(beside.1.as_deref(), Some("Pacific/Auckland"));
        installed.install(Region {
            formats: before,
            zone: None,
        });
    }
}
