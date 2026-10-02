//! Windows Apps as a [`tilo_resource::Resource`]: route overrides sync in the
//! shared document; the live catalogue, appliance readiness, and pending
//! installs stay host projections.
//!
//! Install and appliance lifecycle are Effects with no document delta
//! (published via [`Resource::local_operations`] / the appliance child).
//! Per-app launch / repair Effects ride live catalogue extras — never a
//! second hand-built `windows.manager` root beside [`tilo_resource::publish`].

use ui::automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{
    ActionParam, ActionRegistry, ActionSpec, EncodedAction, OperationKind, ParamType,
    Resource as PublishedResource,
};
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource};
use ui::widget::Element;

use crate::actions::{
    self, appliance_publish_child, live_catalogue_extras, manager_effect_specs,
    override_app_resource, register, WindowsAction, MANAGER_RESOURCE_ID, WINDOWS_ACTION_TYPE,
};
use crate::app::WindowsApp;
use crate::model::Route;

/// Collaborative route overrides keyed by app id.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowsState {
    pub routes: Vec<(String, Route)>,
}

/// The Windows compatibility manager resource.
pub struct Windows;

impl Resource for Windows {
    const KIND: &'static str = "windows.manager";
    const LABEL: &'static str = "Windows manager";
    const ACTION_TYPE: &'static str = WINDOWS_ACTION_TYPE;

    type State = WindowsState;
    type Action = WindowsAction;

    fn register(registry: &mut ActionRegistry) {
        register(registry);
    }

    fn read(doc: &Doc) -> Result<WindowsState, ModelError> {
        let mut routes = Vec::new();
        if let Ok(Some((automerge::Value::Object(automerge::ObjType::Map), map))) =
            automerge::ReadDoc::get(doc, automerge::ROOT, "windows-compat")
        {
            for app_id in automerge::ReadDoc::keys(doc, &map) {
                if let Some(route) = actions::route_override(doc, &app_id) {
                    routes.push((app_id, route));
                }
            }
        }
        routes.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(WindowsState { routes })
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![
            ActionDecl::new(
                "set-route",
                "Set launch route",
                vec![
                    ActionParam::required("app_id", ParamType::Text, "Catalogue application id"),
                    ActionParam::required("route", ParamType::Text, "Wine | Proton | Windows VM")
                        .with_choices(["Wine", "Proton", "Windows VM"]),
                ],
                |args, _| {
                    let app_id = args
                        .text("app_id")
                        .ok_or_else(|| "missing app_id".to_owned())?
                        .to_owned();
                    let route = match args.text("route").unwrap_or_default() {
                        "Wine" => Route::Wine,
                        "Proton" => Route::Proton,
                        "Windows VM" => Route::Vm,
                        other => {
                            return Err(format!("unknown route {other}"));
                        }
                    };
                    Ok(WindowsAction::SetRoute { app_id, route })
                },
            )
            .describe("Set the shared launch-route override for one application."),
            ActionDecl::new(
                "reset-route",
                "Use catalogue recommendation",
                vec![ActionParam::required(
                    "app_id",
                    ParamType::Text,
                    "Catalogue application id",
                )],
                |args, _| {
                    let app_id = args
                        .text("app_id")
                        .ok_or_else(|| "missing app_id".to_owned())?
                        .to_owned();
                    Ok(WindowsAction::ResetRoute { app_id })
                },
            )
            .describe("Clear the shared launch-route override for one application."),
        ]
    }

    fn children(state: &WindowsState) -> Vec<PublishedResource> {
        let mut children = vec![appliance_publish_child()];
        for (app_id, route) in &state.routes {
            children.push(override_app_resource(app_id, *route));
        }
        children
    }

    fn local_operations() -> Vec<ActionSpec> {
        manager_effect_specs()
            .into_iter()
            .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
            .collect()
    }

    fn search(state: &WindowsState) -> Vec<ui::tilo_resource::SearchEntry> {
        state
            .routes
            .iter()
            .map(|(app_id, route)| ui::tilo_resource::SearchEntry {
                anchor: Some(format!("windows.app:{app_id}")),
                title: app_id.clone(),
                text: route.label().to_owned(),
            })
            .collect()
    }
}

