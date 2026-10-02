//! Logs as a [`tilo_resource::Resource`]: shared filter/selection prefs sync
//! in the document; live journal entry rows stay hosted extras.
//!
//! Refresh, live-tail, and export are Effects with no document delta
//! (published via [`Resource::local_operations`]). Per-entry select ops ride
//! the live journal catalogue — never a second hand-built root beside
//! [`tilo_resource::publish`].

use serde::{Deserialize, Serialize};
use ui::automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{
    Action, ActionError, ActionParam, ActionRegistry, ActionSpec, EncodedAction, OperationKind,
    ParamType, Resource as PublishedResource,
};
use ui::tilo_resource::{ActionDecl, Doc, ModelError, Resource};
use ui::widget::Element;

use crate::actions::{self, JOURNAL_RESOURCE_ID};
use crate::app::LogsApp;
use crate::doc::ViewDoc;
use crate::model::Filter;

/// Collaborative filter and selection preferences.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LogsAction {
    SetPriority { value: i64 },
    SetUnit { value: String },
    SetBoot { value: String },
    SetSearch { value: String },
    ClearFilters,
    SelectEntry { entry_id: String },
}

impl Action for LogsAction {
    fn apply(&self, doc: &mut ui::tilo_actions::Doc) -> Result<(), ActionError> {
        match self {
            Self::SetPriority { value } => ViewDoc::set_priority(doc, *value),
            Self::SetUnit { value } => ViewDoc::set_unit(doc, value),
            Self::SetBoot { value } => ViewDoc::set_boot(doc, value),
            Self::SetSearch { value } => ViewDoc::set_search(doc, value),
            Self::ClearFilters => ViewDoc::clear_filters(doc),
            Self::SelectEntry { entry_id } => {
                ViewDoc::set_selected_entry(doc, Some(entry_id.as_str()));
            }
        }
        Ok(())
    }
}

/// View projection stored in the shared document.
#[derive(Clone, Debug, Default)]
pub struct LogsState {
    pub filter: Filter,
    pub selected_entry: Option<String>,
}

/// The system journal resource.
pub struct Logs;

impl Resource for Logs {
    const KIND: &'static str = "logs.journal";
    const LABEL: &'static str = "System journal";
    const ACTION_TYPE: &'static str = "logs.action";

    type State = LogsState;
    type Action = LogsAction;

    fn register(registry: &mut ActionRegistry) {
        actions::register(registry);
        registry.register::<LogsAction>(Self::ACTION_TYPE);
    }

    fn read(doc: &Doc) -> Result<LogsState, ModelError> {
        Ok(LogsState {
            filter: ViewDoc::filter(doc),
            selected_entry: ViewDoc::selected_entry(doc),
        })
    }

