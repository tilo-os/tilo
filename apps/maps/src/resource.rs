//! Maps as a [`tilo_resource::Resource`]: one action vocabulary, one publish
//! path for the workspace document.

use automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{
    ActionContract, ActionSpec, Availability, DiscoveryScope, EncodedAction, ExecutionMode,
    OperationKind, PermissionRequirement, ResultSpec, RetryPolicy,
};
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource};
use ui::widget::Element;

use crate::actions::{
    MAPS_ACTION_TYPE, MapsAction, document_bound_actions, document_children, register,
    search_maps_spec,
};
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
        // Named place/region/route payloads are state-bound on children and
        // [`bound_actions`]. Stub declarations here would advertise unbound
        // ids beside the published tree.
        Vec::new()
    }

    fn bound_actions(state: &MapModel) -> Vec<ActionSpec> {
        document_bound_actions(state)
    }

    fn children(state: &MapModel) -> Vec<tilo_actions::Resource> {
        document_children(state)
    }

    fn local_operations() -> Vec<ActionSpec> {
        // Search is a local brokered View. Locate / traffic are Effects/Views
        // with conservative baseline availability; standalone surfaces overlay
        // live contracts via [`MapsApp::decorate_live_resource`].
        let mut ops = vec![search_maps_spec()];
        if let Some(spec) = locate_spec(Availability::Available) {
            ops.push(spec);
        }
        if let Some(spec) = toggle_traffic_spec(false) {
            ops.push(spec);
        }
        ops
    }
}

/// Use-my-location Effect for the workspace door.
pub(crate) fn locate_spec(availability: Availability) -> Option<ActionSpec> {
    ActionSpec::operation(
        "locate",
        "Use my location",
        OperationKind::Effect,
        "maps.locate",
        &"",
    )
    .ok()
    .map(|spec| {
        spec.with_contract(ActionContract {
            availability,
            permission: PermissionRequirement::Read,
            retry: RetryPolicy::AtMostOnce,
            execution: ExecutionMode::Background { cancellable: false },
            discovery: DiscoveryScope::Launchable,
            result: ResultSpec::Job,
            ..ActionContract::default()
        })
    })
}

/// Toggle traffic overlay View for the workspace door.
pub(crate) fn toggle_traffic_spec(traffic_enabled: bool) -> Option<ActionSpec> {
    ActionSpec::operation(
        "toggle-traffic",
        if traffic_enabled {
            "Hide traffic"
        } else {
            "Show traffic"
        },
        OperationKind::View,
        "maps.toggle-traffic",
        &"",
    )
    .ok()
    .map(|spec| spec.with_contract(ActionContract::view()))
}

/// Hosted-only extras that are not document state: ranked search hits and
/// nearby POIs. The workspace door stays on [`tilo_resource::publish`].
pub(crate) fn live_extras(app: &MapsApp) -> Vec<tilo_actions::Resource> {
    app.live_search_and_poi_resources()
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
    hosted: bool,
    last: Option<EncodedAction>,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
            hosted: false,
            last: None,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Maps>) {
        self.framework = Some(dispatcher);
        self.hosted = true;
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.hosted
    }

    /// Test-only: mark hosted without a real framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.hosted = true;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::provide_resources;
    use crate::model::{Category, LatLon, Place, Route, TravelMode};
    use ui::widget::Widget;

    fn full_model() -> MapModel {
        let mut m = MapModel::default();
        let mut p = Place::new(
            "marienplatz",
            "Marienplatz",
            Category::Landmark,
            "",
            LatLon::new(48.137, 11.575),
        );
        p.saved = true;
        m.places.push(p);
        m.dest_name = "Marienplatz".to_string();
        m.route = Some(Route {
            mode: TravelMode::Car,
            distance_m: 1200.0,
            duration_s: 300.0,
            geometry: vec![LatLon::new(48.13, 11.57), LatLon::new(48.14, 11.58)],
            steps: Vec::new(),
            from: LatLon::new(48.13, 11.57),
            to: LatLon::new(48.14, 11.58),
        });
        m
    }

    #[test]
    fn resource_publish_owns_workspace_door_and_hosted_keeps_only_live_extras() {
        let mut doc = AutoCommit::new();
        MapsDoc::save(&mut doc, &full_model());
        let state = Maps::read(&doc).expect("seeded maps state");
        let published = ui::tilo_resource::publish::<Maps>("maps.workspace", "Maps", &state);
        assert_eq!(published.kind, "maps.workspace");
        assert!(
            published.actions.iter().any(|action| action.id == "search"),
            "search View must be on the framework resource"
        );
        assert!(
            published.actions.iter().any(|action| action.id == "locate"),
            "locate Effect must be on the framework resource"
        );
        let place = published
            .children
            .iter()
            .find(|child| child.kind == "maps.place")
            .expect("saved place child");
        assert!(
            place.actions.iter().any(|action| action.id == "unsave"),
            "unsave must ride the place child"
        );
        assert!(
            place.actions.iter().any(|action| action.id == "open"),
            "open View must ride the place child for hosted publish"
        );
        assert!(
            published
                .children
                .iter()
                .any(|child| child.kind == "maps.route"),
            "active route child must be on the framework resource"
        );

        let mut app = MapsApp::offline();
        let mut seeded = doc.clone();
        app.sync(&mut seeded, &ui::automerge::ROOT);
        assert!(!app.is_hosted(), "standalone stays on the local host");
        let standalone = app.publish_resources(&seeded);
        assert_eq!(standalone[0].kind, "maps.workspace");
        assert!(
            standalone[0]
                .children
                .iter()
                .any(|child| child.kind == "maps.place"),
            "standalone publishes the document tree"
        );

        // Live search hit is not document state — hosted may still publish it.
        app.seed_search_results_for_test(vec![Place::new(
            "live-hit",
            "Live Hit",
            Category::Cafe,
            "",
            LatLon::new(48.1, 11.5),
        )]);
        app.mark_hosted_for_test();
        assert!(app.is_hosted());
        let extras = ui::resources::PublishesResources::resources(&app, &seeded);
        assert!(
            extras
                .iter()
                .all(|resource| resource.kind != "maps.workspace"),
            "hosted must not republish maps.workspace beside ResourceRoot: {:?}",
            extras.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );
        assert!(
            extras
                .iter()
                .any(|resource| resource.id.as_str() == "maps.search-result:live-hit"),
            "hosted still publishes live search hits: {:?}",
            extras.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );

        // Document door still resolves through provide_resources / publish.
        let door = provide_resources(&seeded);
        assert_eq!(door[0].kind, "maps.workspace");
    }

    #[test]
    fn a_live_map_stays_empty_until_a_map_action() {
        let mut app = MapsApp::new();
        let mut doc = automerge::AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        assert!(MapsDoc::load(&doc).is_none());
    }
}
