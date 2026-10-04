//! System Monitor as a [`tilo_resource::Resource`]: the shared table layout
//! (sort, tree or flat) syncs in the document; this machine's processes are
//! the surface's [`ResourceSurface::live_rows`], and everything Monitor does
//! to them (refresh, inspect, copy a PID, stop, kill) is its typed effect
//! vocabulary [`MonitorEffect`], answered by the surface.

use std::rc::Rc;

use serde::{Deserialize, Serialize};
use ui::framework::{
    EffectAnswer, EffectCx, Effected, Fixture, LiveRow, ResourceApp, ResourceSurface, RunMode,
    SurfaceCx, ViewCx,
};
use ui::tilo_actions::{Action, ActionError, ActionParam, ActionRegistry, ParamType};
use ui::tilo_resource::{self, ActionDecl, Doc, Effect, ModelError, Resource};
use ui::widget::Element;

use crate::app::MonitorApp;
use crate::doc::ViewDoc;
use crate::model::MonitorModel;

/// The shared table layout.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MonitorAction {
    SetSort { key: String, desc: bool },
    SetTreeMode { tree: bool },
}

impl Action for MonitorAction {
    fn apply(&self, doc: &mut ui::tilo_actions::Doc) -> Result<(), ActionError> {
        match self {
            Self::SetSort { key, desc } => {
                ViewDoc::set_sort_key(doc, key);
                ViewDoc::set_sort_desc(doc, *desc);
            }
            Self::SetTreeMode { tree } => ViewDoc::set_tree_mode(doc, *tree),
        }
        Ok(())
    }
}

/// How the process table is laid out, as the document holds it.
#[derive(Clone, Debug)]
pub struct MonitorState {
    pub sort_key: String,
    pub sort_desc: bool,
    pub tree_mode: bool,
}

impl Default for MonitorState {
    fn default() -> Self {
        Self {
            sort_key: "cpu".into(),
            sort_desc: true,
            tree_mode: true,
        }
    }
}

/// What Monitor does on this machine: work with no document delta, invoked
/// from its own controls, the palette, an agent or a script alike. A
/// `process` is a live process row's id, which names one process lifetime
/// (its PID with its kernel start time), so a delayed invocation never
/// reaches a process that reused the PID.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "monitor.effect")]
pub enum MonitorEffect {
    /// Refresh processes.
    #[effect(permission = "read")]
    Refresh,
    /// Show tab.
    #[effect(permission = "read")]
    ShowTab {
        #[param("processes | resources")]
        tab: String,
    },
    /// Filter processes.
    #[effect(permission = "read")]
    Filter {
        #[param("Process filter text, or omit to clear")]
        query: Option<String>,
    },
    /// Show process details.
    #[effect(permission = "read")]
    Inspect {
        #[param("The process row's id")]
        process: String,
    },
    /// Copy PID.
    #[effect(permission = "read")]
    CopyPid {
        #[param("The process row's id")]
        process: String,
    },
    /// Stop process.
    #[effect(destructive, permission = "admin")]
    Stop {
        #[param("The process row's id")]
        process: String,
    },
    /// Force kill.
    #[effect(destructive, permission = "admin")]
    Kill {
        #[param("The process row's id")]
        process: String,
    },
}

/// The live process catalogue resource.
pub struct Monitor;

impl Resource for Monitor {
    const KIND: &'static str = "monitor.processes";
    const LABEL: &'static str = "Processes";
    const ACTION_TYPE: &'static str = "monitor.action";

    type State = MonitorState;
    type Action = MonitorAction;

    fn register(registry: &mut ActionRegistry) {
        registry.register::<MonitorAction>(Self::ACTION_TYPE);
    }

    fn read(doc: &Doc) -> Result<MonitorState, ModelError> {
        Ok(MonitorState {
            sort_key: ViewDoc::sort_key(doc),
            sort_desc: ViewDoc::sort_desc(doc),
            tree_mode: ViewDoc::tree_mode(doc),
        })
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![
            ActionDecl::new(
                "set-sort",
                "Sort process table",
                vec![
                    ActionParam::required(
                        "key",
                        ParamType::Text,
                        "pid | name | user | cpu | mem | threads",
                    )
                    .with_choices(["pid", "name", "user", "cpu", "mem", "threads"]),
                    ActionParam::optional(
                        "desc",
                        ParamType::Boolean,
                        "Descending when true; omit for the column default",
                    ),
                ],
                |args, _| {
                    let key = args
                        .text("key")
                        .ok_or_else(|| "missing sort key".to_owned())?
                        .to_owned();
                    let desc = args
                        .boolean("desc")
                        .unwrap_or(!matches!(key.as_str(), "name" | "user"));
                    Ok(MonitorAction::SetSort { key, desc })
                },
            )
            .describe("Set the shared process-table sort."),
            ActionDecl::new(
                "set-tree-mode",
                "Set tree mode",
                vec![ActionParam::required(
                    "tree",
                    ParamType::Boolean,
                    "true for parent→child tree, false for a flat list",
                )],
                |args, _| {
                    let tree = args
                        .boolean("tree")
                        .ok_or_else(|| "missing tree flag".to_owned())?;
                    Ok(MonitorAction::SetTreeMode { tree })
                },
            )
            .describe("Switch the shared tree vs flat process layout."),
        ]
    }
}

