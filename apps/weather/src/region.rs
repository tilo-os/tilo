//! Display units from the person's region & formats ([`tilo_locale`]).
//!
//! Measurement units are a system preference owned by the Settings app's
//! "Region & Formats" panel, not something Weather hosts its own toggle for.
//! Weather derives its temperature/wind/precipitation display units from it
//! and converts snapshots on the fly (a snapshot always records the units it
//! was fetched in).

use tilo_locale::{Locale, Measurement};

use crate::units::Units;

/// Imperial → °F + mph + inches; metric → °C + km/h + millimetres.
pub fn units_for(measurement: Measurement) -> Units {
    match measurement {
        Measurement::Imperial => Units::IMPERIAL,
        Measurement::Metric => Units::METRIC,
    }
}

/// The person's display units. A missing or unreadable setting reads as
/// metric.
pub fn load_units() -> Units {
    units_for(ui::storage::Settings::<Locale>::shared().get().measurement)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::{PrecipUnit, TempUnit, WindUnit};

    #[test]
    fn measurement_system_maps_to_units() {
        let imperial = units_for(Measurement::Imperial);
        assert_eq!(imperial.temp, TempUnit::Fahrenheit);
        assert_eq!(imperial.wind, WindUnit::Mph);
        assert_eq!(
            imperial.precip,
            PrecipUnit::Inches,
            "a person on Imperial does not read rain in millimetres"
        );
        assert_eq!(units_for(Measurement::Metric), Units::METRIC);
    }
}
