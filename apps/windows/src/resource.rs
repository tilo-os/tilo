//! Windows Apps as a [`tilo_resource::Resource`]: route overrides sync in the
//! shared document; the live catalogue, appliance readiness, and pending
//! installs are what the compatibility service reports.
//!
//! A route change is a [`WindowsAction`] through the framework dispatcher.
//! After every change, each device running the app carries the overrides to
//! its own compatibility service (a projection drain), comparing them with
//! the routes the service reports back. Install and appliance lifecycle are
//! effects with no document delta (published on the root and on the live
//! rows the surface reports).

use std::cell::RefCell;
use std::rc::Rc;

use ui::framework::{
    Effected, Fixture, LaunchInput, LiveRow, ResourceApp, ResourceSurface, RunMode, SurfaceCx,
    ViewCx,
};
use ui::tilo_actions::{
    ActionParam, ActionRegistry, ActionSpec, OperationKind, ParamType,
    Resource as PublishedResource,
};
use ui::tilo_resource::{ActionDecl, ActionRef, Doc, Invoke, ModelError, Resource};
use ui::widget::Element;

use crate::actions::{
    self, WINDOWS_ACTION_TYPE, WindowsAction, manager_effect_specs, override_app_resource, register,
};
use crate::app::{SharedProvider, WindowsApp};
use crate::model::{Catalogue, Route};
use crate::provider::SampleProvider;

/// Collaborative route overrides keyed by app id.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowsState {
    pub routes: Vec<(String, Route)>,
}

impl WindowsState {
    /// The override stored for `app_id`, if any.
    pub fn route(&self, app_id: &str) -> Option<Route> {
        self.routes
            .iter()
            .find(|(id, _)| id == app_id)
            .map(|(_, route)| *route)
    }
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
        state
            .routes
            .iter()
            .map(|(app_id, route)| override_app_resource(app_id, *route))
            .collect()
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
                action: None,
            })
            .collect()
    }
}

/// One application's route as the service reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ReportedRoute {
    app_id: String,
    route: Route,
    recommended: Route,
    /// Whether the service accepts an override for it at all.
    overridable: bool,
}

/// The routes the service last reported, shared by the surface (which
/// refreshes them) and the route projection (which compares the document's
/// overrides with them).
#[derive(Clone, Debug, Default)]
pub struct ReportedRoutes(Rc<RefCell<Vec<ReportedRoute>>>);

impl ReportedRoutes {
    /// Take in what `catalogue` reports; whether that changed anything.
    pub(crate) fn report(&self, catalogue: &Catalogue) -> bool {
        let reported: Vec<ReportedRoute> = catalogue
            .apps
            .iter()
            .map(|app| ReportedRoute {
                app_id: app.id.clone(),
                route: app.route,
                recommended: app.recommended_route,
                overridable: app.route_override_available,
            })
            .collect();
        if *self.0.borrow() == reported {
            return false;
        }
        *self.0.borrow_mut() = reported;
        true
    }

    /// The route changes the service still needs for `state`: an override
    /// for each application whose reported route differs from the stored one,
    /// a reset for each that still runs an override the document no longer
    /// holds. Keyed by application and desired override.
    pub(crate) fn pending(&self, state: &WindowsState) -> Vec<(RouteChange, RouteChange)> {
        self.0
            .borrow()
            .iter()
            .filter(|reported| reported.overridable)
            .filter_map(|reported| {
                let desired = state.route(&reported.app_id);
                (desired.unwrap_or(reported.recommended) != reported.route).then(|| {
                    let change = RouteChange {
                        app_id: reported.app_id.clone(),
                        route: desired,
                    };
                    (change.clone(), change)
                })
            })
            .collect()
    }
}

/// One route change carried to the service: an override, or back to the
/// catalogue recommendation (`None`).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct RouteChange {
    app_id: String,
    route: Option<Route>,
}

/// Carry `change` to the service.
fn carry(provider: &SharedProvider, change: &RouteChange) -> Result<(), String> {
    let mut provider = provider.borrow_mut();
    match change.route {
        Some(route) => provider.set_route_override(&change.app_id, route),
        None => provider.reset_route_override(&change.app_id),
    }
}

/// Framework app.
pub struct WindowsShell;

/// Every screenshot scenario, over the sample catalogue.
const SCENARIOS: &[&str] = &[
    "wide",
    "detail",
    "vm-detail",
    "running",
    "error",
    "error-compact",
    "confirm-delete",
];

/// The installer a launch names is installed through the same effect the
/// palette and the install button reach.
const INSTALL: ActionRef<Windows, String> = ActionRef::new("install", &["path"]);

