//! Containers as a [`tilo_resource::Resource`]: Podman jobs are Effects; the
//! shared document holds only local view state (section, selection, search).

use serde::{Deserialize, Serialize};
use ui::framework::{ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{Action, ActionError, ActionRegistry, ActionSpec, OperationKind};
use ui::tilo_resource::{Doc, ModelError, Resource};
use ui::widget::Element;

use crate::app::ContainersApp;
use crate::doc::{DetailTab, Section, ViewDoc};

/// No collaborative document mutations — view fields are written by the
/// surface's existing view helpers; engine work is Effects.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EmptyDocAction {}

impl Action for EmptyDocAction {
    fn apply(&self, _doc: &mut ui::tilo_actions::Doc) -> Result<(), ActionError> {
        Ok(())
    }
}

/// View projection stored in the shared document.
#[derive(Clone, Debug)]
pub struct ContainersState {
    pub section: Section,
    pub selected: Option<String>,
    pub tab: DetailTab,
    pub search: String,
}

impl Default for ContainersState {
    fn default() -> Self {
        Self {
            section: Section::Containers,
            selected: None,
            tab: DetailTab::Logs,
            search: String::new(),
        }
    }
}

/// The containers engine resource.
pub struct Containers;

impl Resource for Containers {
    const KIND: &'static str = "containers.engine";
    const LABEL: &'static str = "Containers";
    const ACTION_TYPE: &'static str = "containers.action";

    type State = ContainersState;
    type Action = EmptyDocAction;

    fn register(registry: &mut ActionRegistry) {
        registry.register::<EmptyDocAction>(Self::ACTION_TYPE);
    }

    fn read(doc: &Doc) -> Result<ContainersState, ModelError> {
        Ok(ContainersState {
            section: ViewDoc::section(doc),
            selected: ViewDoc::selected(doc),
            tab: ViewDoc::tab(doc),
            search: ViewDoc::search(doc),
        })
    }

    fn local_operations() -> Vec<ActionSpec> {
        match ActionSpec::operation(
            "create",
            "Create container",
            OperationKind::View,
            "containers.engine",
            &["create"],
        ) {
            Ok(spec) => vec![spec],
            Err(_) => Vec::new(),
        }
    }
}

/// Framework app.
pub struct ContainersShell;

impl ResourceApp for ContainersShell {
    type Resource = Containers;
    const ID: &'static str = "so.tilo.containers";
    const TITLE: &'static str = "Containers";
    const ICON: Option<&'static str> = Some("containers");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<Containers>) -> Element {
        cx.surface(ContainersApp::new(true))
    }

    fn scenarios() -> &'static [&'static str] {
        &["confirm", "prompt"]
    }
}

impl ResourceSurface<Containers> for ContainersApp {
    fn present(&mut self, _state: &std::rc::Rc<ContainersState>) {}

    fn attach(&mut self, _cx: SurfaceCx<Containers>) {}
}
