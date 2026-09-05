//! Stable resource/action addresses exported by Weather.
//!
//! Saved locations are collaborative document resources, while choosing which
//! forecast to show is presentation-only. Search, agents, accessibility and
//! in-app navigation therefore address the same exported `open` [`View`]
//! operation. Network refresh remains a separate [`Effect`] operation.
//!
//! [`View`]: qos_actions::OperationKind::View
//! [`Effect`]: qos_actions::OperationKind::Effect

use qos_actions::ActionAddress;

use crate::model::LocationId;

/// Widget-local operation type used to reveal one saved forecast.
pub const OPEN_LOCATION_OPERATION: &str = "weather.open-location";
/// Widget-local effect type used to refresh forecasts from the network.
pub const REFRESH_OPERATION: &str = "weather.refresh";

/// Stable resource id for one saved location.
pub fn location_resource_id(id: LocationId) -> String {
    format!("weather.location:{id}")
}

/// Canonical view action that selects and reveals one saved forecast.
pub fn open_location_address(id: LocationId) -> ActionAddress {
    ActionAddress::new(location_resource_id(id), "open")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_location_addresses_are_stable_and_unique() {
        let locations = crate::sample::sample_model().locations;
        let first = locations[0].id;
        let second = locations[1].id;

        assert_eq!(open_location_address(first), open_location_address(first));
        assert_ne!(open_location_address(first), open_location_address(second));
        assert_eq!(
            open_location_address(first),
            ActionAddress::new(location_resource_id(first), "open")
        );
    }
}
