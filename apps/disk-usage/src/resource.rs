//! Disk Usage as a [`tilo_resource::Resource`]: scan jobs are Effects; recent
//! directories are the only shared projection.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use ui::framework::{ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{Action, ActionError, ActionRegistry, ActionSpec, OperationKind};
use ui::tilo_resource::{Doc, ModelError, Resource};
use ui::widget::Element;

use crate::app::{DiskUsageApp, DiskUsageOperation, operation_action};
use crate::doc::DiskUsageDoc;

/// No collaborative document mutations beyond the recent-path projection,
/// which the surface writes when a scan completes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EmptyDocAction {}

impl Action for EmptyDocAction {
    fn apply(&self, _doc: &mut ui::tilo_actions::Doc) -> Result<(), ActionError> {
        Ok(())
    }
}

/// Recent directories remembered in the shared document.
#[derive(Clone, Debug, Default)]
pub struct DiskUsageState {
    pub recent: Vec<PathBuf>,
}

/// The disk-usage analyzer resource.
pub struct DiskUsage;

impl Resource for DiskUsage {
    const KIND: &'static str = "disk-usage";
    const LABEL: &'static str = "Disk Usage Analyzer";
    const ACTION_TYPE: &'static str = "disk_usage.action";

    type State = DiskUsageState;
    type Action = EmptyDocAction;

    fn register(registry: &mut ActionRegistry) {
        registry.register::<EmptyDocAction>(Self::ACTION_TYPE);
    }

    fn read(doc: &Doc) -> Result<DiskUsageState, ModelError> {
        Ok(DiskUsageState {
            recent: DiskUsageDoc::recent(doc),
        })
    }

    fn local_operations() -> Vec<ActionSpec> {
        [
            ("choose", "Choose folder", DiskUsageOperation::Choose),
            ("home", "Show locations", DiskUsageOperation::Home),
            ("back", "Go back", DiskUsageOperation::Back),
        ]
        .into_iter()
        .filter_map(|(id, label, op)| operation_action(id, label, &op))
        .filter(|spec| matches!(spec.kind, OperationKind::View | OperationKind::Effect))
        .collect()
    }
}

/// Framework app.
pub struct DiskUsageShell;

impl ResourceApp for DiskUsageShell {
    type Resource = DiskUsage;
    const ID: &'static str = "so.tilo.disk-usage";
    const TITLE: &'static str = "Disk Usage Analyzer";
    const ICON: Option<&'static str> = Some("disk-usage");
    const SHOT_VIEWPORT: (u32, u32) = (1180, 760);

    fn view(cx: &mut ViewCx<DiskUsage>) -> Element {
        cx.surface(DiskUsageApp::new())
    }

    fn scenarios() -> &'static [&'static str] {
        &[
            "analysis",
            "analysis-narrow",
            "analysis-hover",
            "analysis-hover-narrow",
            "home",
            "home-narrow",
        ]
    }
}

impl ResourceSurface<DiskUsage> for DiskUsageApp {
    fn present(&mut self, _state: &std::rc::Rc<DiskUsageState>) {}

    fn attach(&mut self, _cx: SurfaceCx<DiskUsage>) {}
}