impl ResourceApp for WindowsShell {
    type Resource = Windows;
    const ID: &'static str = "so.tilo.windows";
    const TITLE: &'static str = "Windows Apps";
    const ICON: Option<&'static str> = Some("windows");
    /// An installer to add (`windows setup.exe`).
    const ARGUMENTS: usize = 1;

    fn view(cx: &mut ViewCx<Windows>) -> Element {
        cx.surface(WindowsApp::new())
    }

    /// Opening an installer with Windows Apps installs it.
    fn launch(input: &LaunchInput<'_>, _created: bool) -> Vec<Invoke<Windows>> {
        input
            .argument()
            .map(|path| INSTALL.with(path.to_owned()))
            .into_iter()
            .collect()
    }

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        match scenario.unwrap_or("wide") {
            "detail" | "error-compact" | "vm-detail" | "confirm-delete" => (480, 800),
            "running" | "error" => (1000, 760),
            _ => (1280, 800),
        }
    }

    /// The sample catalogue stands in for the compatibility service; the
    /// error scenarios show its first application's setup failed.
    fn fixture(fixture: &mut Fixture<Windows>) {
        let mut sample = SampleProvider::new();
        if matches!(fixture.scenario(), Some("error" | "error-compact")) {
            sample.seed_setup_failure();
        }
        fixture.provide(sample);
    }
}

impl ResourceSurface<Windows> for WindowsApp {
    /// The routes the window shows are the ones the service reports; the
    /// stored overrides reach the service through the route projection.
    fn present(&mut self, _state: &Rc<WindowsState>) {}

    fn attach(&mut self, cx: SurfaceCx<Windows>) {
        match cx.mode() {
            RunMode::Live => self.use_provider(WindowsApp::live_provider()),
            RunMode::Capture => {
                if let Some(sample) = cx.fixture::<SampleProvider>() {
                    self.use_provider(Box::new((*sample).clone()));
                }
            }
            // A past version reaches no service.
            RunMode::Past => {}
        }
        let reported = self.reported_routes.clone();
        let provider = self.provider();
        let notice = Rc::clone(&self.route_notice);
        let projection = cx.drain(
            move |state: &WindowsState| reported.pending(state),
            move |change: RouteChange, _| {
                if let Err(error) = carry(&provider, &change) {
                    *notice.borrow_mut() = Some(error.clone());
                    return Err(error);
                }
                Ok(Effected::done())
            },
        );
        self.attach_framework(cx.dispatcher(), projection);
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario.unwrap_or("wide"));
    }

    /// Route edits for the application the window shows, beside the
    /// `set-route` declaration that names one by id.
    fn bound_declarations(&self) -> Vec<ActionDecl<Windows>> {
        let Some(app) = self
            .selected_app()
            .filter(|app| app.route_override_available)
        else {
            return Vec::new();
        };
        let mut decls: Vec<ActionDecl<Windows>> = Route::ALL
            .into_iter()
            .map(|route| {
                let app_id = app.id.clone();
                ActionDecl::new(
                    route_decl_id(route),
                    format!("Use {} for {}", route.label(), app.name),
                    Vec::new(),
                    move |_, _| {
                        Ok(WindowsAction::SetRoute {
                            app_id: app_id.clone(),
                            route,
                        })
                    },
                )
            })
            .collect();
        if app.uses_override() {
            let app_id = app.id.clone();
            decls.push(ActionDecl::new(
                "use-recommended-route",
                format!("Use catalogue recommendation for {}", app.name),
                Vec::new(),
                move |_, _| {
                    Ok(WindowsAction::ResetRoute {
                        app_id: app_id.clone(),
                    })
                },
            ));
        }
        decls
    }

    /// The service's catalogue, appliance and pending install, merged by id
    /// with the overrides the document holds.
    fn live_rows(&self) -> Vec<LiveRow> {
        actions::live_rows(&self.catalogue)
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        actions::live_rows(&self.catalogue)
            .into_iter()
            .find(|row| row.id() == id)
    }
}

