//! System Monitor as a [`tilo_resource::Resource`]: shared view preferences
//! sync in the document; live process rows stay hosted extras.
//!
//! Refresh is an Effect with no document delta (published via
//! [`Resource::local_operations`]). Per-process inspect / copy / signal ops
//! ride the live process catalogue — never a second hand-built root beside
//! [`tilo_resource::publish`].

use serde::{Deserialize, Serialize};
use ui::automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{
    Action, ActionError, ActionParam, ActionRegistry, ActionSpec, EncodedAction, ParamType,
    Resource as PublishedResource,
};
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource};
use ui::widget::Element;

use crate::app::{MonitorApp, refresh_effect};
use crate::doc::ViewDoc;

/// Collaborative view preferences. The process snapshot is not document
/// state — it rides live extras / standalone overlay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MonitorAction {
    SetTab { tab: String },
    SetSort { key: String, desc: bool },
    SetTreeMode { tree: bool },
    SetSearch { query: String },
    SetSelected { pid: Option<u64> },
}

impl Action for MonitorAction {
    fn apply(&self, doc: &mut ui::tilo_actions::Doc) -> Result<(), ActionError> {
        match self {
            Self::SetTab { tab } => ViewDoc::set_tab(doc, tab),
            Self::SetSort { key, desc } => {
                ViewDoc::set_sort_key(doc, key);
                ViewDoc::set_sort_desc(doc, *desc);
            }
            Self::SetTreeMode { tree } => ViewDoc::set_tree_mode(doc, *tree),
            Self::SetSearch { query } => ViewDoc::set_search(doc, query),
            Self::SetSelected { pid } => {
                ViewDoc::set_selected_pid(doc, pid.and_then(|p| u32::try_from(p).ok()));
            }
        }
        Ok(())
    }
}

/// View projection stored in the shared document.
#[derive(Clone, Debug)]
pub struct MonitorState {
    pub tab: String,
    pub sort_key: String,
    pub sort_desc: bool,
    pub tree_mode: bool,
    pub search: String,
    pub selected_pid: Option<u32>,
}

impl Default for MonitorState {
    fn default() -> Self {
        Self {
            tab: "processes".into(),
            sort_key: "cpu".into(),
            sort_desc: true,
            tree_mode: true,
            search: String::new(),
            selected_pid: None,
        }
    }
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
            tab: ViewDoc::tab(doc),
            sort_key: ViewDoc::sort_key(doc),
            sort_desc: ViewDoc::sort_desc(doc),
            tree_mode: ViewDoc::tree_mode(doc),
            search: ViewDoc::search(doc),
            selected_pid: ViewDoc::selected_pid(doc),
        })
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![
            ActionDecl::new(
                "set-tab",
                "Show tab",
                vec![
                    ActionParam::required("tab", ParamType::Text, "processes | resources")
                        .with_choices(["processes", "resources"]),
                ],
                |args, _| {
                    let tab = args
                        .text("tab")
                        .ok_or_else(|| "missing tab".to_owned())?
                        .to_owned();
                    Ok(MonitorAction::SetTab { tab })
                },
            )
            .describe("Switch the shared Monitor tab."),
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
            ActionDecl::new(
                "set-search",
                "Filter processes",
                vec![ActionParam::optional(
                    "query",
                    ParamType::Text,
                    "Process filter text, or omit to clear",
                )],
                |args, _| {
                    let query = args.text("query").unwrap_or("").to_owned();
                    Ok(MonitorAction::SetSearch { query })
                },
            )
            .describe("Set the shared process filter."),
            ActionDecl::new(
                "select-process",
                "Select process",
                vec![ActionParam::optional(
                    "pid",
                    ParamType::Integer,
                    "Process id, or omit to clear selection",
                )],
                |args, _| {
                    let pid = args
                        .integer("pid")
                        .and_then(|value| if value < 0 { None } else { Some(value as u64) });
                    Ok(MonitorAction::SetSelected { pid })
                },
            )
            .describe("Focus a process in the shared selection."),
        ]
    }

    fn local_operations() -> Vec<ActionSpec> {
        // Refresh is a palette Effect with no document delta. Per-process
        // Effects ride live process extras (or the standalone overlay).
        refresh_effect().into_iter().collect()
    }
}

/// Hosted-only extras that are not document state: live process rows.
/// The processes door stays on [`tilo_resource::publish`].
pub(crate) fn live_extras(app: &MonitorApp) -> Vec<PublishedResource> {
    app.live_process_extras()
}

/// Framework app.
pub struct MonitorShell;

impl ResourceApp for MonitorShell {
    type Resource = Monitor;
    const ID: &'static str = "so.tilo.monitor";
    const TITLE: &'static str = "System Monitor";
    const ICON: Option<&'static str> = Some("monitor");
    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1280, 800)
    }

    fn view(cx: &mut ViewCx<Monitor>) -> Element {
        // `new` seeds fabricated sample processes for screenshots. The live
        // window samples the machine.
        cx.surface(MonitorApp::new_live())
    }
}

impl ResourceSurface<Monitor> for MonitorApp {
    fn present(&mut self, state: &std::rc::Rc<MonitorState>) {
        // Adopt collaborative view prefs only — never clear the process table.
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Monitor>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    pub(crate) local: ui::ActionHost,
    pub(crate) framework: Option<Dispatcher<Monitor>>,
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
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Monitor>) {
        self.framework = Some(dispatcher);
        self.hosted = true;
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.hosted
    }