/// Hosted-only extras that are not document state: pending install confirmation
/// and live catalogue app rows (launch / repair Effects only). Route Document
/// ops and appliance Effects stay on [`tilo_resource::publish`] — never a
/// second manager root.
pub(crate) fn live_extras(app: &WindowsApp) -> Vec<PublishedResource> {
    live_catalogue_extras(&app.catalogue)
        .into_iter()
        .filter(|resource| resource.kind != "windows.appliance")
        .map(strip_document_actions)
        .collect()
}

fn strip_document_actions(mut resource: PublishedResource) -> PublishedResource {
    resource
        .actions
        .retain(|action| action.kind != OperationKind::Document);
    resource.children = resource
        .children
        .into_iter()
        .map(strip_document_actions)
        .collect();
    resource
}

/// Framework app.
pub struct WindowsShell;

impl ResourceApp for WindowsShell {
    type Resource = Windows;
    const ID: &'static str = "so.tilo.windows";
    const TITLE: &'static str = "Windows Apps";
    const ICON: Option<&'static str> = Some("windows");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<Windows>) -> Element {
        cx.surface(WindowsApp::new_live())
    }
}

impl ResourceSurface<Windows> for WindowsApp {
    fn present(&mut self, state: &std::rc::Rc<WindowsState>) {
        // Adopt collaborative route overrides only — never clear a local
        // catalogue / provider projection that has not been published yet.
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Windows>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    pub(crate) local: ui::ActionHost,
    pub(crate) framework: Option<Dispatcher<Windows>>,
    /// Set by [`Self::attach`]; tests can flip it without a real dispatcher.
    pub(crate) hosted: bool,
    pub(crate) last: Option<EncodedAction>,
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
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Windows>) {
        self.framework = Some(dispatcher);
        self.hosted = true;
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.hosted
    }

    pub(crate) fn dispatch(&mut self, doc: &mut AutoCommit, action: &WindowsAction) {
        let context =
            ui::tilo_actions::ActionContext::for_action("so.tilo.windows", "Windows Apps", action);
        match &self.framework {
            Some(dispatcher) => {
                self.last = EncodedAction::encode(Windows::ACTION_TYPE, action).ok();
                let _ = dispatcher.apply_now_with(doc, action, context);
            }
            None => self
                .local
                .dispatch_with_context(doc, Windows::ACTION_TYPE, action, context),
        }
    }

    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn last_dispatched(&self) -> Option<&EncodedAction> {
        if self.framework.is_some() {
            self.last.as_ref()
        } else {
            self.local.last_dispatched()
        }
    }

    /// Test-only: mark the sink hosted without constructing a framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.hosted = true;
    }
}