    fn declarations() -> Vec<ActionDecl<Self>> {
        vec![
            ActionDecl::new(
                "set-severity",
                "Set severity threshold",
                vec![
                    ActionParam::required(
                        "priority",
                        ParamType::Integer,
                        "Severity threshold from 0 to 7",
                    )
                    .with_schema(serde_json::json!({ "minimum": 0, "maximum": 7 })),
                ],
                |args, _| {
                    let value = args.integer("priority").unwrap_or(7).clamp(0, 7);
                    Ok(LogsAction::SetPriority { value })
                },
            )
            .describe("Set the shared severity threshold (0 emergency only; 7 all)."),
            ActionDecl::new(
                "set-unit",
                "Filter by unit",
                vec![ActionParam::optional(
                    "unit",
                    ParamType::Text,
                    "Exact unit, or omit to show all",
                )],
                |args, _| {
                    Ok(LogsAction::SetUnit {
                        value: args.text("unit").unwrap_or_default().to_owned(),
                    })
                },
            )
            .describe("Set the shared unit filter."),
            ActionDecl::new(
                "set-boot",
                "Filter by boot",
                vec![ActionParam::optional(
                    "boot",
                    ParamType::Text,
                    "Exact boot id, or omit to show all",
                )],
                |args, _| {
                    Ok(LogsAction::SetBoot {
                        value: args.text("boot").unwrap_or_default().to_owned(),
                    })
                },
            )
            .describe("Set the shared boot filter."),
            ActionDecl::new(
                "set-search",
                "Search journal",
                vec![ActionParam::optional(
                    "query",
                    ParamType::Text,
                    "Text to find in messages, units, or identifiers",
                )],
                |args, _| {
                    Ok(LogsAction::SetSearch {
                        value: args.text("query").unwrap_or("").to_owned(),
                    })
                },
            )
            .describe("Set the shared journal search string."),
            ActionDecl::new("clear-filters", "Clear filters", vec![], |_, _| {
                Ok(LogsAction::ClearFilters)
            })
            .describe("Clear every shared journal filter."),
            ActionDecl::new(
                "select-entry",
                "Select entry",
                vec![ActionParam::required(
                    "entry_id",
                    ParamType::Text,
                    "Stable logs.entry.* resource id",
                )],
                |args, _| {
                    let entry_id = args
                        .text("entry_id")
                        .ok_or_else(|| "missing entry_id".to_owned())?
                        .to_owned();
                    Ok(LogsAction::SelectEntry { entry_id })
                },
            )
            .describe("Focus an entry in the shared selection."),
        ]
    }

    fn local_operations() -> Vec<ActionSpec> {
        // Effects with no document delta. Per-entry Views ride live extras
        // (or the standalone overlay).
        actions::journal_effect_specs(false, true)
            .into_iter()
            .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
            .collect()
    }
}

/// Hosted-only extras that are not document state: live journal entry rows.
/// The journal door stays on [`tilo_resource::publish`].
pub(crate) fn live_extras(app: &LogsApp) -> Vec<PublishedResource> {
    app.live_entry_extras()
}

/// Framework app.
pub struct LogsShell;

impl ResourceApp for LogsShell {
    type Resource = Logs;
    const ID: &'static str = "so.tilo.logs";
    const TITLE: &'static str = "Logs";
    const ICON: Option<&'static str> = Some("logs");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<Logs>) -> Element {
        // The live window reads the journal. An empty list would show a
        // blank viewer even when the host has entries.
        let app = match crate::journal::load_live(2000) {
            Ok(entries) => LogsApp::new(entries, true),
            Err(error) => LogsApp::new_with_source_error(Vec::new(), true, error),
        };
        cx.surface(app)
    }
}

