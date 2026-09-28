//! Maps as a [`tilo_resource::Resource`]: one action vocabulary, one publish
//! path for the workspace document.

use automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::EncodedAction;
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource, SearchEntry};
use ui::widget::Element;

use crate::actions::{MAPS_ACTION_TYPE, MapsAction, register};
use crate::app::MapsApp;
use crate::doc::MapsDoc;
use crate::model::MapModel;

/// The maps workspace resource.
pub struct Maps;

impl Resource for Maps {
    const KIND: &'static str = "maps.workspace";
    const LABEL: &'static str = "Maps";
    const ACTION_TYPE: &'static str = MAPS_ACTION_TYPE;

    type State = MapModel;
    type Action = MapsAction;

    fn register(registry: &mut tilo_actions::ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<MapModel, ModelError> {
        Ok(MapsDoc::load(doc).unwrap_or_default())
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        // Search / route / region affordances stay on the published tree.
        Vec::new()
    }

    fn search(state: &MapModel) -> Vec<SearchEntry> {
        let places = state.places.iter().map(|place| SearchEntry {
            anchor: Some(format!("maps.place:{}", place.id)),
            title: place.name.clone(),
            text: place.address.clone(),
        });
        let regions = state.regions.iter().map(|region| SearchEntry {
            anchor: Some(format!("maps.region:{}", region.id)),
            title: region.name.clone(),
            text: String::new(),
        });
        places.chain(regions).collect()
    }
}

/// Framework app: the maps widget is the surface over [`Maps`].
pub struct MapsShell;

impl ResourceApp for MapsShell {
    type Resource = Maps;
    const ID: &'static str = "so.tilo.maps";
    const TITLE: &'static str = "Maps";
    const ICON: Option<&'static str> = Some("maps");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<Maps>) -> Element {
        cx.surface(MapsApp::new())
    }
}

impl ResourceSurface<Maps> for MapsApp {
    fn present(&mut self, state: &std::rc::Rc<MapModel>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Maps>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<Maps>>,
    last: Option<EncodedAction>,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
            last: None,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Maps>) {
        self.framework = Some(dispatcher);
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.framework.is_some()
    }

    pub(crate) fn dispatch_with_context(
        &mut self,
        doc: &mut AutoCommit,
        type_id: &str,
        action: &MapsAction,
        context: ui::tilo_actions::ActionContext,
    ) {
        match &self.framework {
            Some(dispatcher) => {
                self.last = Some(crate::actions::encode(action));
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

    pub(crate) fn last_dispatched(&self) -> Option<&EncodedAction> {
        if self.framework.is_some() {
            self.last.as_ref()
        } else {
            self.local.last_dispatched()
        }
    }
}