/// The screenshot scenarios: the sample machine, staged.
const SCENARIOS: &[&str] = &[
    "detail",
    "interval-popover",
    "resources",
    "resources-many",
    "empty",
    "process-loading",
    "process-permission-error",
    "no-results",
    "tree-collapsed",
    "tree-expanded",
    "per-process",
    "per-process-collapsed",
    "context-menu",
    "kill-confirm",
];

/// Framework app.
pub struct MonitorShell;

impl ResourceApp for MonitorShell {
    type Resource = Monitor;
    const ID: &'static str = "so.tilo.monitor";
    const TITLE: &'static str = "System Monitor";
    const ICON: Option<&'static str> = Some("monitor");

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1280, 800)
    }

    /// A capture shows the sample machine instead of this one.
    fn fixture(fixture: &mut Fixture<Monitor>) {
        fixture.provide(MonitorModel::scenario(fixture.scenario()));
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
        cx.surface(app)
    }
}

impl ResourceSurface<Monitor, MonitorEffect> for MonitorApp {
    fn present(&mut self, state: &Rc<MonitorState>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Monitor>) {
        self.follow_preferences(cx.settings());
        self.attach_framework(cx.dispatcher());
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prefs::MonitorPreferences;
    use ui::framework::testing::TestApp;
    use ui::tilo_actions::OperationKind;

    /// The accessibility node named `name`, depth first.
    fn announced(node: &serde_json::Value, name: &str) -> Option<serde_json::Value> {
        if node.get("name").and_then(serde_json::Value::as_str) == Some(name) {
            return Some(node.clone());
        }
        match node {
            serde_json::Value::Object(fields) => {
                fields.values().find_map(|child| announced(child, name))
            }
            serde_json::Value::Array(items) => {
                items.iter().find_map(|child| announced(child, name))
            }
            _ => None,
        }
    }

    /// The accessibility node whose semantic key is `key`, depth first.
    fn keyed(node: &serde_json::Value, key: &str) -> Option<serde_json::Value> {
        if node.get("semantic_key").and_then(serde_json::Value::as_str) == Some(key) {
            return Some(node.clone());
        }
        match node {
            serde_json::Value::Object(fields) => {
                fields.values().find_map(|child| keyed(child, key))
            }
            serde_json::Value::Array(items) => items.iter().find_map(|child| keyed(child, key)),
            _ => None,
        }
    }

    /// Whether the table shows the process row `id` selected; `None` when
    /// it does not list it.
    fn shown_selected(app: &mut TestApp<MonitorShell>, id: &str) -> Option<bool> {
        let inspection: serde_json::Value =
            serde_json::from_str(&app.harness().inspection_json().unwrap()).unwrap();
        keyed(&inspection, &format!("monitor.{id}")).map(|node| node["state"]["selected"] == true)
    }

    fn named_node(app: &mut TestApp<MonitorShell>, name: &str) -> serde_json::Value {
        let inspection: serde_json::Value =
            serde_json::from_str(&app.harness().inspection_json().unwrap()).unwrap();
        announced(&inspection, name).unwrap_or_else(|| panic!("`{name}` is announced"))
    }

    /// The interval the window shows, as a screen reader hears it.
    fn shown_interval(app: &mut TestApp<MonitorShell>) -> String {
        named_node(app, "Refresh interval")["value"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// Click the control named `name` where a person sees it.
    fn click(app: &mut TestApp<MonitorShell>, name: &str) {
        let node = named_node(app, name);
        let bounds = &node["bounds"];
        let coordinate = |low: &str, high: &str| {
            f64::midpoint(
                bounds[low].as_f64().unwrap(),
                bounds[high].as_f64().unwrap(),
            )
        };
        let (x, y) = (coordinate("x0", "x1"), coordinate("y0", "y1"));
        app.harness().click_at(x, y);
        app.harness().relayout();
    }

    #[test]
    fn preferences_follow_the_person_and_stay_out_of_the_document() {
        let mut app = TestApp::<MonitorShell>::new().unwrap();
        assert_eq!(shown_interval(&mut app), "1.5s");

        // Another of the person's devices changed them.
        app.settings::<MonitorPreferences>()
            .set(&MonitorPreferences {
                interval_ms: 3000,
                show_kernel_threads: true,
                confirm_kill: false,
            })
            .unwrap();
        app.harness().relayout();
        assert_eq!(shown_interval(&mut app), "3s");

        // A click here cycles the cadence and stores it for every device.
        let versions = app.versions().len();
        click(&mut app, "Refresh interval");
        assert_eq!(shown_interval(&mut app), "5s");
        assert_eq!(
            app.settings::<MonitorPreferences>().get(),
            MonitorPreferences {
                interval_ms: 5000,
                show_kernel_threads: true,
                confirm_kill: false,
            }
        );
        assert_eq!(
            app.versions().len(),
            versions,
            "preferences are not document history"
        );

        let mut app = app.reopen().unwrap();
        assert_eq!(shown_interval(&mut app), "5s");
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
            "set-sort",
            "set-tree-mode",
            "refresh",
            "show-tab",
            "filter",
            "inspect",
            "copy-pid",
            "stop",
            "kill",
        ] {
            assert!(
                ids.contains(&id),
                "`{id}` is published on the door: {ids:?}"
            );
        }

        // Sorting is an edit of the shared layout, one undo step.
        let versions = app.versions().len();
        click(&mut app, "Sort by name");
        assert_eq!(app.state().sort_key, "name");
        assert!(!app.state().sort_desc, "text columns sort ascending first");
        assert_eq!(app.versions().len(), versions + 1);
        click(&mut app, "Tree process view");
        assert!(!app.state().tree_mode);
        assert!(app.undo());
        assert!(app.state().tree_mode);

        // The filter and the tab are the window's own: no document change.
        let versions = app.versions().len();
        app.effect(&MonitorEffect::Filter {
            query: Some("cargo".into()),
        })
        .unwrap();
        let inspection: serde_json::Value =
            serde_json::from_str(&app.harness().inspection_json().unwrap()).unwrap();
        assert_eq!(
            keyed(&inspection, "monitor.search").unwrap()["value"],
            "cargo"
        );
        app.effect(&MonitorEffect::ShowTab {
            tab: "resources".into(),
        })
        .unwrap();
        assert_eq!(named_node(&mut app, "Resources")["state"]["selected"], true);
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
        let mut system = sysinfo::System::new();
        let target = sysinfo::Pid::from_u32(pid);
        system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[target]), true);
        let started_at = system.process(target).expect("the child runs").start_time();
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

        // Inspecting its row through the door selects it.
        click(&mut app, "Tree process view");
        assert_eq!(
            shown_selected(&mut app, &id),
            Some(false),
            "the child is listed, unselected"
        );
        app.call_on(&id, "inspect", serde_json::json!({})).unwrap();
        assert_eq!(
            shown_selected(&mut app, &id),
            Some(true),
            "the child is selected"
        );

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
            let inspection: serde_json::Value =
                serde_json::from_str(&app.harness().inspection_json().unwrap()).unwrap();
            let resources_shown = keyed(&inspection, "monitor.tab.resources")
                .map(|tab| tab["state"]["selected"] == true);
            let expected = if *scenario == "kill-confirm" {
                // The confirmation is modal: only the dialog is announced.
                None
            } else {
                Some(scenario.starts_with("resources"))
            };
            assert_eq!(resources_shown, expected, "`{scenario}` shows its tab");
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

    #[test]
    fn a_capture_and_a_past_version_never_read_this_machine() {
        crate::sampler::reset_provider_read_count();
        let mut app = TestApp::<MonitorShell>::capture(None).unwrap();
        app.harness().relayout();
        click(&mut app, "Sort by name");
        let first = app.versions().first().unwrap().hash;
        assert!(app.past_version(&[first]).is_ok());
        assert_eq!(
            crate::sampler::provider_read_count(),
            0,
            "the sample machine and past versions are drawn without probing this one"
        );
    }

    ui::action_contract_tests! {
        seed: ui::automerge::AutoCommit::new(),
        registry: {
            let mut registry = ActionRegistry::new();
            Monitor::register(&mut registry);
            registry
        },
        resources: |_| TestApp::<MonitorShell>::new().unwrap().harness().resources(),
        palette: ["Refresh processes", "Show process details", "Copy PID", "Stop process"],
    }
}
