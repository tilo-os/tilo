//! The screenshot scenarios: the bundled sample city, staged.
//!
//! A capture never reaches the network. What the map draws that the resource
//! does not hold — the stylised basemap standing in for raster imagery, the
//! camera, the ranked results — is the scenario's [`Sample`] fixture. The
//! places, searches, route and transit options are the resource's own, so a
//! scenario writes them with the same actions a person's edits take. The view
//! state a scenario shows (an open panel, a menu, a detached navigation card)
//! is staged by the surface once those actions are laid out.

use ui::framework::Fixture;
use ui::tilo_resource::Invoke;

use crate::actions::MapsAction;
use crate::model::{TransitTime, TransitTimeMode, TravelMode};
use crate::resource::Maps;
use crate::sample;

/// Every named scenario `--shots --scenario NAME` captures.
pub(crate) const SCENARIOS: &[&str] = &[
    "empty",
    "explore",
    "directions",
    "transit",
    "transit-arrive",
    "edit-from",
    "search",
    "pois",
    "traffic",
    "traffic-empty",
    "offline",
    "menu",
    "attribution",
    "error",
    "narrow",
    "nav-closed",
    "floating-navigation",
    "floating-navigation-attached",
    "floating-navigation-hover",
    "floating-navigation-returning",
    "floating-navigation-returned",
];

/// What a capture's map shows in place of live imagery and a live session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sample {
    /// The bundled sample city: its basemap, camera and ranked results.
    City,
    /// A first run: a plain themed basemap and nothing else.
    Empty,
}

impl Sample {
    fn of(scenario: Option<&str>) -> Self {
        match scenario {
            Some("empty" | "traffic-empty") => Self::Empty,
            _ => Self::City,
        }
    }
}

/// Prepare `fixture`'s sample before the view is built.
pub(crate) fn prepare(fixture: &mut Fixture<Maps>) {
    let sample = Sample::of(fixture.scenario());
    fixture.provide(sample);
}

/// The viewport `scenario` is captured at.
pub(crate) fn viewport(scenario: Option<&str>) -> (u32, u32) {
    match scenario {
        Some("narrow" | "nav-closed") => (390, 844),
        _ => (1280, 800),
    }
}

/// The actions that put `scenario`'s places, searches and plans in the
/// resource.
pub(crate) fn actions(scenario: Option<&str>) -> Vec<Invoke<Maps>> {
    if Sample::of(scenario) == Sample::Empty {
        return Vec::new();
    }
    let mut actions = city();
    match scenario {
        Some("transit") => {
            actions.push(MapsAction::SetTravelMode {
                mode: TravelMode::Transit,
            });
            actions.push(MapsAction::ClearRoute);
        }
        Some("transit-arrive") => {
            actions.push(MapsAction::SetTravelMode {
                mode: TravelMode::Transit,
            });
            actions.push(MapsAction::ClearRoute);
            actions.push(MapsAction::SetTransitTime {
                value: TransitTime {
                    mode: TransitTimeMode::Arrive,
                    offset_min: 60,
                },
            });
        }
        Some("pois" | "offline") => actions.push(MapsAction::ClearRoute),
        _ => {}
    }
    actions
        .iter()
        .filter_map(|action| Invoke::apply(action).ok())
        .collect()
}

/// The sample city as the resource holds it: its places (two of them
/// saved), three recent searches, and a road route beside transit options.
fn city() -> Vec<MapsAction> {
    let model = sample::default_model();
    let mut actions = vec![MapsAction::UpsertPlaces {
        places: model.places.clone(),
    }];
    // Each committed search moves to the front of the history, so the
    // oldest is committed first.
    actions.extend(
        model
            .recents
            .iter()
            .rev()
            .map(|query| MapsAction::CommitSearch {
                query: query.clone(),
            }),
    );
    // Accepted transit options switch the travel mode to transit; the road
    // route accepted after them switches it back, keeping both plans.
    actions.push(MapsAction::SetTransit {
        transit: model.transit.clone(),
        origin: model.origin,
        origin_name: model.origin_name.clone(),
        dest: model.dest,
        dest_name: model.dest_name.clone(),
    });
    if let Some(route) = model.route.clone() {
        actions.push(MapsAction::SetRoute {
            route,
            origin: model.origin,
            origin_name: model.origin_name.clone(),
            dest: model.dest,
            dest_name: model.dest_name.clone(),
            mode: model.mode,
        });
    }
    actions
}
