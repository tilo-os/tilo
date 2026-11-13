//! Shared input capability passed through Calendar's internal components.
//!
//! The public UI widget contract necessarily exposes each piece separately.

use automerge::ObjId;
use taffy::{NodeId, TaffyTree};
use winit::event::{Modifiers, WindowEvent};

pub(crate) struct InputContext<'a> {
    pub(crate) event: &'a WindowEvent,
    pub(crate) scale_factor: f64,
    pub(crate) modifiers: Modifiers,
    pub(crate) state: ui::widget::Edits<'a>,
    pub(crate) path: &'a ObjId,
    pub(crate) handled: &'a mut bool,
    pub(crate) focus_target: &'a mut Option<NodeId>,
    pub(crate) node_id: NodeId,
    pub(crate) taffy: &'a TaffyTree<()>,
}

impl InputContext<'_> {
    pub(crate) fn event_ctx(&mut self) -> ui::widget::EventCtx<'_> {
        self.event_ctx_at(self.node_id)
    }

    pub(crate) fn event_ctx_at(&mut self, node_id: NodeId) -> ui::widget::EventCtx<'_> {
        ui::widget::EventCtx {
            scale_factor: self.scale_factor,
            modifiers: &self.modifiers,
            state: self.state.reborrow(),
            path: self.path,
            handled: self.handled,
            focus_target: self.focus_target,
            node_id,
            taffy: self.taffy,
        }
    }
}