    pub(crate) fn dispatch(&mut self, doc: &mut AutoCommit, action: &MonitorAction) {
        let context = ui::tilo_actions::ActionContext::for_action(
            "so.tilo.monitor",
            "System Monitor",
            action,
        );
        match &self.framework {
            Some(dispatcher) => {
                self.last = tilo_actions::EncodedAction::encode(Monitor::ACTION_TYPE, action).ok();
                let _ = dispatcher.apply_now_with(doc, action, context);
            }
            None => self
                .local
                .dispatch_with_context(doc, Monitor::ACTION_TYPE, action, context),
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
pub fn provide_resources(state: &MonitorState) -> Vec<PublishedResource> {
    vec![ui::tilo_resource::publish::<Monitor>(
        "processes",
        "Processes",
        state,
    )]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::widget::Widget;

    #[allow(
        clippy::too_many_lines,
        reason = "one integration check covers publish tree, hosted extras, and live overlay"
    )]
    #[test]
    fn resource_publish_owns_process_door_and_hosted_keeps_only_live_extras() {
        let mut doc = AutoCommit::new();
        MonitorAction::SetTab {
            tab: "resources".into(),
        }
        .apply(&mut doc)
        .unwrap();
        MonitorAction::SetSort {
            key: "name".into(),
            desc: false,
        }
        .apply(&mut doc)
        .unwrap();
        MonitorAction::SetTreeMode { tree: false }
            .apply(&mut doc)
            .unwrap();
        MonitorAction::SetSearch {
            query: "chrome".into(),
        }
        .apply(&mut doc)
        .unwrap();
        MonitorAction::SetSelected { pid: Some(7) }
            .apply(&mut doc)
            .unwrap();
        doc.commit();

        let state = Monitor::read(&doc).expect("seeded monitor state");
        let published = ui::tilo_resource::publish::<Monitor>("processes", "Processes", &state);
        assert_eq!(published.kind, "monitor.processes");
        assert!(
            published
                .actions
                .iter()
                .any(|action| action.id == "set-tab"),
            "set-tab declaration must be on the framework resource"
        );
        assert!(
            published
                .actions
                .iter()
                .any(|action| action.id == "set-sort"),
            "set-sort declaration must be on the framework resource"
        );
        assert!(
            published
                .actions
                .iter()
                .any(|action| action.id == "set-tree-mode"),
            "set-tree-mode declaration must be on the framework resource"
        );
        assert!(
            published
                .actions
                .iter()
                .any(|action| action.id == "set-search"),
            "set-search declaration must be on the framework resource"
        );
        assert!(
            published
                .actions
                .iter()
                .any(|action| action.id == "select-process"),
            "select-process declaration must be on the framework resource"
        );
        assert!(
            published
                .actions
                .iter()
                .any(|action| action.id == "refresh"),
            "refresh Effect must be on the framework resource"
        );
        assert!(
            published.children.is_empty(),
            "shared projection must not carry live process rows"
        );
        assert_eq!(state.tab, "resources");
        assert_eq!(state.sort_key, "name");
        assert!(!state.sort_desc);
        assert!(!state.tree_mode);
        assert_eq!(state.search, "chrome");
        assert_eq!(state.selected_pid, Some(7));

        let mut app = MonitorApp::new();
        let mut seeded = doc.clone();
        app.sync(&mut seeded, &ui::automerge::ROOT);
        assert!(!app.is_hosted(), "standalone stays on the local host");
        let standalone = app.publish_resources(&seeded);
        assert_eq!(standalone.len(), 1);
        assert_eq!(standalone[0].kind, "monitor.processes");
        assert!(
            !standalone[0].children.is_empty(),
            "standalone overlays live process children"
        );
        assert!(
            standalone[0]
                .children
                .iter()
                .any(|child| child.kind == "monitor.process"),
            "standalone publishes process rows from the sampler snapshot"
        );

        app.mark_hosted_for_test();
        assert!(app.is_hosted());
        let extras = ui::resources::PublishesResources::resources(&app, &seeded);
        assert!(
            extras
                .iter()
                .all(|resource| resource.kind != "monitor.processes"),
            "hosted must not republish monitor.processes beside ResourceRoot: {:?}",
            extras.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );
        assert!(
            extras
                .iter()
                .any(|resource| resource.kind == "monitor.process"),
            "hosted still publishes live process rows: {:?}",
            extras.iter().map(|r| r.kind.as_str()).collect::<Vec<_>>()
        );

        let door = provide_resources(&state);
        assert_eq!(door[0].kind, "monitor.processes");
        assert!(door[0].children.is_empty());

        let mut local = AutoCommit::new();
        let mut standalone = MonitorApp::new();
        standalone.commit_view(
            &mut local,
            &MonitorAction::SetTab {
                tab: "resources".into(),
            },
        );
        assert!(
            standalone.last_document_action().is_some(),
            "ActionSink must record document actions on the local host"
        );
        assert_eq!(ViewDoc::tab(&local), "resources");

        // Empty present must not wipe the live process snapshot.
        let before = standalone.model_process_count();
        assert!(before > 0, "sample monitor starts with processes");
        standalone.adopt_resource_state(&MonitorState::default());
        assert_eq!(
            standalone.model_process_count(),
            before,
            "adopting empty view prefs must not clear the process table"
        );
    }
}
