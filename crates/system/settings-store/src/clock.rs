//! This device's clock: whether its time zone follows where the device is.
//!
//! Settings' time-zone picker writes [`AUTOMATIC_ZONE`] (its first choice,
//! "Set automatically"); the session's zone follower reads it, asks for the
//! device's location and sets the zone of the nearest city tzdata publishes
//! ([`nearest_zone_in`]). A zone the person picks turns it off.

use crate::Key;
use crate::day_schedule::{Location, parse_iso6709};

/// The section holding this device's clock settings.
pub const SECTION: &str = "clock";

/// Whether the time zone follows the device's location. Off until the
/// person chooses it: the zone set at installation stays until then.
pub const AUTOMATIC_ZONE: Key<bool> = Key::new(SECTION, "automatic_zone", || false);

/// The tzdata tables that place each zone's city, the 1970 one first.
pub const ZONE_TABLES: [&str; 2] = [
    "/usr/share/zoneinfo/zone1970.tab",
    "/usr/share/zoneinfo/zone.tab",
];

/// The zone of the city nearest `location` in a `zone1970.tab`/`zone.tab`
/// body, by great-circle distance.
#[must_use]
pub fn nearest_zone_in(location: Location, table: &str) -> Option<String> {
    table
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| {
            let mut columns = line.split('\t');
            let _countries = columns.next()?;
            let city = parse_iso6709(columns.next()?)?;
            let zone = columns.next()?.trim();
            (!zone.is_empty()).then(|| (zone, apart(location, city)))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(zone, _)| zone.to_owned())
}

/// The angle between two places on the globe, in radians.
fn apart(a: Location, b: Location) -> f64 {
    let (lat_a, lat_b) = (a.latitude.to_radians(), b.latitude.to_radians());
    let delta = (a.longitude - b.longitude).to_radians();
    (lat_a.sin() * lat_b.sin() + lat_a.cos() * lat_b.cos() * delta.cos())
        .clamp(-1.0, 1.0)
        .acos()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = "# tzdata fixture\n\
        NL\t+5222+00454\tEurope/Amsterdam\n\
        DE\t+5230+01322\tEurope/Berlin\n\
        US\t+404251-0740023\tAmerica/New_York\n\
        FJ\t-1808+17825\tPacific/Fiji\n\
        US\t+515248-1763929\tAmerica/Adak\n";

    #[test]
    fn a_location_takes_the_zone_of_the_nearest_city() {
        let at = |latitude, longitude| {
            nearest_zone_in(
                Location {
                    latitude,
                    longitude,
                },
                TABLE,
            )
        };
        assert_eq!(at(52.09, 5.12).as_deref(), Some("Europe/Amsterdam"));
        assert_eq!(at(52.39, 13.06).as_deref(), Some("Europe/Berlin"));
        assert_eq!(at(40.73, -74.17).as_deref(), Some("America/New_York"));
        assert_eq!(at(-17.0, -179.5).as_deref(), Some("Pacific/Fiji"));
        assert_eq!(
            nearest_zone_in(
                Location {
                    latitude: 0.0,
                    longitude: 0.0
                },
                "# empty\n"
            ),
            None
        );
    }

    #[test]
    fn the_zone_follows_the_location_only_when_chosen() {
        assert!(!AUTOMATIC_ZONE.default_value());
        assert_eq!(AUTOMATIC_ZONE.section(), "clock");
    }
}