/// The id of the bound edit that routes the shown application to `route`.
fn route_decl_id(route: Route) -> &'static str {
    match route {
        Route::Wine => "use-wine-route",
        Route::Proton => "use-proton-route",
        Route::Vm => "use-vm-route",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::CompatibilityProvider;
    use ui::prelude::TestApp;
    use ui::tilo_actions::Action;

    fn reported(app_id: &str, route: Route, recommended: Route) -> Catalogue {
        let mut catalogue = SampleProvider::new().catalogue();
        catalogue.apps.retain(|app| app.id == app_id);
        let app = &mut catalogue.apps[0];
        app.route = route;
        app.recommended_route = recommended;
        app.route_override_available = true;
        catalogue
    }

    /// A stored override the service does not run yet is pending, and so is
    /// an override the service still runs after the document cleared it —
    /// the reset of the last override reaches the service too.
    #[test]
    fn the_route_projection_carries_overrides_and_resets_to_the_service() {
        let routes = ReportedRoutes::default();
        assert!(routes.report(&reported("adobe-reader", Route::Wine, Route::Wine)));
        assert!(
            !routes.report(&reported("adobe-reader", Route::Wine, Route::Wine)),
            "the same report changes nothing"
        );
        let overridden = WindowsState {
            routes: vec![("adobe-reader".into(), Route::Proton)],
        };
        let pending = routes.pending(&overridden);
        assert_eq!(
            pending
                .iter()
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>(),
            [RouteChange {
                app_id: "adobe-reader".into(),
                route: Some(Route::Proton),
            }]
        );
        assert!(routes.pending(&WindowsState::default()).is_empty());

        routes.report(&reported("adobe-reader", Route::Proton, Route::Wine));
        assert!(
            routes.pending(&overridden).is_empty(),
            "the service runs it"
        );
        let reset = routes.pending(&WindowsState::default());
        assert_eq!(
            reset.iter().map(|(key, _)| key.clone()).collect::<Vec<_>>(),
            [RouteChange {
                app_id: "adobe-reader".into(),
                route: None,
            }]
        );

        let provider: SharedProvider = Rc::new(RefCell::new(Box::new(SampleProvider::new())));
        carry(&provider, &reset[0].1).unwrap();
        let catalogue = provider.borrow_mut().catalogue();
        let reader = catalogue.app("adobe-reader").unwrap();
        assert_eq!(reader.route, reader.recommended_route);
    }

    /// The published tree carries the stored overrides as rows with their
    /// route edits, the root offers install and the route declarations, and
    /// nothing else publishes a document edit.
    #[test]
    fn the_resource_publishes_overrides_and_the_install_door() {
        let mut doc = ui::automerge::AutoCommit::new();
        WindowsAction::SetRoute {
            app_id: "adobe-reader".into(),
            route: Route::Proton,
        }
        .apply(&mut doc)
        .unwrap();
        let state = Windows::read(&doc).unwrap();
        assert_eq!(state.routes, vec![("adobe-reader".into(), Route::Proton)]);
        let published =
            ui::tilo_resource::publish::<Windows>("windows.manager", "Windows manager", &state);
        for id in ["set-route", "reset-route", "install"] {
            assert!(
                published.actions.iter().any(|action| action.id == id),
                "{id} must be on the framework resource"
            );
        }
        let reader = published
            .children
            .iter()
            .find(|child| child.id.as_str() == "windows.app:adobe-reader")
            .expect("the override row");
        assert!(
            reader
                .actions
                .iter()
                .any(|action| action.id == "route-proton")
        );
        assert!(
            reader
                .actions
                .iter()
                .any(|action| action.id == "reset-route")
        );
    }

    /// Opening an installer with the app installs it through the install
    /// effect; a plain start takes no action.
    #[test]
    fn a_launch_naming_an_installer_invokes_install() {
        let arguments = ["/home/you/Downloads/setup.exe".to_owned()];
        let launch = WindowsShell::launch(&LaunchInput::new(&arguments, None), false);
        assert_eq!(launch.len(), 1);
        assert_eq!(launch[0].id, "install");
        assert_eq!(launch[0].arguments["path"], "/home/you/Downloads/setup.exe");
        assert!(WindowsShell::launch(&LaunchInput::new(&[], None), false).is_empty());
    }

    /// Every screenshot is the sample catalogue with its scenario's view
    /// state, at the scenario's viewport; the live rows the window shows
    /// carry effects, merged under the manager with the stored overrides.
    #[test]
    fn captures_show_the_sample_catalogue_and_publish_its_rows() {
        let mut detail = TestApp::<WindowsShell>::capture(Some("confirm-delete")).unwrap();
        assert_eq!(detail.harness().inspection().viewport, [480, 800]);
        let frame = detail.harness().inspection_json().unwrap();
        assert!(frame.contains("windows.delete-confirm"), "{frame}");

        let mut wide = TestApp::<WindowsShell>::capture(Some("wide")).unwrap();
        assert_eq!(wide.harness().inspection().viewport, [1280, 800]);
        let surface = wide.agent_surface();
        let rows = surface.as_array().unwrap();
        assert!(rows.iter().any(|row| {
            row["resource"] == "windows.app:adobe-reader" && row["action"] == "launch"
        }));
        assert!(
            rows.iter()
                .any(|row| { row["resource"] == "windows.appliance" && row["action"] == "start" })
        );
        assert!(
            rows.iter()
                .any(|row| row["resource"] == "windows.manager"
                    && row["action"] == "use-proton-route"),
            "the shown application's route edits are bound on the manager"
        );
    }
}
