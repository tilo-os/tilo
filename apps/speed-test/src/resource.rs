//! Speed Test as a [`tilo_resource::Resource`]: host jobs are Effects; there
//! is no collaborative document vocabulary.

use serde::{Deserialize, Serialize};
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
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
        // stays on the hosted extra instead of the always-on door.
        [
            ("start", "Run speed test", SpeedOperation::Start),
            (
                "change-region",
                "Change test region",
                SpeedOperation::ChangeRegion,
            ),
        ]
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
    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1180, 760)
    }

    fn view(cx: &mut ViewCx<SpeedTest>) -> Element {
        cx.surface(SpeedTestApp::new())
    }

    fn scenarios() -> &'static [&'static str] {
        &[
            "idle",
            "running",
            "complete",
            "complete-narrow",
            "complete-short",
        ]
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher. Results stay on this device.
#[derive(Default)]
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<SpeedTest>>,
    /// Set by [`Self::attach`]; tests can flip it without a real dispatcher.
    hosted: bool,
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<SpeedTest>) {
        self.framework = Some(dispatcher);
        self.hosted = true;
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.hosted || self.framework.is_some()
    }

    pub(crate) fn take_committed(&mut self) -> Option<Vec<ui::automerge::ChangeHash>> {
        self.local.take_committed()
    }

    /// Test-only: mark hosted without constructing a framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.hosted = true;
    }
}

impl ResourceSurface<SpeedTest> for SpeedTestApp {
    /// Results stay on this device. An empty projection must not clear them.
    fn present(&mut self, _state: &std::rc::Rc<SpeedTestState>) {}

    fn attach(&mut self, cx: SurfaceCx<SpeedTest>) {
        self.actions.attach(cx.dispatcher());
        self.mark_framework_hosted();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::resources::PublishesResources;

    #[test]
    fn publish_owns_start_and_hosted_keeps_stop_only_while_running() {
        let published = ui::tilo_resource::publish::<SpeedTest>(
            "speed-test",
            "Internet speed test",
            &SpeedTestState,
        );
        let ids: Vec<&str> = published
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect();
        assert!(ids.contains(&"start"), "start rides publish: {ids:?}");
        assert!(
            ids.contains(&"change-region"),
            "change-region rides publish: {ids:?}"
        );
        assert!(!ids.contains(&"stop"), "stop stays live: {ids:?}");

        let mut idle = SpeedTestApp::new();
        assert!(!idle.is_framework_hosted());
        let standalone = idle.resources(&ui::automerge::AutoCommit::new());
        assert_eq!(standalone.len(), 1);
        assert_eq!(standalone[0].kind, SpeedTest::KIND);

        idle.mark_hosted_for_test();
        assert!(
            idle.resources(&ui::automerge::AutoCommit::new()).is_empty(),
            "idle hosted adds no second speed-test root"
        );

        let mut running = SpeedTestApp::demo("running");
        running.mark_hosted_for_test();
        let extras = running.resources(&ui::automerge::AutoCommit::new());
        assert_eq!(extras.len(), 1);
        assert_ne!(extras[0].kind, SpeedTest::KIND);
        assert_eq!(extras[0].id.as_str(), "speed-test.run");
        assert!(
            extras[0]
                .actions
                .iter()
                .any(|action| action.id.as_str() == "stop")
        );
    }

    #[test]
    fn attach_hosted_surface_does_not_republish_the_speed_test_root() {
        let mut idle = SpeedTestApp::new();
        assert!(!idle.is_framework_hosted());
        assert!(!idle.sink_is_hosted());
        let doc = ui::automerge::AutoCommit::new();
        let standalone = idle.resources(&doc);
        assert_eq!(standalone.len(), 1);
        assert_eq!(standalone[0].kind, SpeedTest::KIND);

        idle.mark_hosted_for_test();
        assert!(idle.is_framework_hosted());
        assert!(idle.sink_is_hosted());
        assert!(
            idle.resources(&doc).is_empty(),
            "hosted attach must not republish the speed-test root"
        );

        let mut running = SpeedTestApp::demo("running");
        running.mark_hosted_for_test();
        let extras = running.resources(&doc);
        assert_eq!(extras.len(), 1);
        assert_ne!(extras[0].kind, SpeedTest::KIND);
        assert!(
            extras[0]
                .actions
                .iter()
                .any(|action| action.id.as_str() == "stop"),
            "an in-flight run stays on the widget"
        );
    }

    #[test]
    fn present_empty_does_not_wipe_a_completed_result() {
        let mut app = SpeedTestApp::demo("complete");
        assert!(app.has_result());
        ResourceSurface::<SpeedTest>::present(&mut app, &std::rc::Rc::new(SpeedTestState));
        assert!(
            app.has_result(),
            "empty present must not wipe a local result"
        );
    }
}
