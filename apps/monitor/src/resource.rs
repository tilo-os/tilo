//! System Monitor as a resource: the shared table layout (sort, tree or
//! flat) is the [`Monitor`] model, edited at its paths; this machine's
//! processes are the surface's [`ResourceSurface::live_rows`], and
//! everything Monitor does to them (refresh, inspect, copy a PID, end a
//! process or a whole app) is its typed effect vocabulary [`MonitorEffect`],
//! answered by the surface.

use std::rc::Rc;

use ui::framework::{
    EffectAnswer, EffectCx, Effected, Fixture, LiveRow, ResourceApp, ResourceSurface, RunMode,
    SurfaceCx, ViewCx,
};
use ui::i18n::EmbeddedCatalog;
use ui::tilo_resource::{self, Effect, Invoke, Model, resource};
use ui::widget::Element;

use crate::app::{MonitorApp, SortKey};
use crate::model::MonitorModel;

ui::embedded_catalog!(static CATALOG, ["en-US"]);

/// How the process table is laid out, shared with everyone who sees it.
/// Each part is edited at its path (`Monitor::paths().flat.set(true)`); the
/// tab, the filter and the selection are each window's own.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Monitor {
    /// Sort column
    pub sort: SortKey,
    /// Ascending
    ///
    /// The smallest value, or A, first.
    pub ascending: bool,
    /// Flat list
    ///
    /// Every process in one sorted list rather than the process tree.
    pub flat: bool,
}

#[resource(kind = "monitor.processes", label = "Processes")]
impl Monitor {}

/// What Monitor does on this machine: work with no document delta, invoked
/// from its own controls, the palette, an agent or a script alike. A
/// `process` is a live process row's id, which names one process lifetime
/// (its PID with its kernel start time), so a delayed invocation never
/// reaches a process that reused the PID.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "monitor.effect")]
pub enum MonitorEffect {
    /// Refresh processes.
    #[effect(kind = "read")]
    Refresh,
    /// Show tab.
    #[effect(kind = "read")]
    ShowTab {
        #[param("processes | resources")]
        tab: String,
    },
    /// Filter processes.
    #[effect(kind = "read")]
    Filter {
        #[param("Process filter text, or omit to clear")]
        query: Option<String>,
    },
    /// Show process details.
    #[effect(kind = "read")]
    Inspect {
        #[param("The process row's id")]
        process: String,
    },
    /// Copy PID.
    #[effect(kind = "read")]
    CopyPid {
        #[param("The process row's id")]
        process: String,
    },
    /// End process.
    ///
    /// Asks the process to close, as quitting it would. Only the person's
    /// own apps and the programs they started can be ended, never the
    /// system's services.
    #[effect(destructive)]
    EndProcess {
        #[param("The process row's id")]
        process: String,
    },
    /// Force kill.
    #[effect(destructive)]
    Kill {
        #[param("The process row's id")]
        process: String,
    },
    /// End app.
    ///
    /// Asks every process of the app to close.
    #[effect(destructive)]
    EndApp {
        #[param("The app's id, as a process row names it")]
        app: String,
    },
}

/// The screenshot scenarios: the sample machine, staged.
const SCENARIOS: &[&str] = &[
    "detail",
    "resources",
    "resources-many",
    "empty",
    "process-loading",
    "process-permission-error",
    "no-results",
    "flat",
    "tree-expanded",
    "per-process",
    "kernel-threads",
    "context-menu",
    "interval",
];

/// Framework app.
pub struct MonitorShell;

impl ResourceApp for MonitorShell {
    type Resource = Monitor;
    const ID: &'static str = "so.tilo.monitor";
    const READS_HOST: &'static [ui::launch::HostSource] = &[ui::launch::HostSource::Processes];
    const TITLE: &'static str = "System Monitor";
    const ICON: Option<&'static str> = Some("monitor");

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    /// A capture shows the sample machine instead of this one.
    fn fixture(fixture: &mut Fixture<Monitor>) {
        fixture.provide(MonitorModel::scenario(fixture.scenario()));
    }

