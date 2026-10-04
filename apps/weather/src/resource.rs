//! Weather as a [`tilo_resource::Resource`]: the saved places, their order
//! and the default place, with one action vocabulary and one publication.

use std::rc::Rc;

use ui::prelude::*;
use ui::tilo_actions::{ActionParam, ParamType};
use ui::tilo_resource::{Doc, ModelError, SearchEntry};

use crate::actions::{WEATHER_ACTION_TYPE, WeatherAction, WeatherEffect, document_children};
use crate::app::WeatherApp;
use crate::doc::WeatherDoc;
use crate::model::{SavedLocation, WeatherModel};

/// The weather locations resource.
pub struct Weather;

impl Resource for Weather {
    const KIND: &'static str = "weather.locations";
    const LABEL: &'static str = "Weather";
    const ACTION_TYPE: &'static str = WEATHER_ACTION_TYPE;

    type State = WeatherModel;
    type Action = WeatherAction;

    fn read(doc: &Doc) -> Result<WeatherModel, ModelError> {
        Ok(WeatherDoc::read_model(doc))
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![ActionDecl::new(
            "add-saved-location",
            "Save location",
            vec![
                ActionParam::required("name", ParamType::Text, "City or place name"),
                ActionParam::required("country", ParamType::Text, "Country name"),
                ActionParam::required("latitude", ParamType::Number, "Latitude from -90 to 90"),
                ActionParam::required("longitude", ParamType::Number, "Longitude from -180 to 180"),
                ActionParam::optional("region", ParamType::Text, "Administrative region"),
                ActionParam::optional("country_code", ParamType::Text, "ISO country code"),
                ActionParam::optional(
                    "timezone",
                    ParamType::Text,
                    "IANA timezone; defaults to auto",
                ),
            ],
            |args, _| {
                let name = args
                    .text("name")
                    .map(str::trim)
                    .filter(|value| !value.is_empty());
                let country = args
                    .text("country")
                    .map(str::trim)
                    .filter(|value| !value.is_empty());
                let latitude = args
                    .number("latitude")
                    .filter(|value| (-90.0..=90.0).contains(value));
                let longitude = args
                    .number("longitude")
                    .filter(|value| (-180.0..=180.0).contains(value));
                match (name, country, latitude, longitude) {
                    (Some(name), Some(country), Some(latitude), Some(longitude)) => {
                        Ok(WeatherAction::AddLocation {
                            location: SavedLocation::new(
                                name,
                                args.text("region").unwrap_or_default().trim(),
                                country,
                                args.text("country_code").unwrap_or_default().trim(),
                                latitude,
                                longitude,
                                args.text("timezone").unwrap_or("auto").trim(),
                            ),
                        })
                    }
                    _ => Err("name, country, and valid latitude/longitude are required".into()),
                }
            },
        )]
    }

    fn children(state: &WeatherModel) -> Vec<tilo_actions::Resource> {
        document_children(state)
    }

    fn search(state: &WeatherModel) -> Vec<SearchEntry> {
        state
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
            })
            .collect()
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
        sample
            .locations
            .iter()
            .map(|location| WeatherAction::AddLocation {
                location: location.clone(),
            })
            .chain(
                sample
                    .selected_location()
                    .map(|location| crate::actions::select_location_action(location.id)),
            )
            .filter_map(|action| Invoke::apply(&action).ok())
            .collect()
    }
}

impl ResourceSurface<Weather, WeatherEffect> for WeatherApp {
    fn present(&mut self, state: &Rc<WeatherModel>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Weather>) {
        self.attach_framework(&cx);
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario.unwrap_or_default());
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
