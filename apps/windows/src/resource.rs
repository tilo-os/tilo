//! Windows Apps as a resource: the launch routes people agreed on are the
//! [`Windows`] model, edited at their paths; the live catalogue, appliance
//! readiness and pending installs are what the compatibility service
//! reports.
//!
//! After every change, each device running the app carries the overrides to
//! its own compatibility service (a projection drain), comparing them with
//! the routes the service reports back. Install and appliance lifecycle are
//! effects with no document delta (published on the root and on the live
//! rows the surface reports).

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use ui::framework::{
    EffectAnswer, EffectCx, EffectStatus, Effected, Fixture, LaunchInput, LiveRow, ResourceApp,
    ResourceSurface, RunMode, SurfaceCx, ViewCx,
};
use ui::tilo_resource::{self, Invoke, Model, resource};
use ui::widget::Element;

use crate::actions::{self, WindowsEffect};
use crate::app::{SharedProvider, WindowsApp};
use crate::model::{Catalogue, Route};
use crate::provider::SampleProvider;

/// The launch routes the people sharing this manager agreed on. An
/// application without an override runs the catalogue's recommendation.
/// Each is edited at its path:
/// `Windows::paths().routes.key(id).set(Route::Proton)` overrides one,
/// `Windows::paths().routes.remove(id)` goes back to the recommendation.
#[derive(Model, Default, Debug, Clone, PartialEq, Eq)]
pub struct Windows {
    /// Launch routes
    ///
    /// The route each overridden application runs with, by its id.
    pub routes: BTreeMap<String, Route>,
}

impl Windows {
    /// The override stored for `app_id`, if any.
    pub fn route(&self, app_id: &str) -> Option<Route> {
        self.routes.get(app_id).copied()
    }
}

#[resource(kind = "windows.manager", label = "Windows manager")]
impl Windows {}

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
    pub(crate) fn pending(&self, state: &Windows) -> Vec<(RouteChange, RouteChange)> {
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
    "appliance",
    "appliance-setup",
    "pending-install",
];

impl ResourceApp for WindowsShell {
    type Resource = Windows;
    const ID: &'static str = "so.tilo.windows";
    const TITLE: &'static str = "Windows Apps";
    const ICON: Option<&'static str> = Some("windows");
    /// An installer to add (`windows setup.exe`).
    const ARGUMENTS: usize = 1;

    fn view(cx: &mut ViewCx<Windows>) -> Element {
        let app = WindowsApp::new();
        // A search hit, or an agent's `open`, shows the application it names.
        cx.on_open(app.opener());
        cx.surface(app)
    }

    /// Opening an installer with Windows Apps installs it, through the same
    /// effect the palette and the install button reach.
    fn launch(input: &LaunchInput<'_>, _created: bool) -> Vec<Invoke<Windows>> {
        input
            .argument()
            .map(|path| {
                Invoke::effect(&WindowsEffect::Install {
                    path: Some(path.to_owned()),
                })
            })
            .into_iter()
            .collect()
    }

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        match scenario.unwrap_or("wide") {
            "detail" | "error-compact" | "vm-detail" | "confirm-delete" => (480, 800),
            "running" | "error" | "pending-install" => (1000, 760),
            _ => (1280, 800),
        }
    }

    /// The sample catalogue stands in for the compatibility service; the
    /// error scenarios show its first application's setup failed, and the
    /// pending-install scenario an installer awaiting confirmation.
    fn fixture(fixture: &mut Fixture<Windows>) {
        let mut sample = SampleProvider::new();
        if matches!(fixture.scenario(), Some("error" | "error-compact")) {
            sample.seed_setup_failure();
        }
        if fixture.scenario() == Some("pending-install") {
            sample.seed_install_confirmation();
        }
        fixture.provide(sample);
    }
}

impl ResourceSurface<Windows, WindowsEffect> for WindowsApp {
    /// The routes the window shows are the ones the service reports; the
    /// stored overrides reach the service through the route projection.
    fn present(&mut self, _state: &Rc<Windows>) {}

