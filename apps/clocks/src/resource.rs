//! Clocks as a [`tilo_resource::Resource`]. Timers and alarms are device-local
//! effects today; the document action vocabulary is empty until those migrate
//! onto the CRDT. The collection kind matches the published root.

use serde::{Deserialize, Serialize};
use ui::framework::{ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{Action, ActionError, ActionRegistry};
use ui::tilo_resource::{Doc, ModelError, Resource};
use ui::widget::Element;

use crate::ClockState;
use crate::app::ClocksApp;

/// No collaborative document mutations yet — clocks persist outside Automerge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EmptyDocAction {}

impl Action for EmptyDocAction {
    fn apply(&self, _doc: &mut ui::tilo_actions::Doc) -> Result<(), ActionError> {
        Ok(())
    }
}

/// The clocks collection resource.
pub struct Clocks;

impl Resource for Clocks {
    const KIND: &'static str = "clocks.collection";
    const LABEL: &'static str = "Clocks";
    const ACTION_TYPE: &'static str = "clocks.action";

    type State = ClockState;
    type Action = EmptyDocAction;

    fn register(registry: &mut ActionRegistry) {
        registry.register::<EmptyDocAction>(Self::ACTION_TYPE);
    }

    fn read(_doc: &Doc) -> Result<ClockState, ModelError> {
        // Live path starts empty; the widget still loads device-local state.
        Ok(ClockState::default())
    }

    fn local_operations() -> Vec<tilo_actions::ActionSpec> {
        // Per-timer/alarm effects need live state and stay on the widget tree
        // until they move here with document-backed state.
        Vec::new()
    }
}

/// Framework app: the clocks widget is the surface over [`Clocks`].
pub struct ClocksShell;

impl ResourceApp for ClocksShell {
    type Resource = Clocks;
    const ID: &'static str = "so.tilo.clocks";
    const TITLE: &'static str = "Clocks";
    const ICON: Option<&'static str> = Some("clocks");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<Clocks>) -> Element {
        cx.surface(ClocksApp::new())
    }

    fn scenarios() -> &'static [&'static str] {
        &[
            "empty",
            "running",
            "ringing",
            "world",
            "alarms",
            "stopwatch",
            "timers",
        ]
    }
}

impl ResourceSurface<Clocks> for ClocksApp {
    fn present(&mut self, _state: &std::rc::Rc<ClockState>) {}

    fn attach(&mut self, _cx: SurfaceCx<Clocks>) {}
}
