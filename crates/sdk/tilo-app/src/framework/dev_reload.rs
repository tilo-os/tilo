//! The symbols a `thin` app library exports so `tilo-app dev` can run new
//! code inside the windows the process already has.

use tilo_resource::{Doc, Reader, Resource};
use ui_core::permissions::{LivePermissions, Role};
use ui_core::widget::AppWidget;

use super::app::{Editing, ResourceApp, action_registry, build_root, role_permissions};

/// Address of the `ui` this library linked. The host compares it with its own
/// and refuses the library when they differ.
pub fn linked_ui() -> *const () {
    ui_core::app::dev_stamp_addr()
}

/// Build `A`'s current view and hand it, with the decoders of the edits it
/// makes, to the host's `ui`.
///
/// Returns `1` when the view was published. The document, route and caret
/// already in the open windows are restored by the host after it installs
/// this view. The decoders must come from this library too: the view encodes
/// its edits with this code, which the running code may not read.
pub fn publish_root<A: ResourceApp>() -> i32 {
    let mut doc = Doc::new();
    if <A::Resource as Resource>::initialize(&mut doc).is_err() {
        return 0;
    }
    let Ok(reader) = Reader::<A::Resource>::read(&doc) else {
        return 0;
    };
    let permissions = LivePermissions::fixed(role_permissions("dev", Role::Owner).borrow().clone());
    let me = ui_core::collaboration::local_user_id().0;
    let root = build_root::<A>(reader, Editing::Live { permissions, me });
    let view: Box<dyn AppWidget> = Box::new(root);
    ui_core::app::publish_dev_root(ui_core::app::DevRoot {
        view,
        registry: action_registry::<A>(),
    });
    1
}

/// Copy one window's state back to the host.
///
/// # Safety
/// `state` points at `state_len` readable bytes. `out` points at `out_cap`
/// writable bytes. Both live for this call.
pub unsafe fn copy_window_state(
    state: *const u8,
    state_len: usize,
    out: *mut u8,
    out_cap: usize,
) -> isize {
    if state_len > out_cap || (state_len > 0 && (state.is_null() || out.is_null())) {
        return -1;
    }
    if state_len > 0 {
        // SAFETY: the caller upholds the pointer contract above.
        unsafe { std::ptr::copy_nonoverlapping(state, out, state_len) };
    }
    isize::try_from(state_len).unwrap_or(-1)
}