    /// The flat list is the shared table layout, set as a person sets it.
    fn scenario(name: Option<&str>) -> Vec<Invoke<Monitor>> {
        match name {
            Some("flat" | "kernel-threads") => Invoke::edit(&Monitor::paths().flat.set(true))
                .into_iter()
                .collect(),
            _ => Vec::new(),
        }
    }

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }

    fn view(cx: &mut ViewCx<Monitor>) -> Element {
        // Only a live window samples this machine; a capture shows its
        // fixture, and a past version shows the layout with no processes.
        let app = match cx.mode() {
            RunMode::Live => MonitorApp::new_live(),
            RunMode::Capture => MonitorApp::sample(
                cx.fixture::<MonitorModel>()
                    .map_or_else(MonitorModel::sample, |model| (*model).clone()),
            ),
            RunMode::Past => MonitorApp::past(),
        };
        // A search hit, or an agent's `open`, shows the process it names.
        cx.on_open(app.opener());
        cx.surface(app)
    }
}

impl ResourceSurface<Monitor, MonitorEffect> for MonitorApp {
    fn present(&mut self, state: &Rc<Monitor>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Monitor>) {
        self.wake_with(cx.waker());
        self.follow_preferences(cx.settings());
        self.attach_framework(cx.dispatcher());
        self.attach_actions(cx.actions());
    }

    fn stage(&mut self, scenario: Option<&str>) {
        if let Some(scenario) = scenario {
            self.stage_scenario(scenario);
        }
    }

    fn effect(&mut self, effect: MonitorEffect, _cx: &mut EffectCx<Monitor, Self>) -> EffectAnswer {
        self.answer(effect).map(|()| Effected::done())
    }

    /// The processes on this machine, beneath the processes door: inspect,
    /// copy the PID, stop or kill one. Not document state.
    fn live_rows(&self) -> Vec<LiveRow> {
        self.live_process_rows()
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.live_process_row(id)
    }

    /// Every process running here, each found by its name, user or PID and
    /// opening its exact generation — the tail beyond the published rows too.
    fn search_entries(&self) -> Vec<ui::tilo_resource::SearchEntry> {
        self.process_search_entries()
    }

