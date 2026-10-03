//! Speed Test as a [`tilo_resource::Resource`]: host jobs are Effects; there
//! is no collaborative document vocabulary.

use serde::{Deserialize, Serialize};
use ui::framework::{ResourceApp, ResourceSurface, ViewCx};
use ui::tilo_actions::{Action, ActionError, ActionRegistry, ActionSpec, OperationKind};
use ui::tilo_resource::{Doc, ModelError, Resource};
use ui::widget::Element;

use crate::app::{SpeedOperation, SpeedTestApp, action};

/// No collaborative document mutations — results stay on this device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EmptyDocAction {}

impl Action for EmptyDocAction {
    fn apply(&self, _doc: &mut ui::tilo_actions::Doc) -> Result<(), ActionError> {
        Ok(())
    }
}

/// Ephemeral run state lives on the surface; the shared document is empty.
#[derive(Clone, Debug, Default)]
pub struct SpeedTestState;

/// The internet speed-test resource.
pub struct SpeedTest;

impl Resource for SpeedTest {
    const KIND: &'static str = "speed-test";
    const LABEL: &'static str = "Internet speed test";
    const ACTION_TYPE: &'static str = "speed_test.action";

    type State = SpeedTestState;
    type Action = EmptyDocAction;

    fn register(registry: &mut ActionRegistry) {
        registry.register::<EmptyDocAction>(Self::ACTION_TYPE);
    }

    fn read(_doc: &Doc) -> Result<SpeedTestState, ModelError> {
        Ok(SpeedTestState)
    }

    fn local_operations() -> Vec<ActionSpec> {
        // Stop is live: it only exists while a measurement is running, so it
        // stays on the surface's extra instead of the always-on door.
        [("start", "Run speed test", SpeedOperation::Start)]
            .into_iter()
            .filter_map(|(id, label, op)| action(id, label, &op))
            .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
            .collect()
    }
}

/// Framework app.
pub struct SpeedTestShell;

impl ResourceApp for SpeedTestShell {
    type Resource = SpeedTest;
    const ID: &'static str = "so.tilo.speed-test";
    const TITLE: &'static str = "Speed Test";
    const ICON: Option<&'static str> = Some("speed-test");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<SpeedTest>) -> Element {
        cx.surface(SpeedTestApp::new())
    }

    fn scenarios() -> &'static [&'static str] {
        &[
            "idle",
            "running",
            "complete",
            "error",
            "complete-narrow",
            "complete-short",
        ]
    }

    fn scenario_viewport(name: &str) -> Option<(u32, u32)> {
        match name {
            "complete-narrow" => Some((520, 820)),
            "complete-short" => Some((960, 560)),
            _ => None,
        }
    }
}

impl ResourceSurface<SpeedTest> for SpeedTestApp {
    /// Results stay on this device. An empty projection must not clear them.
    fn present(&mut self, _state: &std::rc::Rc<SpeedTestState>) {}

    fn stage(&mut self, scenario: Option<&str>) {
        self.model_mut().stage_scenario(scenario);
    }
}
