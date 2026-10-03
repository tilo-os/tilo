//! Widgets gated by a permission scope.

use crate::widget::Widget;
use crate::widget::capability::capability;

/// A widget whose input and actions are gated by a permission scope.
pub trait PermissionGated: Widget {
    /// The scope gating this widget.
    fn permission_scope(&self) -> Option<super::PermissionScope> {
        None
    }
}

/// The scope gating `widget`, if any.
pub fn permission_scope(widget: &(impl Widget + ?Sized)) -> Option<super::PermissionScope> {
    capability::<dyn PermissionGated>(widget)?.permission_scope()
}
