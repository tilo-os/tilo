//! Disks as a [`tilo_resource::Resource`]: format/mount/eject are Effects; the
//! shared document holds a topology projection plus view selection.

use serde::{Deserialize, Serialize};
use ui::framework::{ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{Action, ActionError, ActionRegistry, ActionSpec};
use ui::tilo_resource::{Doc, ModelError, Resource};
use ui::widget::Element;

use crate::app::DisksApp;
use crate::doc::DisksDoc;

/// No collaborative document mutations — topology is a host projection
/// reconciled wholesale; disk jobs are Effects.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EmptyDocAction {}

impl Action for EmptyDocAction {
    fn apply(&self, _doc: &mut ui::tilo_actions::Doc) -> Result<(), ActionError> {
        Ok(())
    }
}

/// Selection / tab projection from the shared document.
#[derive(Clone, Debug, Default)]
pub struct DisksState {
    pub selected: usize,
    pub selected_part: Option<usize>,
    pub tab: String,
    pub topology_present: bool,
}

/// The storage-devices resource.
pub struct Disks;

impl Resource for Disks {
    const KIND: &'static str = "disks.system";
    const LABEL: &'static str = "Storage devices";
    const ACTION_TYPE: &'static str = "disks.action";

    type State = DisksState;
    type Action = EmptyDocAction;

    fn register(registry: &mut ActionRegistry) {
        registry.register::<EmptyDocAction>(Self::ACTION_TYPE);
    }

    fn read(doc: &Doc) -> Result<DisksState, ModelError> {
        Ok(DisksState {
            selected: DisksDoc::selected(doc),
            selected_part: DisksDoc::selected_part(doc),
            tab: DisksDoc::tab(doc),
            topology_present: DisksDoc::topology_json(doc).is_some(),
        })
    }

    fn local_operations() -> Vec<ActionSpec> {
        // Per-drive format/mount/eject Effects are bound to probed identities
        // and published by the surface when topology is known.
        Vec::new()
    }
}

/// Framework app.
pub struct DisksShell;

impl ResourceApp for DisksShell {
    type Resource = Disks;
    const ID: &'static str = "so.tilo.disks";
    const TITLE: &'static str = "Disks";
    const ICON: Option<&'static str> = Some("disks");
    const SHOT_VIEWPORT: (u32, u32) = (1280, 800);

    fn view(cx: &mut ViewCx<Disks>) -> Element {
        cx.surface(DisksApp::new())
    }
}

impl ResourceSurface<Disks> for DisksApp {
    fn present(&mut self, _state: &std::rc::Rc<DisksState>) {}

    fn attach(&mut self, _cx: SurfaceCx<Disks>) {}
}
