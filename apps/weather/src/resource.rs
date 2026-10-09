//! Weather as a resource: the saved places, their order and the default
//! place ([`Weather`]). Reordering is the collection's move at its path;
//! saving, making a place the default, moving it to the top and removing it
//! are verbs, the last three offered on each place.

use std::rc::Rc;

use serde::{Deserialize, Serialize};
use ui::prelude::*;
use ui::tilo_resource::{Atom, JsonSchema, Refusal, SearchEntry};

use crate::actions::WeatherEffect;
use crate::app::WeatherApp;
pub use crate::model::Weather;
use crate::model::{Place, SavedLocation, WeatherModel};

/// A place to save, as found by the search or named by an agent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PlaceDetails {
    /// City or place name
    pub name: String,
    /// Country name
    pub country: String,
    /// Latitude from -90 to 90
    pub latitude: f64,
    /// Longitude from -180 to 180
    pub longitude: f64,
    /// Administrative region
    #[serde(default)]
    pub region: Option<String>,
    /// ISO country code
    #[serde(default)]
    pub country_code: Option<String>,
    /// IANA timezone; defaults to auto
    #[serde(default)]
    pub timezone: Option<String>,
}

#[resource(kind = "weather.locations", label = "Weather", search = Weather::search)]
impl Weather {
    /// Save a place to follow.
    ///
    /// A place already saved near the same coordinates is kept, not saved
    /// twice.
    #[action]
    pub fn save_location(
        &mut self,
        cx: &mut ActionCx,
        #[param("The place")] place: PlaceDetails,
        #[param("Show it first from now on")] make_default: Option<bool>,
    ) {
        let existing = self
            .places
            .iter()
            .find(|(_, saved)| saved.near(place.latitude, place.longitude))
            .map(|(id, _)| id.clone());
        let id = existing.unwrap_or_else(|| {
            // Places are keyed by uuid, the form the window and its fetches
            // name them by; this one is minted from the action like any id.
            let minted = u128::from_str_radix(cx.id().as_str(), 16).unwrap_or_default();
            let id = Weather::item(uuid::Uuid::from_u128(minted));
            self.places.push(
                id.clone(),
                Place {
                    name: place.name.trim().to_owned(),
                    region: place.region.unwrap_or_default().trim().to_owned(),
                    country: place.country.trim().to_owned(),
                    country_code: Atom(place.country_code.unwrap_or_default().trim().to_owned()),
                    latitude: place.latitude,
                    longitude: place.longitude,
                    timezone: Atom(
                        place
                            .timezone
                            .map_or_else(|| "auto".to_owned(), |zone| zone.trim().to_owned()),
                    ),
                },
            );
            id
        });
        if make_default.unwrap_or(false) {
            self.default_place = Atom(id.as_str().to_owned());
        }
    }

    /// Use as default location.
    #[action(item = places)]
    pub fn use_as_default(&mut self, place: &Id) -> Result<(), Refusal> {
        if !self.places.contains(place) {
            return Err(Refusal::new("there is no such place"));
        }
        self.default_place = Atom(place.as_str().to_owned());
        Ok(())
    }

    /// Move to top.
    #[action(item = places)]
    pub fn move_to_top(&mut self, place: &Id) -> Result<(), Refusal> {
        match self.places.position(place) {
            None => Err(Refusal::new("there is no such place")),
            Some(0) => Err(Refusal::new("this place is already first")),
            Some(_) => {
                self.places.move_to(place, 0);
                Ok(())
            }
        }
    }

    /// Remove location.
    #[action(item = places, destructive)]
    pub fn remove(&mut self, place: &Id) -> Result<(), Refusal> {
        self.places
            .remove(place)
            .ok_or_else(|| Refusal::new("there is no such place"))?;
        if self.default_place.0 == place.as_str() {
            self.default_place = Atom::default();
        }
        Ok(())
    }
}

impl Weather {
    /// Each saved place, found by its name, region and country.
    fn search(&self) -> Vec<SearchEntry> {
        self.model()
            .locations
            .iter()
            .map(|location| SearchEntry {
                anchor: Some(crate::actions::location_resource_id(location.id)),
                title: location.name.clone(),
                text: [location.region.as_str(), location.country.as_str()]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join(", "),
                action: None,
                ..Default::default()
            })
            .collect()
    }

    /// Saving `location` as the default place, as the search does.
    pub fn save(location: &SavedLocation) -> Invoke<Self> {
        Self::SAVE_LOCATION.with(weather::SaveLocation {
            place: PlaceDetails {
                name: location.name.clone(),
                country: location.country.clone(),
                latitude: location.latitude,
                longitude: location.longitude,
                region: Some(location.region.clone()),
                country_code: Some(location.country_code.clone()),
                timezone: Some(location.timezone.clone()),
            },
            make_default: Some(true),
        })
    }
}

/// What a capture shows in place of the network: the bundled forecasts of
/// the sample places, and the units they are shown in.
pub struct SampleForecasts(pub Rc<WeatherModel>);

/// Framework app: the weather widget is the surface over [`Weather`].
pub struct WeatherShell;

impl ResourceApp for WeatherShell {
    type Resource = Weather;
    const ID: &'static str = "so.tilo.weather";
    const TITLE: &'static str = "Weather";
    const ICON: Option<&'static str> = Some("weather");

    fn view(cx: &mut ViewCx<Weather>) -> Element {
        crate::locales::install();
        cx.surface(WeatherApp::new())
    }

    fn scenarios() -> &'static [&'static str] {
        &[
            "onboarding",
            "detail",
            "drag",
            "scrolled",
            "detail_scrolled",
            "collapsed",
            "search",
            "search_loading",
            "search_empty",
            "offline",
            "error",
        ]
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        match scenario {
            Some("detail" | "detail_scrolled") => (560, 720),
            _ => (1280, 800),
        }
    }

    fn fixture(fixture: &mut Fixture<Weather>) {
        fixture.provide(SampleForecasts(Rc::new(crate::sample::sample_model())));
    }

    /// Every scenario but the first-run onboarding shows the sample places,
    /// the first of them the default.
    fn scenario(name: Option<&str>) -> Vec<Invoke<Weather>> {
        if name == Some("onboarding") {
            return Vec::new();
        }
        let sample = crate::sample::sample_model();
        let places = Weather::paths().places;
        sample
            .locations
            .iter()
            .enumerate()
            .filter_map(|(at, location)| {
                Invoke::edit(&places.insert_as(
                    &Weather::item(location.id),
                    at,
                    &Place::of(location),
                ))
                .ok()
            })
            .chain(sample.selected_location().and_then(|location| {
                Invoke::edit(
                    &Weather::paths()
                        .default_place
                        .set(Atom(location.id.to_string())),
                )
                .ok()
            }))
            .collect()
    }
}

impl ResourceSurface<Weather, WeatherEffect> for WeatherApp {
    fn present(&mut self, state: &Rc<Weather>) {
        self.adopt_resource_state(&state.model());
    }

    fn attach(&mut self, cx: SurfaceCx<Weather>) {
        self.attach_framework(&cx);
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_capture(scenario);
    }

    fn effect(
        &mut self,
        effect: WeatherEffect,
        cx: &mut EffectCx<Weather, Self>,
    ) -> Result<Effected, String> {
        self.answer_effect(effect, cx)
    }

    fn effects(&self) -> Vec<EffectStatus> {
        self.effect_statuses()
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        self.location_rows()
    }
}
