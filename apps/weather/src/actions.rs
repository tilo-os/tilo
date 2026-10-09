//! What Weather does with its saved places beyond the document.
//!
//! Saved places, their order and the default place are the document
//! ([`Weather`](crate::model::Weather)), changed by its verbs and field edits.
//! Opening a forecast and fetching one change no document, so they are
//! [`WeatherEffect`]s answered by the window, offered on each place as a live
//! row joining that place's published item.

use ui::prelude::{Effect, LiveRow, tilo_resource};

use crate::model::{LocationId, SavedLocation, Weather};

/// The resource a saved place is published as: its item of the record,
/// which this window's live row joins.
pub fn location_resource_id(id: LocationId) -> String {
    tilo_resource::items::item_resource_id::<Weather>("places", &Weather::item(id))
}

/// What Weather fetches and shows, with no document delta.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "weather.effects")]
pub enum WeatherEffect {
    /// Refresh forecasts.
    ///
    /// Fetch the forecast and air quality of every saved place.
    #[effect(job, permission = "read")]
    Refresh,
    /// Refresh this forecast.
    ///
    /// Fetch the forecast and air quality of one saved place.
    #[effect(job, permission = "read")]
    RefreshLocation {
        #[param("Id of the saved place")]
        location: String,
    },
    /// Open forecast.
    ///
    /// Show one saved place's forecast.
    #[effect(permission = "read")]
    OpenLocation {
        #[param("Id of the saved place")]
        location: String,
    },
}

