//! Presentation state (scroll offset, view mode) a root contributes to the
//! App's [`View`](super::View).

use crate::widget::Widget;
use crate::widget::capability::{capability, capability_mut};

/// A root with restorable presentation state.
pub trait ViewState: Widget {
    /// This root's contribution, restored through [`Self::restore_view_state`].
    fn view_state(&self) -> Option<super::ViewContribution> {
        None
    }

    fn restore_view_state(&mut self, _view: &super::ViewContribution) {}
}

pub fn view_state(widget: &(impl Widget + ?Sized)) -> Option<super::ViewContribution> {
    capability::<dyn ViewState>(widget)?.view_state()
}

pub fn restore_view_state(widget: &mut (impl Widget + ?Sized), view: &super::ViewContribution) {
    if let Some(state) = capability_mut::<dyn ViewState>(widget) {
        state.restore_view_state(view);
    }
}
