//! Whose dates a worker renders: the device's formats and zone.
//!
//! A confined worker reads neither settings nor the zone database, so every request carries
//! the region its views render in ([`Region`]): the device's formats, and its zone as its
//! `TZif` data.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use ui::format::region::RegionFormats;

/// A region as it crosses to a worker: the formats, and the zone by its name with its `TZif`
/// data, which the worker cannot read for itself.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Region {
    pub(crate) formats: RegionFormats,
    pub(crate) zone: Option<(String, Vec<u8>)>,
}

/// What a request this thread sends carries: the formats the device's settings name, and the
/// zone it is in.
pub(crate) fn wire() -> Region {
    Region {
        formats: ui::format::region::current(),
        zone: tilo_clock::local_zone_name()
            .and_then(|name| tilo_clock::local::tzif(&name).map(|data| (name, data.to_vec()))),
    }
}

/// The zones a worker read, by name: each zone is read from its data once.
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