/// Standalone / `--shots` / unit-test tree via [`tilo_resource::publish`].
pub fn provide_resources(state: &WindowsState) -> Vec<PublishedResource> {
    vec![ui::tilo_resource::publish::<Windows>(
        MANAGER_RESOURCE_ID,
        "Windows manager",
        state,
    )]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::tilo_actions::Action;
    use ui::widget::Widget;

    #[allow(
        clippy::too_many_lines,
        reason = "one publish/hosted extras check covers manager door and live overlay"
    )]
    #[test]
    fn resource_publish_owns_manager_door_and_hosted_keeps_only_live_extras() {
        let mut doc = AutoCommit::new();
        WindowsAction::SetRoute {
            app_id: "adobe-reader".into(),
            route: Route::Proton,
        }
        .apply(&mut doc)
        .unwrap();
        doc.commit();

        let state = Windows::read(&doc).expect("seeded windows state");
        let published =
            ui::tilo_resource::publish::<Windows>(MANAGER_RESOURCE_ID, "Windows manager", &state);
        assert_eq!(published.kind, "windows.manager");
        for id in ["set-route", "reset-route", "install"] {
            assert!(
                published.actions.iter().any(|action| action.id == id),
                "{id} must be on the framework resource"
            );
        }
        assert!(
            published
                .children
                .iter()
                .any(|child| child.id.as_str() == "windows.appliance"
                    && child.actions.iter().any(|action| action.id == "start")),
            "appliance child keeps Start Windows on the published door"
        );
        assert!(
            published.children.iter().any(|child| {
                child.id.as_str() == "windows.app:adobe-reader"
                    && child
                        .actions
                        .iter()
                        .any(|action| action.id == "route-proton")
                    && child
                        .actions
                        .iter()
                        .any(|action| action.id == "reset-route")
            }),
            "override children keep bound route shortcuts"
        );
        assert_eq!(state.routes, vec![("adobe-reader".into(), Route::Proton)]);

        let mut app = WindowsApp::new_offline();
        let mut seeded = doc.clone();
        app.sync(&mut seeded, &ui::automerge::ROOT);
        assert!(!app.is_hosted(), "standalone stays on the local host");
        let standalone = app.publish_resources(&seeded);
        assert_eq!(standalone.len(), 1);
        assert_eq!(standalone[0].kind, "windows.manager");
        assert!(
            standalone[0]
                .children
                .iter()
                .any(|child| child.kind == "windows.compat-app"),
            "standalone overlays live catalogue apps"
        );
        assert!(
            standalone[0].children.iter().any(|child| {
                child.id.as_str() == "windows.app:adobe-reader"
                    && child
                        .actions
                        .iter()
                        .any(|action| action.id == "reset-route")
            }),
            "standalone keeps the document route override on the catalogue app"
        );
        assert!(
            standalone[0]
                .children
                .iter()
                .any(|child| child.id.as_str() == "windows.appliance"),
            "standalone overlays the live appliance child"
        );

        app.mark_hosted_for_test();
        assert!(app.is_hosted());
        assert!(ui::resources::resource(&app, &seeded, actions::MANAGER_RESOURCE_ID).is_none());
        assert!(
            ui::resources::resource(&app, &seeded, "windows.app:adobe-reader").is_some(),
            "a hosted route override stays the published row"
        );
        let extras = ui::resources::PublishesResources::resources(&app, &seeded);
        assert!(
            extras
                .iter()
                .all(|resource| resource.kind != "windows.manager"),
            "hosted must not republish windows.manager beside ResourceRoot: {:?}",
            extras.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );
        assert!(
            extras
                .iter()
                .any(|resource| resource.kind == "windows.compat-app"),
            "hosted still publishes live catalogue apps: {:?}",
            extras.iter().map(|r| r.kind.as_str()).collect::<Vec<_>>()
        );
        assert!(
            extras
                .iter()
                .all(|resource| resource.kind != "windows.appliance"),
            "hosted must not duplicate the published appliance child: {:?}",
            extras.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );

        let door = provide_resources(&state);
        assert_eq!(door[0].kind, "windows.manager");
        assert!(door[0]
            .children
            .iter()
            .any(|child| child.id.as_str() == "windows.appliance"));

        let mut local = AutoCommit::new();
        let mut standalone = WindowsApp::new_offline();
        standalone.commit_route(
            &mut local,
            &WindowsAction::SetRoute {
                app_id: "cyberpunk-2077".into(),
                route: Route::Wine,
            },
        );
        assert!(
            standalone.last_document_action().is_some(),
            "ActionSink must record document actions on the local host"
        );
        assert_eq!(
            actions::route_override(&local, "cyberpunk-2077"),
            Some(Route::Wine)
        );

        // Empty present must not wipe a local override that has not been published.
        standalone.project_shared_route_policy(&local);
        standalone.refresh();
        let before = standalone
            .catalogue
            .app("cyberpunk-2077")
            .map(|app| app.route);
        assert_eq!(before, Some(Route::Wine));
        standalone.adopt_resource_state(&WindowsState::default());
        assert_eq!(
            standalone
                .catalogue
                .app("cyberpunk-2077")
                .map(|app| app.route),
            before,
            "adopting empty view prefs must not clear unpublished local overrides"
        );
    }
}
