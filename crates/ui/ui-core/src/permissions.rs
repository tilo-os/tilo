//! Who may do what in a workspace: the permission context lives with the collaboration model
//! (`collaboration::permissions`), where a server checks it without the toolkit; the toolkit adds
//! the capability widgets and wakes its loops when an outside authority changes.

pub use ::collaboration::permissions::*;

/// Wake `wake` whenever an outside authority permissions follow changes, for as long as its scope
/// is open.
#[cfg(feature = "native")]
pub(crate) fn wake_loop_on_outside_change(wake: crate::wake::WakeHandle) {
    ::collaboration::permissions::wake_on_outside_change(move || {
        if !wake.is_active() {
            return false;
        }
        wake.wake();
        true
    });
}
