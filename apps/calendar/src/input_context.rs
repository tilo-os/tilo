//! Shared input capability passed through Calendar's internal components.
//!
//! The public UI widget contract necessarily exposes each piece separately.
//! Calendar's nested controllers receive them as one value so their APIs can
//! focus on domain-specific inputs rather than repeating framework plumbing.

use automerge::{AutoCommit, ObjId};
use taffy::{NodeId, TaffyTree};
use winit::event::{Modifiers, WindowEvent};

pub(crate) struct InputContext<'a> {
    pub(crate) event: &'a WindowEvent,
    pub(crate) scale_factor: f64,
    pub(crate) modifiers: Modifiers,
    pub(crate) state: &'a mut AutoCommit,
    pub(crate) path: &'a ObjId,
    pub(crate) handled: &'a mut bool,
    pub(crate) focus_target: &'a mut Option<NodeId>,
    pub(crate) node_id: NodeId,
    pub(crate) taffy: &'a TaffyTree<()>,
}
