//! Which windows may belong to a downloaded program: the compositor's side of
//! `shell::downloaded_apps`, which does the looking on its own thread.

use smithay::reexports::wayland_server::Resource as _;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;

use crate::compositor::state::ClientState;
use crate::shell::downloaded_apps::ObservedWindow;

/// Hand a window that just named itself to the downloaded-program worker
/// when it belongs to an unconfined host process Tilo did not start: a
/// program run from the person's terminal. A package, a host app Tilo
/// launched, a legacy guest or a privileged tool is never a download.
pub fn observe(surface: &WlSurface, surface_id: u64, app_id: &str) {
    let Some(client) = surface.client() else {
        return;
    };
    let Some(state) = client.get_data::<ClientState>() else {
        return;
    };
    if state.privileged
        || state.package_id.is_some()
        || state.launch_custody.is_some()
        || state.legacy_guest.is_some()
        || state.input_method.is_some()
    {
        return;
    }
    let Some(pid) = state.pid else {
        return;
    };
    // An X11 program's windows all come from satellite's connection: the
    // X11 side finds the program behind each, and its icon.
    crate::compositor::x11_windows::observe_window(ObservedWindow {
        surface_id,
        pid,
        pid_start_time: state.pid_start_time,
        app_id: app_id.to_owned(),
    });
}
