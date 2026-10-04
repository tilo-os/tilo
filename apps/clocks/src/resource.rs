//! Clocks as a [`tilo_resource::Resource`]. Timers and alarms are device-local
//! effects today; the document action vocabulary is empty until those migrate
//! onto the CRDT. The collection kind matches the published root.

use serde::{Deserialize, Serialize};
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
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
        // Creates and stopwatch transport do not name a live row. Per-item
        // effects and the floating-overlay polarity stay on the widget.
        crate::actions::collection_local_operations()
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
            "empty-world",
            "empty-alarms",
            "running",
            "ringing",
            "world",
            "alarms",
            "stopwatch",
            "timers",
            "running-narrow",
            "ringing-narrow",
            "world-narrow",
            "alarms-narrow",
            "stopwatch-narrow",
            "timers-narrow",
            "new-alarm",
            "new-timer",
            "new-alarm-narrow",
            "new-timer-narrow",
        ]
    }

    fn scenario_viewport(name: &str) -> Option<(u32, u32)> {
        name.ends_with("-narrow").then_some((520, 820))
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher. Clocks has no document action yet.
#[derive(Default)]
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<Clocks>>,
    /// Set by [`Self::attach`]; tests can flip it without a real dispatcher.
    hosted: bool,
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Clocks>) {
        self.framework = Some(dispatcher);
        self.hosted = true;
    }

    pub(crate) fn is_hosted(&self) -> bool {
        self.hosted || self.framework.is_some()
    }

    pub(crate) fn take_committed(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.local.take_committed()
    }

    /// Test-only: mark hosted without constructing a framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.hosted = true;
    }
}

impl ResourceSurface<Clocks> for ClocksApp {
    /// Alarms and timers are device-local. An empty projection must not wipe
    /// an unpublished local collection.
    fn present(&mut self, _state: &std::rc::Rc<ClockState>) {}

    fn attach(&mut self, cx: SurfaceCx<Clocks>) {
        self.actions.attach(cx.dispatcher());
        self.mark_framework_hosted();
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario.unwrap_or_default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::resources::PublishesResources;
    use ui::widget::Widget;

    #[test]
    fn publish_owns_collection_door_and_hosted_keeps_only_live_rows() {
        let published =
            ui::tilo_resource::publish::<Clocks>("clocks", "Clocks", &ClockState::default());
        assert_eq!(published.kind, Clocks::KIND);
        let ids: Vec<&str> = published
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect();
        for expected in [
            "new-timer",
            "new-alarm",
            "add-new-york",
            "toggle",
            "lap",
            "reset",
        ] {
            assert!(
                ids.contains(&expected),
                "palette `{expected}` must ride publish via local_operations: {ids:?}"
            );
        }
        assert!(
            !ids.contains(&"set-floating"),
            "floating polarity stays on the live stopwatch: {ids:?}"
        );

        let mut app = ClocksApp::scenario("running");
        let mut state = ui::State::new();
        Widget::sync(&mut app, &mut state.doc, &state.root);
        assert!(!app.is_framework_hosted());
        let standalone = app.resources(&state.doc);
        assert_eq!(standalone.len(), 1);
        assert_eq!(standalone[0].id.as_str(), "clocks");

        app.mark_hosted_for_test();
        let extras = app.resources(&state.doc);
        assert!(
            extras.iter().all(|resource| {
                resource.kind != Clocks::KIND && resource.id.as_str() != "clocks"
            }),
            "hosted must not republish clocks.collection beside ResourceRoot: {:?}",
            extras
                .iter()
                .map(|resource| resource.id.as_str())
                .collect::<Vec<_>>()
        );
        assert!(
            extras
                .iter()
                .any(|resource| resource.kind == "clocks.timer"),
            "hosted keeps the running timer"
        );
        let stopwatch = extras
            .iter()
            .find(|resource| resource.id.as_str() == "clocks.stopwatch")
            .expect("stopwatch extra");
        assert!(
            stopwatch
                .actions
                .iter()
                .any(|action| action.id.as_str() == "set-floating")
        );
        assert!(
            !stopwatch
                .actions
                .iter()
                .any(|action| action.id.as_str() == "lap"),
            "lap already rides publish"
        );
    }

    #[test]
    fn attach_hosted_surface_does_not_republish_the_collection_root() {
        let mut app = ClocksApp::scenario("running");
        let mut state = ui::State::new();
        Widget::sync(&mut app, &mut state.doc, &state.root);
        assert!(!app.is_framework_hosted());
        assert!(!app.sink_is_hosted());
        let standalone = app.resources(&state.doc);
        assert_eq!(standalone.len(), 1);
        assert_eq!(standalone[0].kind, Clocks::KIND);

        app.mark_hosted_for_test();
        assert!(app.is_framework_hosted());
        assert!(app.sink_is_hosted());
        let extras = app.resources(&state.doc);
        assert!(
            extras.iter().all(|resource| {
                resource.kind != Clocks::KIND && resource.id.as_str() != "clocks"
            }),
            "hosted attach must not republish clocks.collection: {:?}",
            extras
                .iter()
                .map(|resource| resource.id.as_str())
                .collect::<Vec<_>>()
        );
        assert!(
            extras
                .iter()
                .any(|resource| resource.kind == "clocks.timer"),
            "hosted keeps the running timer on the widget"
        );
    }

    #[test]
    fn present_empty_does_not_wipe_unpublished_local_timers() {
        let mut app = ClocksApp::scenario("running");
        assert!(app.timer_count() > 0);
        let before = app.timer_count();
        ResourceSurface::<Clocks>::present(&mut app, &std::rc::Rc::new(ClockState::default()));
        assert_eq!(
            app.timer_count(),
            before,
            "empty present must not wipe unpublished local timers"
        );
    }
}