    /// Processes are the machine's state of the moment: a closed Monitor
    /// leaves none in the system's search index to open into one that has
    /// ended.
    fn kept_in_search_index(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prefs::MonitorPreferences;
    use ui::framework::testing::TestApp;
    use ui::tilo_actions::OperationKind;

    /// Whether the inspector shows the process `pid`: its PID property, not
    /// the table's cell.
    fn inspected(app: &mut TestApp<MonitorShell>, pid: u32) -> bool {
        fn find(node: &serde_json::Value, pid: &str) -> bool {
            if node.get("name").and_then(serde_json::Value::as_str) == Some("PID")
                && node.get("role").and_then(serde_json::Value::as_str) == Some("StaticText")
                && node.get("value").and_then(serde_json::Value::as_str) == Some(pid)
            {
                return true;
            }
            match node {
                serde_json::Value::Object(fields) => fields.values().any(|child| find(child, pid)),
                serde_json::Value::Array(items) => items.iter().any(|child| find(child, pid)),
                _ => false,
            }
        }
        let inspection: serde_json::Value =
            serde_json::from_str(&app.harness().inspection_json().unwrap()).unwrap();
        find(&inspection, &pid.to_string())
    }

    /// The window bar's slots as the window declares them now.
    fn bar(app: &mut TestApp<MonitorShell>) -> ui::window_toolbar::WindowToolbar {
        app.settle();
        ui::window_toolbar::declared().expect("the window declares its slots")
    }

    /// Whether the bar's view `index` (0 Processes, 1 Resources) is chosen.
    fn tab_shown(app: &mut TestApp<MonitorShell>, index: usize) -> Option<bool> {
        bar(app)
            .views
            .map(|views| views.chosen_index() == Some(index))
    }

    /// Press the window bar's slot `id`, as the desktop's bar does.
    fn press(app: &mut TestApp<MonitorShell>, id: &str) {
        app.harness()
            .press_window_slot(ui::window_toolbar::ToolbarEvent::Action { id: id.into() });
    }

    /// The update frequency the window bar's menu button names.
    fn shown_interval(app: &mut TestApp<MonitorShell>) -> String {
        bar(app)
            .actions
            .into_iter()
            .find(|action| action.id == crate::app::window_bar::CADENCE)
            .expect("the bar holds the update frequency")
            .name
    }

    #[test]
    fn preferences_follow_the_person_and_stay_out_of_the_document() {
        let mut app = TestApp::<MonitorShell>::new().unwrap();
        assert_eq!(
            shown_interval(&mut app),
            "Update frequency: Every 1.5 seconds"
        );

        app.settings::<MonitorPreferences>()
            .set(&MonitorPreferences {
                interval_ms: 3000,
                show_kernel_threads: true,
            })
            .unwrap();
        app.harness().relayout();
        app.harness().relayout();
        assert_eq!(
            shown_interval(&mut app),
            "Update frequency: Every 3 seconds"
        );

        let versions = app.versions().len();
        press(&mut app, &crate::view::Cadence::Every(5000).item_id());
        assert_eq!(
            shown_interval(&mut app),
            "Update frequency: Every 5 seconds"
        );
        assert_eq!(
            app.settings::<MonitorPreferences>().get(),
            MonitorPreferences {
                interval_ms: 5000,
                show_kernel_threads: true,
            }
        );
        assert_eq!(
            app.versions().len(),
            versions,
            "preferences are not document history"
        );

        let mut app = app.reopen().unwrap();
        assert_eq!(
            shown_interval(&mut app),
            "Update frequency: Every 5 seconds"
        );
    }

    #[test]
    fn the_table_layout_syncs_and_what_the_window_shows_stays_local() {
        let mut app = TestApp::<MonitorShell>::new().unwrap();
        let published = app.harness().resources();
        let door = published
            .iter()
            .find(|resource| resource.kind == "monitor.processes")
            .expect("the processes door");
        let ids: Vec<&str> = door
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect();
        for id in [
            "field-edit",
            "refresh",
            "show-tab",
            "filter",
            "inspect",
            "copy-pid",
            "end-process",
            "kill",
            "end-app",
        ] {
            assert!(
                ids.contains(&id),
                "`{id}` is published on the door: {ids:?}"
            );
        }

        // Sorting is an edit of the shared layout, one call and one undo
        // step each.
        let versions = app.versions().len();
        let paths = Monitor::paths();
        for edit in [paths.sort.set(SortKey::Name), paths.ascending.set(true)] {
            app.call("field-edit", serde_json::json!({ "edit": edit }))
                .unwrap();
        }
        assert_eq!(app.state().sort, SortKey::Name);
        assert!(app.state().ascending);
        assert_eq!(app.versions().len(), versions + 2);
        press(&mut app, crate::app::window_bar::TREE);
        assert!(app.state().flat);
        assert!(app.undo());
        assert!(!app.state().flat);
        assert!(app.undo());
        assert!(!app.state().ascending, "each edit is its own step");

        // The filter and the tab are the window's own: no document change.
        let versions = app.versions().len();
        app.effect(&MonitorEffect::Filter {
            query: Some("cargo".into()),
        })
        .unwrap();
        app.harness().relayout();
        let mut count = None;
        ui::widget::with_filterable(app.surface_mut::<MonitorApp>(), &mut |list| {
            count = list.filter_count();
        });
        assert!(count.is_some(), "the list is narrowed to the query");
        app.effect(&MonitorEffect::ShowTab {
            tab: "resources".into(),
        })
        .unwrap();
        assert_eq!(tab_shown(&mut app, 1), Some(true));
        assert!(
            app.effect(&MonitorEffect::ShowTab {
                tab: "graphs".into()
            })
            .is_err(),
            "an unknown tab is refused"
        );
        assert_eq!(app.versions().len(), versions);
    }

    /// The process row id of `pid`, a process this test started.
    fn row_id(pid: u32) -> String {
        // On Linux a lifetime is the kernel's start tick, as the session's
        // service reports it.
        #[cfg(target_os = "linux")]
        let started_at = {
            let stat =
                std::fs::read_to_string(format!("/proc/{pid}/stat")).expect("the child runs");
            tilo_system_stats::probe::parse_stat(pid, &stat)
                .expect("a stat line")
                .started_at
        };
        #[cfg(not(target_os = "linux"))]
        let started_at = {
            use tilo_system_stats::probe::Probe as _;
            tilo_system_info::local::SysinfoProbe::new()
                .process(pid)
                .expect("the child runs")
                .started_at
        };
        format!("process:{pid}:started:{started_at}")
    }

    #[test]
    fn process_rows_are_live_children_of_the_door_answered_by_the_surface() {
        let mut app = TestApp::<MonitorShell>::new().unwrap();
        let resources = app.harness().resources();
        let doors: Vec<_> = resources
            .iter()
            .filter(|resource| resource.kind == "monitor.processes")
            .collect();
        assert_eq!(doors.len(), 1, "one processes door");
        assert!(
            resources
                .iter()
                .all(|resource| resource.kind != "monitor.process"),
            "no process row beside the door"
        );
        let rows: Vec<_> = doors[0]
            .children
            .iter()
            .filter(|child| child.kind == "monitor.process")
            .collect();
        assert!(!rows.is_empty(), "this machine's processes are rows");
        assert!(
            rows.iter().all(|row| row
                .actions
                .iter()
                .all(|action| action.kind != OperationKind::Document)),
            "a live process row carries only views and effects"
        );

        // A process this test owns, so a kill reaches nothing else.
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("start a child process");
        app.effect(&MonitorEffect::Refresh).unwrap();
        let id = row_id(child.id());
        app.effect(&MonitorEffect::Filter {
            query: Some(child.id().to_string()),
        })
        .unwrap();

        press(&mut app, crate::app::window_bar::TREE);
        assert!(
            !inspected(&mut app, child.id()),
            "the child is not selected"
        );
        app.call_on(&id, "inspect", serde_json::json!({})).unwrap();
        app.harness().relayout();
        assert!(inspected(&mut app, child.id()), "the child is selected");

        let hit = app
            .surface_mut::<MonitorApp>()
            .process_search_entries()
            .into_iter()
            .find(|entry| entry.anchor.as_deref() == Some(id.as_str()))
            .expect("the child is found");
        assert!(hit.action.is_none(), "a hit names its row, never an action");
        press(&mut app, crate::app::Tab::Resources.view_id());
        assert_eq!(tab_shown(&mut app, 1), Some(true));
        app.call("open", serde_json::json!({ "anchor": id }))
            .unwrap();
        assert!(
            app.call("open", serde_json::json!({ "anchor": "a-process" }))
                .is_err(),
            "an anchor that names no process is refused"
        );
        app.settle();
        app.harness().relayout();
        assert_eq!(
            tab_shown(&mut app, 1),
            Some(false),
            "open shows the processes"
        );
        assert!(inspected(&mut app, child.id()), "and the child in them");

        // Killing it asks first, then signals exactly that process.
        assert!(
            app.call_on(&id, "kill", serde_json::json!({})).is_err(),
            "a kill needs confirmation"
        );
        app.effect_confirmed(&MonitorEffect::Kill {
            process: id.clone(),
        })
        .unwrap();
        let status = child.wait().expect("the child exits");
        assert!(!status.success(), "the child was killed: {status:?}");
        app.effect(&MonitorEffect::Refresh).unwrap();
        assert!(
            app.effect(&MonitorEffect::CopyPid { process: id }).is_err(),
            "a process that is gone is refused"
        );
    }

    #[test]
    fn processes_are_found_while_the_monitor_runs_and_never_kept_in_the_search_index() {
        let scenario = MonitorShell::scenarios()
            .iter()
            .find(|scenario| {
                !matches!(
                    **scenario,
                    "empty" | "process-loading" | "process-permission-error"
                )
            })
            .expect("a scenario with the sample processes");
        let mut app = TestApp::<MonitorShell>::capture(Some(scenario)).unwrap();
        app.harness().relayout();
        let processes: Vec<String> = app
            .surface_mut::<MonitorApp>()
            .process_search_entries()
            .into_iter()
            .filter_map(|entry| entry.anchor)
            .collect();
        assert!(
            !processes.is_empty(),
            "the running Monitor finds its processes"
        );
        let kept = app.search_index_entries();
        assert!(
            kept.iter()
                .all(|entry| !processes.contains(&entry.resource)),
            "no process outlives the Monitor in the search index: {kept:?}"
        );
    }

    /// The process rows a capture publishes.
    fn process_rows(app: &mut TestApp<MonitorShell>) -> usize {
        app.harness()
            .resources()
            .iter()
            .flat_map(|resource| resource.children.iter())
            .filter(|child| child.kind == "monitor.process")
            .count()
    }

    #[test]
    fn every_scenario_stages_the_sample_machine() {
        for scenario in MonitorShell::scenarios() {
            let mut app = TestApp::<MonitorShell>::capture(Some(scenario)).unwrap();
            app.harness().relayout();
            assert_eq!(
                tab_shown(&mut app, 1),
                Some(scenario.starts_with("resources")),
                "`{scenario}` shows its tab"
            );
            let rows = process_rows(&mut app);
            if matches!(
                *scenario,
                "empty" | "process-loading" | "process-permission-error"
            ) {
                assert_eq!(rows, 0, "`{scenario}` lists no processes");
            } else {
                assert!(rows > 0, "`{scenario}` lists the sample processes");
            }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn captured_startup_paints_the_actual_hosted_machine_at_both_widths() {
        fn assert_painted(app: &mut TestApp<MonitorShell>, viewport: (u32, u32)) {
            assert_eq!(tab_shown(app, 1), Some(false));
            assert!(process_rows(app) > 0);
            let pixels = app.harness().render_rgba();
            let first = pixels.get(..4).expect("the real host must render pixels");
            assert!(
                pixels.chunks_exact(4).any(|pixel| pixel != first),
                "the captured machine must paint more than the window ground at {viewport:?}"
            );
        }
        for viewport in [(1280, 800), (390, 844)] {
            let mut app = TestApp::<MonitorShell>::capture_at(None, viewport).unwrap();
            // Observe startup before any explicit extra relayout or theme
            // change can repair a lost retained layout.
            assert_painted(&mut app, viewport);
            app.harness().set_theme(ui::Theme::light());
            assert_painted(&mut app, viewport);
        }
    }

    #[test]
    fn a_capture_and_a_past_version_never_read_this_machine() {
        crate::sampler::reset_provider_read_count();
        let mut app = TestApp::<MonitorShell>::capture(None).unwrap();
        app.harness().relayout();
        press(&mut app, crate::app::window_bar::TREE);
        let first = app.versions().first().unwrap().hash;
        assert!(app.past_version(&[first]).is_ok());
        assert_eq!(
            crate::sampler::provider_read_count(),
            0,
            "the sample machine and past versions are drawn without probing this one"
        );
    }

    /// The real summary, shared layout and window-local tabs all enter
    /// through admitted controls in a live process window.
    #[test]
    fn live_monitor_controls_are_certified() {
        let mut app = TestApp::<MonitorShell>::new().unwrap();
        let audit = app.harness().native_app_audit();
        assert!(audit.is_certified(), "processes: {audit:?}");
        press(&mut app, crate::app::window_bar::TREE);
        assert!(app.state().flat);
        let audit = app.harness().native_app_audit();
        assert!(audit.is_certified(), "flat processes: {audit:?}");
        press(&mut app, crate::app::Tab::Resources.view_id());
        assert_eq!(tab_shown(&mut app, 1), Some(true));
        let audit = app.harness().native_app_audit();
        assert!(audit.is_certified(), "resources: {audit:?}");
    }

    ui::action_contract_tests! {
        seed: ui::automerge::AutoCommit::new(),
        registry: {
            let mut registry = ui::tilo_actions::ActionRegistry::new();
            <Monitor as ui::tilo_resource::Resource>::register(&mut registry);
            registry
        },
        resources: |_| TestApp::<MonitorShell>::new().unwrap().harness().resources(),
        palette: ["Refresh processes", "Show process details", "Copy PID", "End process"],
    }

    /// Two people changing the layout at once converge: each part is its
    /// own field, so a sort and a switch to the flat list both land.
    #[test]
    fn concurrent_layout_edits_converge() {
        use ui::tilo_resource::Replica;
        let paths = Monitor::paths();
        let mut ours = Replica::<Monitor>::new();
        let mut theirs = ours.fork();
        ours.apply(Invoke::edit(&paths.sort.set(SortKey::Mem)).unwrap())
            .unwrap();
        theirs
            .apply(Invoke::edit(&paths.flat.set(true)).unwrap())
            .unwrap();
        ours.sync_with(&mut theirs).unwrap();
        assert_eq!(ours.state().unwrap(), theirs.state().unwrap());
        assert_eq!(
            ours.state().unwrap(),
            Monitor {
                sort: SortKey::Mem,
                ascending: false,
                flat: true,
            }
        );
    }

    /// A fresh layout is the process tree by CPU, busiest first.
    #[test]
    fn a_new_layout_is_the_tree_by_cpu() {
        let app = TestApp::<MonitorShell>::new().unwrap();
        assert_eq!(app.state(), Monitor::default());
        assert_eq!(Monitor::default().sort, SortKey::Cpu);
    }

    /// What an agent changes in the shared layout is history a person
    /// reads as Monitor's own sentences, with who did it for whom.
    #[test]
    fn history_entries_render_as_the_apps_sentences() {
        use ui::tilo_actions::{ActionInvoker, ActorKind};

        let mut app = TestApp::<MonitorShell>::new().unwrap();
        let agent = ActionInvoker::actor("agent:claude")
            .with_kind(ActorKind::Agent)
            .acting_for("ada");
        let kind = <Monitor as ui::tilo_resource::Resource>::KIND;
        let paths = Monitor::paths();
        for edit in [
            paths.sort.set(SortKey::Mem),
            paths.ascending.set(true),
            paths.flat.set(true),
            paths.flat.set(false),
        ] {
            app.call_by(
                agent.clone(),
                kind,
                "field-edit",
                serde_json::json!({ "edit": edit }),
            )
            .unwrap();
        }
        let entries: Vec<_> = app
            .history()
            .into_iter()
            .filter(|entry| {
                entry
                    .invoker
                    .as_ref()
                    .is_some_and(|invoker| invoker.actor == "agent:claude")
            })
            .collect();
        let sentences = app.sentences(&entries);
        assert_eq!(
            sentences,
            [
                "Sorted the processes by memory",
                "Sorted the processes ascending",
                "Listed the processes flat",
                "Showed the process tree",
            ]
        );
        let invoker = entries[0].invoker.as_ref().unwrap();
        assert_eq!(
            (invoker.kind, invoker.on_behalf_of.as_deref()),
            (ActorKind::Agent, Some("ada"))
        );
    }
}

#[cfg(test)]
mod reach_tests {
    use ui::framework::ResourceApp as _;
    use ui::framework::testing::{TestApp, unreachable_actions};

    /// Every action the app publishes is reachable by address in a drive of
    /// each of its scenarios: none is advertised yet handled only by a
    /// click. The scenarios show a sample table, which no signal reaches;
    /// the app someone opens lists this machine's processes, and walking
    /// its Kill would signal every one of them, the test's own among them.
    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action() {
        for scenario in super::MonitorShell::scenarios() {
            let mut app = TestApp::<super::MonitorShell>::drive(scenario)
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            assert_eq!(
                unreachable_actions(&mut app),
                Vec::<String>::new(),
                "{scenario}: published actions a drive cannot invoke"
            );
        }
    }
}
