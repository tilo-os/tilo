//! A setup prepared on another computer arrives only where Tilo is the
//! operating system (prepared.rs): elsewhere none is ever pending, so first use
//! goes the ordinary way.
use super::OnboardingView;
use std::path::Path;

impl OnboardingView {
    pub(super) fn load_prepared_setup(&mut self, _identity_dir: &Path) {}

    #[allow(clippy::unused_self, reason = "the same call as on Linux")]
    pub(super) fn prepared_working(&self) -> bool {
        false
    }

    pub fn prepared_setup_status(&self) -> Option<&'static str> {
        self.prepared_pending.then_some("unavailable")
    }

    pub fn submit_prepared_setup_secret(&mut self, _password: &[u8]) -> Result<(), ()> {
        Err(())
    }

    #[allow(clippy::unused_self, reason = "the same call as on Linux")]
    pub fn verified_browser_projection(&self) -> Option<&Path> {
        None
    }

    pub(super) fn sync_prepared(&mut self) {}
}
