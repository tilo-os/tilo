//! What this session lends setup ([`tilo_onboarding_host::host`]), and the
//! catalogue setup is driven through from its first frame.

use std::path::PathBuf;
use std::rc::Rc;
use std::task::Waker;

struct Session;

impl tilo_onboarding_host::host::SessionHost for Session {
    fn loop_waker(&self) -> Waker {
        crate::shell::search::invocation_wake::waker()
    }

    fn browser_continuity(&self, projection: Option<PathBuf>) {
        crate::shell::launcher::set_browser_continuity(projection);
    }

    fn wake(&self) {
        crate::shell::wake_compositor();
    }

    fn person(&self) -> String {
        crate::shell::state::local_user_id()
    }
}

/// Lend setup this session. Startup calls it once, before setup runs.
pub fn install() {
    tilo_onboarding_host::host::install(&Session);
}

/// The tab setup's actions are listed under: past every tab id, so setup
/// never names a window.
pub(crate) const SETUP_TAB: u64 = u64::MAX - 0x0005_e70b;

/// Setup's actions in this device's catalogue, as the `onboarding`
/// catalogue the first-use control phase admits: what the setup app
/// publishes, for the person who owns this desktop while it is unlocked.
pub(crate) fn setup_actions(
    view: &crate::onboarding_view::OnboardingView,
) -> Rc<dyn crate::shell::search::SearchProvider> {
    Rc::new(
        crate::shell::device_local_actions::DeviceLocalActionsProvider::for_door(
            "onboarding",
            "Identity setup",
            crate::onboarding::intro_title,
            SETUP_TAB,
            view.device_local_door(),
        ),
    )
}
