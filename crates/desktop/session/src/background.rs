//! The desktop under the tilolang shell: the session's own desktop, which it runs each frame, and
//! the policy that places the windows in the wells the shell draws; and the apps that run, as the
//! portal asks of them.

use std::cell::RefCell;
use std::rc::Rc;

use tilo_session_core::desktop::host::contract::DesktopAsk;

use crate::Options;
use crate::platform::surfaces::{ShellDesktop, WellPolicy, desktop};

/// The apps that run now, in the states the portal asks of them: the app in front is in use, the
/// ones behind it only run.
pub(crate) fn running_apps(compositor: &tilo_compositor::Compositor) -> Vec<(String, u32)> {
    use tilo_portal::background::{ACTIVE, RUNNING};
    let focused = compositor.focused_window().and_then(|window| window.app_id);
    let mut apps: std::collections::BTreeMap<String, u32> = std::collections::BTreeMap::new();
    for (window, _) in compositor.windows() {
        let Some(app_id) = window.app_id else {
            continue;
        };
        let state = if focused.as_deref() == Some(app_id.as_str()) {
            ACTIVE
        } else {
            RUNNING
        };
        // An app showing more than one window is as in use as its best.
        let state = apps
            .get(&app_id)
            .copied()
            .map_or(state, |seen| seen.max(state));
        apps.insert(app_id, state);
    }
    apps.into_iter().collect()
}

/// The shell of this session's workspace and the policy placing its windows in the wells the
/// tilolang shell draws, with what the desktop asks of the session. The session runs the desktop
/// each frame; no widget of it is built.
pub(crate) fn background(
    options: &Options,
    activity: &crate::activity::Activity,
    notices: tilo_session_core::desktop::state::DesktopNotifications,
) -> (ShellDesktop, WellPolicy, Rc<RefCell<Vec<DesktopAsk>>>) {
    let asks = Rc::new(RefCell::new(Vec::new()));
    let desktop = desktop(options.salt);
    desktop.with_policy(|desktop| {
        // Other devices' shared windows are read through the desktop: their pictures land on it.
        crate::window_share::peer::install(desktop.handle());
        // The shell's windows ask it for the workspace's activity, and the apps running beside it
        // ask the same model through the broker.
        let pass = desktop.pass_mut();
        pass.set_history_source(Box::new(activity.clone()));
        pass.serve_activity(activity.requests());
        pass.set_notifications(notices);
        // A tab opens with the name of its app in the person's language, never with the app's id.
        desktop.reads_app_names(Rc::new(crate::app_names::app_name));
    });
    let lists = options.lists.clone();
    desktop.run_desktop(tilo_session_core::desktop::shell_verbs::ShellVerbs::new(
        desktop.with_policy(|desktop| desktop.handle().clone()),
        Rc::clone(&asks),
        Rc::new(move || crate::host_actions::cards(&lists)),
    ));
    let policy = desktop.well_policy();
    (desktop, policy, asks)
}
