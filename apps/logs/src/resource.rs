//! Logs as a [`tilo_resource::Resource`]: journal access is Effects; filters
//! are View operations. There is no collaborative document vocabulary.

use serde::{Deserialize, Serialize};
use ui::framework::{ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{Action, ActionError, ActionRegistry, ActionSpec, OperationKind};
use ui::tilo_resource::{Doc, ModelError, Resource};
use ui::widget::Element;

use crate::actions;
use crate::app::LogsApp;
use crate::model::Filter;

/// No collaborative document mutations — the journal is a host projection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EmptyDocAction {}

impl Action for EmptyDocAction {
    fn apply(&self, _doc: &mut ui::tilo_actions::Doc) -> Result<(), ActionError> {
        Ok(())
    }
}

/// Local filter projection (not stored as a collaborative record).
#[derive(Clone, Debug, Default)]
pub struct LogsState {
    pub filter: Filter,
}

/// The system journal resource.
pub struct Logs;

impl Resource for Logs {
    const KIND: &'static str = "logs.journal";
    const LABEL: &'static str = "System journal";
    const ACTION_TYPE: &'static str = "logs.action";

    type State = LogsState;
    type Action = EmptyDocAction;

    fn register(registry: &mut ActionRegistry) {
        actions::register(registry);
        registry.register::<EmptyDocAction>(Self::ACTION_TYPE);
    }

    fn read(_doc: &Doc) -> Result<LogsState, ModelError> {
        Ok(LogsState::default())
    }

    fn local_operations() -> Vec<ActionSpec> {
        actions::resources(&[], &Filter::default(), false, true)
            .into_iter()
            .flat_map(|resource| resource.actions)
            .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
            .collect()
    }
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
    fn present(&mut self, _state: &std::rc::Rc<LogsState>) {}

    fn attach(&mut self, _cx: SurfaceCx<Logs>) {}
}
