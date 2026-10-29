//! What this session lends setup ([`tilo_onboarding_host::host`]).

use std::path::PathBuf;
use std::task::Waker;

struct Session;

impl tilo_onboarding_host::host::SessionHost for Session {
    fn loop_waker(&self) -> Waker {
        crate::shell::search::invocation_wake::waker()
    }

    fn browser_continuity(&self, projection: Option<PathBuf>) {
        crate::shell::launcher::set_browser_continuity(projection);
    }
}

/// Lend setup this session. Startup calls it once, before setup runs.
pub fn install() {
    tilo_onboarding_host::host::install(&Session);
}
