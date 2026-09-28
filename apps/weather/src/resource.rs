//! Weather as a [`tilo_resource::Resource`]: one action vocabulary, one publish
//! path for saved locations.

use automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{WEATHER_ACTION_TYPE, WeatherAction, register};
use crate::app::WeatherApp;
use crate::doc::WeatherDoc;
use crate::model::WeatherModel;

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
        // Location open/refresh stay View/Effect specs on the published tree.
        Vec::new()
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
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Weather>) {
        self.framework = Some(dispatcher);
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
}