/// A saved place as the window offers it: open its forecast, refresh it.
/// Joins the place's published child of the same id.
pub(crate) fn location_row(location: &SavedLocation) -> LiveRow {
    let id = location.id.to_string();
    let mut open = WeatherEffect::OpenLocation {
        location: id.clone(),
    }
    .bound();
    open.id = "open".into();
    let mut refresh = WeatherEffect::RefreshLocation { location: id }.bound();
    refresh.id = "refresh".into();
    LiveRow::new(
        location_resource_id(location.id),
        "weather.location",
        location.name.clone(),
    )
    .operation(open)
    .operation(refresh)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Place;
    use crate::resource::PlaceDetails;
    use ui::tilo_resource::{Invoke, Replica};

    /// The id the seed gives its `n`th saved place.
    fn nth_id(n: usize) -> LocationId {
        LocationId::from_u128(u128::try_from(n).expect("small index") + 1)
    }

    /// The bundled sample places, with stable ids, inserted at their paths.
    fn seeded() -> Replica<Weather> {
        let mut replica = Replica::<Weather>::new();
        for (n, location) in crate::sample::sample_model().locations.iter().enumerate() {
            let insert = Weather::paths().places.insert_as(
                &Weather::item(nth_id(n)),
                n,
                &Place::of(location),
            );
            replica.apply(Invoke::edit(&insert).unwrap()).unwrap();
        }
        replica
    }

    fn details(name: &str, latitude: f64, longitude: f64) -> PlaceDetails {
        PlaceDetails {
            name: name.into(),
            country: "Somewhere".into(),
            latitude,
            longitude,
            region: None,
            country_code: None,
            timezone: None,
        }
    }

    #[test]
    fn each_place_publishes_its_verbs_and_its_row_offers_its_effects() {
        let published = seeded().published().unwrap();
        let root_actions: Vec<&str> = published.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(root_actions.contains(&"save-location"), "{root_actions:?}");
        let places: Vec<_> = published
            .children
            .iter()
            .filter(|child| child.kind == "weather.locations.places")
            .collect();
        assert_eq!(places.len(), crate::sample::sample_model().locations.len());
        for place in &places {
            for verb in ["use-as-default", "move-to-top", "remove"] {
                assert!(
                    place.actions.iter().any(|action| action.id == verb),
                    "{verb} on {}",
                    place.id.as_str()
                );
            }
        }

        let place = seeded().state().unwrap().model().locations.remove(1);
        let row = location_row(&place).into_resource();
        assert_eq!(row.id.as_str(), location_resource_id(place.id));
        assert!(
            places.iter().any(|child| child.id == row.id),
            "the row joins its place"
        );
        for (id, effect) in [
            (
                "open",
                WeatherEffect::OpenLocation {
                    location: place.id.to_string(),
                },
            ),
            (
                "refresh",
                WeatherEffect::RefreshLocation {
                    location: place.id.to_string(),
                },
            ),
        ] {
            let spec = row.actions.iter().find(|action| action.id == id).unwrap();
            assert_eq!(spec.kind, tilo_actions::OperationKind::Effect);
            let encoded = spec.build_json(&serde_json::Map::new()).unwrap();
            assert_eq!(
                WeatherEffect::from_payload(&encoded.payload).unwrap(),
                effect
            );
        }
    }

    /// Saving keeps one place per spot, making a place the default, moving
    /// it up and removing it are each one undo step of the record.
    #[test]
    fn the_verbs_change_the_places_and_refuse_what_cannot_be() {
        let mut replica = seeded();
        let count = replica.state().unwrap().places.len();
        replica
            .apply(
                Weather::SAVE_LOCATION.with((details("Auckland", -36.8485, 174.7633), Some(true))),
            )
            .unwrap();
        let state = replica.state().unwrap();
        assert_eq!(state.places.len(), count + 1);
        assert_eq!(state.model().selected_location().unwrap().name, "Auckland");
        let first = state.model().locations[0].clone();
        replica
            .apply(Weather::SAVE_LOCATION.with((
                details("Again", first.latitude + 0.01, first.longitude),
                Some(false),
            )))
            .unwrap();
        assert_eq!(
            replica.state().unwrap().places.len(),
            count + 1,
            "a place near a saved one is that place"
        );
        assert!(
            replica
                .apply(Weather::SAVE_LOCATION.with((details("Nowhere", 120.0, 0.0), None)))
                .is_err(),
            "coordinates off the globe are refused"
        );
        assert!(
            replica
                .apply(Weather::MOVE_TO_TOP.with(Weather::item(first.id)))
                .is_err(),
            "the first place is already on top"
        );
        let second = Weather::item(nth_id(1));
        replica
            .apply(Weather::MOVE_TO_TOP.with(second.clone()))
            .unwrap();
        assert_eq!(replica.state().unwrap().model().locations[0].id, nth_id(1));
        replica
            .apply(Weather::USE_AS_DEFAULT.with(second.clone()))
            .unwrap();
        replica
            .apply_confirmed(Weather::REMOVE.with(second))
            .unwrap();
        let state = replica.state().unwrap();
        assert!(
            state.default_place.0.is_empty(),
            "removing the default forgets it"
        );
        assert_eq!(state.places.len(), count);
    }

    /// Places two devices add, and moves of different places, converge.
    #[test]
    fn saved_places_converge() {
        let mut left = seeded();
        let mut right = left.fork();
        left.apply(Weather::SAVE_LOCATION.with((details("Berlin", 52.52, 13.405), None)))
            .unwrap();
        right
            .apply(Weather::SAVE_LOCATION.with((details("Tokyo", 35.6762, 139.6503), None)))
            .unwrap();
        let places = Weather::paths().places;
        left.apply(Invoke::edit(&places.move_to(&Weather::item(nth_id(3)), 0)).unwrap())
            .unwrap();
        right
            .apply(Invoke::edit(&places.move_to(&Weather::item(nth_id(2)), 1)).unwrap())
            .unwrap();
        left.sync_with(&mut right).unwrap();
        assert_eq!(left.state().unwrap(), right.state().unwrap());
        let names: Vec<String> = left
            .state()
            .unwrap()
            .model()
            .locations
            .into_iter()
            .map(|location| location.name)
            .collect();
        assert!(names.contains(&"Berlin".to_owned()) && names.contains(&"Tokyo".to_owned()));
    }
}