impl ResourceSurface<Logs> for LogsApp {
    fn present(&mut self, state: &std::rc::Rc<LogsState>) {
        // Adopt collaborative view prefs only — never clear the journal cache.
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Logs>) {
        self.attach_framework(cx.dispatcher());
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher.
pub(crate) struct ActionSink {
    pub(crate) local: ui::ActionHost,
    pub(crate) framework: Option<Dispatcher<Logs>>,
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
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Logs>) {
        self.framework = Some(dispatcher);
        self.hosted = true;
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.hosted
    }

    pub(crate) fn dispatch(&mut self, doc: &mut AutoCommit, action: &LogsAction) {
        let context = ui::tilo_actions::ActionContext::for_action("so.tilo.logs", "Logs", action);
        match &self.framework {
            Some(dispatcher) => {
                self.last = tilo_actions::EncodedAction::encode(Logs::ACTION_TYPE, action).ok();
                let _ = dispatcher.apply_now_with(doc, action, context);
            }
            None => self
                .local
                .dispatch_with_context(doc, Logs::ACTION_TYPE, action, context),
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
pub fn provide_resources(state: &LogsState) -> Vec<PublishedResource> {
    vec![ui::tilo_resource::publish::<Logs>(
        JOURNAL_RESOURCE_ID,
        "System journal",
        state,
    )]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::widget::Widget;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario / publish-ownership assertion kept top-to-bottom"
    )]
    fn resource_publish_owns_journal_door_and_hosted_keeps_only_live_extras() {
        let mut doc = AutoCommit::new();
        LogsAction::SetPriority { value: 3 }
            .apply(&mut doc)
            .unwrap();
        LogsAction::SetUnit {
            value: "shell.service".into(),
        }
        .apply(&mut doc)
        .unwrap();
        LogsAction::SetSearch {
            value: "iroh".into(),
        }
        .apply(&mut doc)
        .unwrap();
        LogsAction::SelectEntry {
            entry_id: "logs.entry.test".into(),
        }
        .apply(&mut doc)
        .unwrap();
        doc.commit();

        let state = Logs::read(&doc).expect("seeded logs state");
        let published =
            ui::tilo_resource::publish::<Logs>(JOURNAL_RESOURCE_ID, "System journal", &state);
        assert_eq!(published.kind, "logs.journal");
        for id in [
            "set-severity",
            "set-unit",
            "set-boot",
            "set-search",
            "clear-filters",
            "select-entry",
            "refresh",
            "set-live-tail",
            "export-filtered-jsonl",
        ] {
            assert!(
                published.actions.iter().any(|action| action.id == id),
                "{id} must be on the framework resource"
            );
        }
        assert!(
            published.children.is_empty(),
            "shared projection must not carry live journal rows"
        );
        assert_eq!(state.filter.min_priority, 3);
        assert_eq!(state.filter.unit, "shell.service");
        assert_eq!(state.filter.search, "iroh");
        assert_eq!(state.selected_entry.as_deref(), Some("logs.entry.test"));

        let mut app = LogsApp::new(crate::journal::sample_entries(), false);
        let mut seeded = doc.clone();
        app.sync(&mut seeded, &ui::automerge::ROOT);
        assert!(!app.is_hosted(), "standalone stays on the local host");
        let standalone = app.publish_resources(&seeded);
        assert_eq!(standalone.len(), 1);
        assert_eq!(standalone[0].kind, "logs.journal");
        assert!(
            standalone[0]
                .children
                .iter()
                .any(|child| child.kind == "logs.entry"),
            "standalone publishes journal entry rows from the local cache"
        );

        let entry_id = standalone[0]
            .children
            .iter()
            .find(|child| child.kind == "logs.entry")
            .map(|child| child.id.as_str().to_owned())
            .expect("journal entry");
        app.mark_hosted_for_test();
        assert!(app.is_hosted());
        assert!(
            ui::resources::resource(&app, &seeded, crate::actions::JOURNAL_RESOURCE_ID).is_none()
        );
        assert!(
            ui::resources::resource(&app, &seeded, &entry_id).is_some(),
            "a hosted journal entry stays the published row"
        );
        let extras = ui::resources::PublishesResources::resources(&app, &seeded);
        assert!(
            extras
                .iter()
                .all(|resource| resource.kind != "logs.journal"),
            "hosted must not republish logs.journal beside ResourceRoot: {:?}",
            extras.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
        );
        assert!(
            extras.iter().any(|resource| resource.kind == "logs.entry"),
            "hosted still publishes live journal entry rows: {:?}",
            extras.iter().map(|r| r.kind.as_str()).collect::<Vec<_>>()
        );

        let door = provide_resources(&state);
        assert_eq!(door[0].kind, "logs.journal");
        assert!(door[0].children.is_empty());

        let mut local = AutoCommit::new();
        let mut standalone = LogsApp::new(crate::journal::sample_entries(), false);
        standalone.commit_view(
            &mut local,
            &LogsAction::SetSearch {
                value: "privacy".into(),
            },
        );
        assert!(
            standalone.last_document_action().is_some(),
            "ActionSink must record document actions on the local host"
        );
        assert_eq!(ViewDoc::filter(&local).search, "privacy");

        // Empty present must not wipe the local journal cache.
        let before = standalone.entry_count();
        assert!(before > 0, "sample journal starts with entries");
        standalone.adopt_resource_state(&LogsState::default());
        assert_eq!(
            standalone.entry_count(),
            before,
            "adopting empty view prefs must not clear the journal cache"
        );
    }
}
