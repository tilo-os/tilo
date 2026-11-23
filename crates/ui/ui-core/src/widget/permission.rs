//! A widget's place in the workspace's permission authority: the scope its
//! input and actions answer to. The authority itself and what a scope means
//! are the collaboration model's; the toolkit only publishes the scope a
//! control is guarded by, so the host can ask and paint a denial.

use super::Widget;
use super::capability::capability;

pub use ::collaboration::permissions::PermissionScope;

/// A widget whose input and actions are guarded by a permission scope.
pub trait PermissionGuarded: Widget {
    fn permission_scope(&self) -> Option<PermissionScope> {
        None
    }
}

/// The scope `widget` is guarded by, when it names one.
pub fn permission_scope(widget: &(impl Widget + ?Sized)) -> Option<PermissionScope> {
    capability::<dyn PermissionGuarded>(widget)?.permission_scope()
}
