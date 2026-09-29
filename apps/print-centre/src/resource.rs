//! Print Centre as a [`tilo_resource::Resource`]: print/scan jobs are Effects
//! and View navigation; there is no collaborative document vocabulary.

use serde::{Deserialize, Serialize};
use ui::framework::{ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
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
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

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

impl ResourceSurface<PrintCentreResource> for PrintCentre {
    /// Queues and scanners are this machine's. An empty projection must not
    /// clear a snapshot the surface already loaded.
    fn present(&mut self, _state: &std::rc::Rc<PrintCentreState>) {}

    fn attach(&mut self, _cx: SurfaceCx<PrintCentreResource>) {
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
}
