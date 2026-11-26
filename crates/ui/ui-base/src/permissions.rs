//! Who may do what in a workspace: the permission context lives with the collaboration model
//! (`collaboration::permissions`), where a server checks it without the toolkit; the toolkit adds
//! the capability widgets and wakes its loops when an outside authority changes.
//!
//! The toolkit itself asks one question of whatever authority a drawing answers to, through
//! [`WidgetAuthority`]: may the person act on what this widget guards? A window drawn under no
//! authority, such as any window of a toolkit built without Tilo's model, may act on everything.

/// Who a member is, and what kind of action a grant reaches: plain values of the action policy,
/// which the toolkit names without the collaboration model.
pub use tilo_action_policy::{Acting, ActionKind, Grantee, MemberId};

#[cfg(feature = "model")]
pub use ::collaboration::permissions::*;

/// The authority a drawing answers to, as the toolkit asks it: whether the person the drawing is
/// for may act on what `widget` guards. A widget the authority refuses is drawn behind the
/// denied-permission scrim. The workspace's permission context answers from the scope the
/// widget is guarded by; where no authority is lent, everything is allowed.
pub trait WidgetAuthority {
    fn permits_widget(&self, widget: &dyn crate::widget::Widget) -> bool;
}

#[cfg(feature = "model")]
impl WidgetAuthority for PermissionContext {
    fn permits_widget(&self, widget: &dyn crate::widget::Widget) -> bool {
        crate::widget::permission::permission_scope(widget).is_none_or(|scope| self.check(&scope))
    }
}

/// Wake `wake` whenever an outside authority permissions follow changes, for as long as its scope
/// is open.
#[cfg(all(feature = "native", feature = "model"))]
pub(crate) fn wake_loop_on_outside_change(wake: crate::wake::WakeHandle) {
    ::collaboration::permissions::wake_on_outside_change(move || {
        if !wake.is_active() {
            return false;
        }
        wake.wake();
        true
    });
}
