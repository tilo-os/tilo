//! What the person at a window may do: the device's verdicts as the window keeps them live with
//! `tilo_permissions`; the toolkit wakes its loops when they
//! change from outside.

/// Who a member is, and what kind of action a grant reaches: plain values of the action model,
/// which the toolkit names without the collaboration model.
pub use tilo_action_types::ActionKind;
pub use tilo_member_types::{Acting, MemberId};

#[cfg(feature = "model")]
pub use tilo_permissions::*;

/// Wake `wake` whenever something outside the verdicts follow changes, for as long as its scope
/// is open.
#[cfg(all(feature = "native", feature = "model"))]
pub fn wake_loop_on_outside_change(wake: crate::wake::WakeHandle) {
    tilo_permissions::wake_on_outside_change(move || {
        if !wake.is_active() {
            return false;
        }
        wake.wake();
        true
    });
}
