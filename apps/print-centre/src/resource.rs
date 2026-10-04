//! Print Centre as a [`tilo_resource::Resource`]: print/scan jobs are Effects
//! and View navigation; there is no collaborative document vocabulary.

use serde::{Deserialize, Serialize};
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{Action, ActionError, ActionRegistry, ActionSpec, OperationKind};
use ui::tilo_resource::{Doc, ModelError, Resource};
use ui::widget::Element;

use crate::surface::{PrintCentre, resource_action};

/// No collaborative document mutations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EmptyDocAction {}

impl Action for EmptyDocAction {
    fn apply(&self, _doc: &mut ui::tilo_actions::Doc) -> Result<(), ActionError> {
        Ok(())
    }
}

#[derive(Clone, Debug, Default)]
pub struct PrintCentreState;

/// The print-centre dashboard resource.
pub struct PrintCentreResource;

impl Resource for PrintCentreResource {
    const KIND: &'static str = "print-centre.dashboard";
    const LABEL: &'static str = "Print Centre";
    const ACTION_TYPE: &'static str = "print_centre.action";

    type State = PrintCentreState;
    type Action = EmptyDocAction;

    fn register(registry: &mut ActionRegistry) {
        registry.register::<EmptyDocAction>(Self::ACTION_TYPE);
    }

    fn read(_doc: &Doc) -> Result<PrintCentreState, ModelError> {
        Ok(PrintCentreState)
    }

    fn local_operations() -> Vec<ActionSpec> {
        [
            (
                "refresh",
                "Refresh printers and documents",
                OperationKind::Effect,
                "refresh",
            ),
            (
                "open-dashboard",
                "Open dashboard",
                OperationKind::View,
                "dashboard",
            ),
            (
                "open-queue",
                "Open print queue",
                OperationKind::View,
                "queue",
            ),
            (
                "open-history",
                "Open print history",
                OperationKind::View,
                "history",
            ),
            (
                "run-diagnostics",
                "Run printer diagnostics",
                OperationKind::View,
                "diagnostics",
            ),
            (
                "find-printers",
                "Find network printers",
                OperationKind::Effect,
                "discover",
            ),
            (
                "print-document",
                "Print a document",
                OperationKind::View,
                "print",
            ),
            (
                "scan-document",
                "Scan a document",
                OperationKind::View,
                "scan",
            ),
        ]
        .into_iter()
        .filter_map(|(id, label, kind, command)| resource_action(id, label, kind, command))
        .collect()
    }
}

/// Framework app.
pub struct PrintCentreShell;

impl ResourceApp for PrintCentreShell {
    type Resource = PrintCentreResource;
    const ID: &'static str = "so.tilo.print-centre";
    const TITLE: &'static str = "Print Centre";
    const ICON: Option<&'static str> = Some("print-centre");
    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1280, 800)
    }

    fn view(cx: &mut ViewCx<PrintCentreResource>) -> Element {
        cx.surface(PrintCentre::new())
    }

    fn scenarios() -> &'static [&'static str] {
        &[
            "active",
            "attention",
            "empty",
            "narrow",
            "queue",
            "device",
            "discovery",
            "print",
            "print-narrow",
            "scan",
            "scan-result",
            "scan-result-narrow",
            "history",
            "diagnostics",
            "service-error",
            "service-error-narrow",
        ]
    }
}

/// Framework dispatcher when hosted; the local action host for standalone
/// tests and the screenshot launcher. Queues and scanners stay on this machine.
#[derive(Default)]
pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<PrintCentreResource>>,
    /// Set by [`Self::attach`]; tests can flip it without a real dispatcher.
    hosted: bool,
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<PrintCentreResource>) {
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

impl ResourceSurface<PrintCentreResource> for PrintCentre {
    /// Queues and scanners are this machine's. An empty projection must not
    /// clear a snapshot the surface already loaded.
    fn present(&mut self, _state: &std::rc::Rc<PrintCentreState>) {}

    fn attach(&mut self, cx: SurfaceCx<PrintCentreResource>) {
        self.actions.attach(cx.dispatcher());
        self.mark_framework_hosted();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_owns_dashboard_door() {
        let published = ui::tilo_resource::publish::<PrintCentreResource>(
            "print-centre",
            "Print Centre",
            &PrintCentreState,
        );
        let ids: Vec<&str> = published
            .actions
            .iter()
            .map(|action| action.id.as_str())
            .collect();
        for expected in ["refresh", "open-queue", "find-printers", "print-document"] {
            assert!(
                ids.contains(&expected),
                "palette `{expected}` must ride publish: {ids:?}"
            );
        }
    }

    #[test]
    fn attach_hosted_surface_does_not_republish_the_dashboard_root() {
        let mut centre = PrintCentre::demo(crate::surface::DemoScenario::Empty);
        let doc = automerge::AutoCommit::new();
        assert!(!centre.is_framework_hosted());
        assert!(!centre.sink_is_hosted());
        let standalone = ui::resources::resources(&centre, &doc);
        assert_eq!(standalone.len(), 1);
        assert_eq!(standalone[0].kind, PrintCentreResource::KIND);

        centre.mark_hosted_for_test();
        assert!(centre.is_framework_hosted());
        assert!(centre.sink_is_hosted());
        assert!(
            ui::resources::resources(&centre, &doc).is_empty(),
            "hosted attach must not republish print-centre.dashboard"
        );
    }
}