    fn attach(&mut self, cx: SurfaceCx<Windows>) {
        match cx.mode() {
            RunMode::Live => self.connect_live(),
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
            move |state: &Windows| reported.pending(state),
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
        self.stage_scenario(scenario);
    }

    fn effect(&mut self, effect: WindowsEffect, cx: &mut EffectCx<Windows, Self>) -> EffectAnswer {
        self.answer_effect(&effect, cx)
    }

    fn effects(&self) -> Vec<EffectStatus> {
        const CODE: &str = "windows.busy";
        const REASON: &str = "Another Windows operation is still running";
        if !self.is_busy() {
            return Vec::new();
        }
        vec![
            EffectStatus::unavailable(WindowsEffect::INSTALL, CODE, REASON),
            EffectStatus::unavailable(WindowsEffect::IMPORT_ISO, CODE, REASON),
            EffectStatus::unavailable(WindowsEffect::OPEN_FILE, CODE, REASON),
        ]
    }

    /// The service's catalogue, appliance and pending install, merged by id
    /// with the overrides the document holds.
    fn search_entries(&self) -> Vec<ui::tilo_resource::SearchEntry> {
        self.catalogue
            .apps
            .iter()
            .map(|app| {
                ui::tilo_resource::SearchEntry::new(
                    app.name.clone(),
                    format!("{} {}", app.publisher, app.route.label()),
                )
                .subtitled(app.publisher.clone())
                .grouped("Windows apps")
                .anchored(crate::search::app_resource_id(&app.id))
            })
            .collect()
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        actions::live_rows(&self.catalogue)
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        actions::live_rows(&self.catalogue)
            .into_iter()
            .find(|row| row.id() == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::CompatibilityProvider;
    use ui::prelude::TestApp;
    use ui::tilo_resource::Replica;

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
        let overridden = Windows {
            routes: BTreeMap::from([("adobe-reader".into(), Route::Proton)]),
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
        assert!(routes.pending(&Windows::default()).is_empty());

        routes.report(&reported("adobe-reader", Route::Proton, Route::Wine));
        assert!(
            routes.pending(&overridden).is_empty(),
            "the service runs it"
        );
        let reset = routes.pending(&Windows::default());
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

    /// A route is overridden and reset with field edits at its path; two
    /// people overriding different applications at once both land.
    #[test]
    fn routes_are_field_edits_that_converge() {
        let routes = Windows::paths().routes;
        let mut ours = Replica::<Windows>::new();
        ours.apply(Invoke::edit(&routes.key("adobe-reader").set(Route::Wine)).unwrap())
            .unwrap();
        let mut theirs = ours.fork();
        ours.apply(Invoke::edit(&routes.key("adobe-reader").set(Route::Proton)).unwrap())
            .unwrap();
        theirs
            .apply(Invoke::edit(&routes.key("cyberpunk-2077").set(Route::Vm)).unwrap())
            .unwrap();
        ours.sync_with(&mut theirs).unwrap();
        assert_eq!(ours.state().unwrap(), theirs.state().unwrap());
        let state = ours.state().unwrap();
        assert_eq!(state.route("adobe-reader"), Some(Route::Proton));
        assert_eq!(state.route("cyberpunk-2077"), Some(Route::Vm));

        ours.apply(Invoke::edit(&routes.remove("adobe-reader")).unwrap())
            .unwrap();
        assert_eq!(ours.state().unwrap().route("adobe-reader"), None);
        let published = ours.published().unwrap();
        assert!(
            published
                .actions
                .iter()
                .any(|action| action.id == "field-edit"),
            "routes are edited through the field edit"
        );
        assert!(
            published
                .actions
                .iter()
                .all(|action| action.id != "set-route" && action.id != "reset-route"),
            "no setter beside it"
        );
    }

    /// An agent overrides a route in one call; undo takes it back, and the
    /// entry reads as Windows Apps' sentence, naming who did it for whom.
    #[test]
    fn an_agent_routes_an_app_in_one_call_and_history_says_so() {
        use ui::tilo_actions::{ActionInvoker, ActorKind};

        let mut app = TestApp::<WindowsShell>::new().unwrap();
        let agent = ActionInvoker::actor("agent:claude")
            .with_kind(ActorKind::Agent)
            .acting_for("ada");
        let routes = Windows::paths().routes;
        for edit in [
            routes.key("adobe-reader").set(Route::Proton),
            routes.key("adobe-reader").set(Route::Vm),
            routes.remove("adobe-reader"),
        ] {
            app.call_by(
                agent.clone(),
                <Windows as ui::tilo_resource::Resource>::KIND,
                "field-edit",
                serde_json::json!({ "edit": edit }),
            )
            .unwrap();
        }
        assert_eq!(app.state().route("adobe-reader"), None);

        let catalogue = tilo_history::Catalogue::new("en-US", &[crate::strings::EN_US]);
        let reader = tilo_history::Reader::new("en-US").with_app(&catalogue);
        let entries: Vec<_> = app
            .versions()
            .into_iter()
            .filter_map(|version| version.context)
            .filter(|entry| {
                entry
                    .invoker
                    .as_ref()
                    .is_some_and(|invoker| invoker.actor == "agent:claude")
            })
            .collect();
        let sentences: Vec<String> = entries.iter().map(|entry| reader.sentence(entry)).collect();
        assert_eq!(
            sentences,
            [
                "Set an app to open with Proton",
                "Set an app to open in the Windows VM",
                "Let an app open the way the catalogue recommends",
            ]
        );
        let invoker = entries[0].invoker.as_ref().unwrap();
        assert_eq!(
            (invoker.kind, invoker.on_behalf_of.as_deref()),
            (ActorKind::Agent, Some("ada"))
        );
        assert!(app.undo());
        assert_eq!(app.state().route("adobe-reader"), Some(Route::Vm));
    }

    /// The window's effects join the manager: installing, importing an ISO.
    #[test]
    fn a_live_window_publishes_its_effects_on_the_manager() {
        let mut app = TestApp::<WindowsShell>::new().unwrap();
        let resources = app.harness().resources();
        let manager = resources
            .iter()
            .find(|resource| resource.id.as_str() == "windows.manager")
            .expect("the manager");
        for id in ["install", "import-iso"] {
            assert!(
                manager.actions.iter().any(|action| action.id == id),
                "{id} is on the manager"
            );
        }
    }

    /// Opening an installer with the app installs it through the install
    /// effect; a plain start takes no action.
    #[test]
    fn a_launch_naming_an_installer_invokes_install() {
        let arguments = ["/home/you/Downloads/setup.exe".to_owned()];
        let launch = WindowsShell::launch(&LaunchInput::new(&arguments, None), false);
        assert_eq!(launch.len(), 1);
        assert_eq!(launch[0].id, WindowsEffect::INSTALL.id);
        assert!(launch[0].is_effect());
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
        assert!(frame.contains("windows-confirmation"), "{frame}");

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
                .any(|row| row["resource"] == "windows.manager" && row["action"] == "field-edit"),
            "routes are edited on the manager"
        );
    }
}

#[cfg(test)]
mod capture_tests {
    /// Every declared scenario stages through the framework's capture hook
    /// and renders, the same path `--shots --scenario NAME` takes.
    #[test]
    fn every_scenario_stages_and_renders_in_the_framework_app() {
        let dir = tempfile::tempdir().unwrap();
        for scenario in super::SCENARIOS {
            let mut app =
                ui::framework::testing::TestApp::<super::WindowsShell>::capture(Some(scenario))
                    .unwrap();
            app.harness()
                .render_png(&dir.path().join(format!("{scenario}.png")));
        }
    }
}
