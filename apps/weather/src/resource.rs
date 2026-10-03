//! Weather as a [`tilo_resource::Resource`]: one action vocabulary, one publish
//! path for saved locations.

use automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{ActionParam, ActionSpec, ParamType};
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{
    WEATHER_ACTION_TYPE, WeatherAction, document_children, register, root_local_operations,
};
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

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

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

    fn local_operations() -> Vec<ActionSpec> {
        // Network refresh is an Effect with no document delta. Standalone hosts
        // overlay offline / empty-library availability; the live widget refuses
        // when fetches are disabled.
        root_local_operations()
    }

    fn search(state: &WeatherModel) -> Vec<SearchEntry> {
        state
            .locations
            .iter()
            .map(|location| SearchEntry {
                anchor: Some(crate::actions::location_resource_id(location.id)),
                title: location.name.clone(),
                text: location.region.clone(),
            })
            .collect()
    }
}

/// Framework app: the weather widget is the surface over [`Weather`].
pub struct WeatherShell;

impl ResourceApp for WeatherShell {
    type Resource = Weather;
    const ID: &'static str = "so.tilo.weather";
    const TITLE: &'static str = "Weather";
    const ICON: Option<&'static str> = Some("weather");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<Weather>) -> Element {
        cx.surface(WeatherApp::live())
    }
}

impl ResourceSurface<Weather> for WeatherApp {
    fn present(&mut self, state: &std::rc::Rc<WeatherModel>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Weather>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<Weather>>,
    /// Set by [`Self::attach`]; tests can flip it without a real dispatcher.
    hosted: bool,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
            hosted: false,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Weather>) {
        self.framework = Some(dispatcher);
        self.hosted = true;
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.hosted
    }

    pub(crate) fn dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &WeatherAction,
        context: ui::tilo_actions::ActionContext,
    ) {
        match &self.framework {
            Some(dispatcher) => {
                let _ = dispatcher.apply_now_with(doc, action, context);
            }
            None => self
                .local
                .dispatch_with_context(doc, type_id, action, context),
        }
    }

    pub(crate) fn take_committed(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.local.take_committed()
    }

    /// Test-only: mark the sink hosted without constructing a framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.hosted = true;
    }
}
