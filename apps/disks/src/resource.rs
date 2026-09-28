//! Disks as a [`tilo_resource::Resource`]: format/mount/eject are Effects; the
//! shared document holds a topology projection plus view selection.

use serde::{Deserialize, Serialize};
use ui::automerge::AutoCommit;
use ui::framework::{Dispatcher, ResourceApp, ResourceSurface, SurfaceCx, ViewCx};
use ui::tilo_actions::{Action, ActionError, ActionRegistry, ActionSpec};
use ui::tilo_resource::{Doc, ModelError, Resource};
use ui::widget::Element;

use crate::app::DisksApp;
use crate::doc::DisksDoc;

/// Shared disks document mutations: selection, tab, confirmation, status, and
/// the probed topology snapshot. Mount and format stay effects; they change
/// one machine, not the document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DisksAction {
    SetSelected {
        index: u64,
    },
    /// `-1` is the whole disk.
    SetSelectedPart {
        part: i64,
    },
    SetTab {
        tab: String,
    },
    SetPending {
        pending: String,
    },
    SetStatus {
        status: String,
    },
    WriteTopology {
        json: String,
    },
}

impl Action for DisksAction {
    fn apply(&self, doc: &mut ui::tilo_actions::Doc) -> Result<(), ActionError> {
        match self {
            Self::SetSelected { index } => DisksDoc::set_selected(doc, *index as usize),
            Self::SetSelectedPart { part } => {
                let part = (*part >= 0).then_some(*part as usize);
                DisksDoc::set_selected_part(doc, part);
            }
            Self::SetTab { tab } => DisksDoc::set_tab(doc, tab),
            Self::SetPending { pending } => DisksDoc::set_pending(doc, pending),
            Self::SetStatus { status } => DisksDoc::set_status(doc, status),
            Self::WriteTopology { json } => DisksDoc::write_topology_json(doc, json),
        }
        Ok(())
    }
}

/// Selection / tab projection from the shared document.
#[derive(Clone, Debug, Default)]
pub struct DisksState {
    pub selected: usize,
    pub selected_part: Option<usize>,
    pub tab: String,
    pub pending: String,
    pub status: String,
}

/// The storage-devices resource.
pub struct Disks;

impl Resource for Disks {
    const KIND: &'static str = "disks.system";
    const LABEL: &'static str = "Storage devices";
    const ACTION_TYPE: &'static str = "disks.action";

    type State = DisksState;
    type Action = DisksAction;

    fn register(registry: &mut ActionRegistry) {
        registry.register::<DisksAction>(Self::ACTION_TYPE);
    }

    fn read(doc: &Doc) -> Result<DisksState, ModelError> {
        Ok(DisksState {
            selected: DisksDoc::selected(doc),
            selected_part: DisksDoc::selected_part(doc),
            tab: DisksDoc::tab(doc),
            pending: DisksDoc::pending(doc),
            status: DisksDoc::status(doc),
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
    fn present(&mut self, state: &std::rc::Rc<DisksState>) {
        self.adopt_shared(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Disks>) {
        self.attach_framework(cx.dispatcher());
    }
}

pub(crate) struct ActionSink {
    local: ui::ActionHost,
    framework: Option<Dispatcher<Disks>>,
}

impl Default for ActionSink {
    fn default() -> Self {
        Self {
            local: ui::ActionHost::new(),
            framework: None,
        }
    }
}

impl ActionSink {
    pub(crate) fn attach(&mut self, dispatcher: Dispatcher<Disks>) {
        self.framework = Some(dispatcher);
    }

    pub(crate) fn dispatch(&mut self, doc: &mut AutoCommit, action: &DisksAction) {
        let context = ui::tilo_actions::ActionContext::for_action("so.tilo.disks", "Disks", action);
        match &self.framework {
            Some(dispatcher) => {
                let _ = dispatcher.apply_now_with(doc, action, context);
            }
            None => self
                .local
                .dispatch_with_context(doc, Disks::ACTION_TYPE, action, context),
        }
    }
}
