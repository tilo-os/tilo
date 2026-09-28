//! System Monitor as a [`tilo_resource::Resource`]: process control is Effects;
//! there is no collaborative document vocabulary.

use serde::{Deserialize, Serialize};
use ui::framework::{ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{Action, ActionError, ActionRegistry, ActionSpec, OperationKind};
use ui::tilo_resource::{Doc, ModelError, Resource};
use ui::widget::Element;

use crate::app::{MonitorApp, monitor_process_root_resource};

/// No collaborative document mutations — the process table is a host projection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EmptyDocAction {}

impl Action for EmptyDocAction {
    fn apply(&self, _doc: &mut ui::tilo_actions::Doc) -> Result<(), ActionError> {
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
pub struct MonitorState;

/// The live process catalogue resource.
pub struct Monitor;

impl Resource for Monitor {
    const KIND: &'static str = "monitor.processes";
    const LABEL: &'static str = "Processes";
    const ACTION_TYPE: &'static str = "monitor.action";

    type State = MonitorState;
    type Action = EmptyDocAction;

    fn register(registry: &mut ActionRegistry) {
        registry.register::<EmptyDocAction>(Self::ACTION_TYPE);
    }

    fn read(_doc: &Doc) -> Result<MonitorState, ModelError> {
        Ok(MonitorState)
    }

    fn local_operations() -> Vec<ActionSpec> {
        monitor_process_root_resource()
            .actions
            .into_iter()
            .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
            .collect()
    }
}

/// Framework app.
pub struct MonitorShell;

impl ResourceApp for MonitorShell {
    type Resource = Monitor;
    const ID: &'static str = "so.tilo.monitor";
    const TITLE: &'static str = "System Monitor";
    const ICON: Option<&'static str> = Some("monitor");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<Monitor>) -> Element {
        // `new` seeds fabricated sample processes for screenshots. The live
        // window samples the machine.
        cx.surface(MonitorApp::new_live())
    }
}

impl ResourceSurface<Monitor> for MonitorApp {
    fn present(&mut self, _state: &std::rc::Rc<MonitorState>) {}

    fn attach(&mut self, _cx: SurfaceCx<Monitor>) {}
}
